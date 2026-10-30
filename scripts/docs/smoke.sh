#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
# The tutorials' commands, run headless: build the binaries, round-trip an
# envelope and refute a tampered one (T1), run a daemon on private
# endpoints, publish and tap (T2), run the two-process example (T3), then
# the embedded-ring and UDP-loopback examples (T5, T6). Part of the
# documentation gate (`just docs`). Each part runs where the tree holds
# what it needs: `pulse` for T1; the daemon, `pms` and the two-process
# example for T2 and T3; the examples for T5 and T6. The integrated tree
# holds all of them, a repository cut from it some, and the last line
# names the parts that ran.
# Outputs go to files before they are grepped: a `grep -q` that closes its
# pipe early would make the binary die on SIGPIPE.
#
# Every run has endpoints of its own, so runs can share a machine: the
# socket lives in the run's temporary directory, the WebSocket listens on a
# port the kernel picks (the daemon logs which), and the hub name is
# `smoke-` plus that directory's random suffix, 14 of the 15 characters a
# hub name may have, which keeps its shared-memory names within 30 bytes.
# A step that needs another process ready waits for the line that process
# prints when it is (for the tap and the receiver, the line the tutorials
# show), not for a fixed time.
set -euo pipefail
cd "$(dirname "$0")/../.."
TMP="${TMPDIR:-/tmp}"
WORK="$(mktemp -d "${TMP%/}/pms-smoke.XXXXXXXX")"; SOCK="$WORK/pms.sock"
HUB="smoke-${WORK##*.}"
PIDS=()
# The daemon removes its socket and unlinks its shared memory as it exits,
# so each process gets up to five seconds to go before the directory does.
cleanup() {
  local p i
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  for p in "${PIDS[@]:-}"; do
    i=0; while [ -n "$p" ] && [ "$i" -lt 50 ] && kill -0 "$p" 2>/dev/null; do sleep 0.1; i=$((i + 1)); done
  done
  rm -rf "$WORK"
}
trap cleanup EXIT
# wait_for <file> <text> <tenths of a second>: until the file holds the text.
wait_for() {
  local i=0
  while [ "$i" -lt "$3" ]; do grep -qF -- "$2" "$1" 2>/dev/null && return 0; sleep 0.1; i=$((i + 1)); done
  return 1
}
# has <crate>: whether the workspace holds the crate as a member (not as a
# dependency: the daemon depends on the standard's tool without holding it).
members=$(cargo metadata --no-deps --format-version 1 |
  python3 -c 'import json, sys; print(" ".join(p["name"] for p in json.load(sys.stdin)["packages"]))')
has() { case " $members " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }
build=()
for crate in pulse-cli pms-cli mesh-hub multi-process example-02-loopback-udp-rust example-03-embed-ring-rust; do
  if has "$crate"; then build+=(-p "$crate"); fi
done
# (an empty array is unbound to bash 3.2 under set -u, hence the guard)
if [ "${#build[@]}" -gt 0 ]; then cargo build -q --release "${build[@]}"; fi
BIN="${CARGO_TARGET_DIR:-target}/release"
PULSE="$BIN/pulse"
PMS="$BIN/pms"
ran=""

if has pulse-cli; then
echo "== T1: encode, decode, verify, refute"
cat > "$WORK/spec.json" <<'JSON'
{ "envelope_type": 1, "payload_utf8": "hello, mesh" }
JSON
"$PULSE" encode --json "$WORK/spec.json" | tee "$WORK/encode.txt"
HEX="$(sed -n 's/^canonical_hex *//p' "$WORK/encode.txt")"
"$PULSE" decode "$HEX" | tee "$WORK/decode.txt"
grep -q 'decoded_equal   true' "$WORK/encode.txt"
grep -q 'payload          Raw, 11 byte(s)' "$WORK/decode.txt"
grep -q 'verified         true' "$WORK/decode.txt"
if "$PULSE" decode "${HEX%68}69" > "$WORK/tampered.txt"; then
  echo "pulse decode exited 0 on an envelope that does not verify"; exit 1
fi
grep -q 'verified         false' "$WORK/tampered.txt"
ran="$ran T1"
fi

if has mesh-hub && has pms-cli; then
echo "== T2: a daemon on private endpoints, publish and tap"
"$BIN/mesh-hub" --hub-name "$HUB" --uds-path "$SOCK" --ws-bind 127.0.0.1:0 > "$WORK/hub.log" 2>&1 &
PIDS+=("$!"); disown
# The daemon logs the WebSocket's address once its hub exists, after it has
# claimed the socket.
wait_for "$WORK/hub.log" 'websocket listening' 100 || { echo "daemon did not start"; cat "$WORK/hub.log"; exit 1; }
[ -S "$SOCK" ] || { echo "daemon did not bind $SOCK"; cat "$WORK/hub.log"; exit 1; }
WS="$(sed -n 's/.*websocket listening.*\(127\.0\.0\.1:[0-9][0-9]*\).*/\1/p' "$WORK/hub.log")"
[ -n "$WS" ] || { echo "daemon did not report its WebSocket address"; cat "$WORK/hub.log"; exit 1; }
echo "daemon: hub $HUB, socket $SOCK, websocket $WS"
export PMS_DAEMON="unix://$SOCK"
"$PMS" tap --channel 'jobs/*' > "$WORK/tap.txt" 2> "$WORK/tap.err" &
PIDS+=("$!"); disown
wait_for "$WORK/tap.err" 'tapped; Ctrl-C to stop' 100 || { echo "pms tap did not subscribe"; cat "$WORK/tap.err"; exit 1; }
"$PMS" push jobs/new 'first job' | tee "$WORK/push.txt"
grep -q '^envelope_id=[0-9a-f]\{32\}$' "$WORK/push.txt"
wait_for "$WORK/tap.txt" 'jobs/new' 100 || true
grep 'jobs/new' "$WORK/tap.txt"
"$PMS" hello > "$WORK/hello.txt" 2>&1
grep -q 'got: hello from pms' "$WORK/hello.txt"
"$PMS" ring inspect "$HUB" --count 1 > "$WORK/ring.txt"
grep -q "^ring *\/pms-$HUB\$" "$WORK/ring.txt"
ran="$ran T2"

if has multi-process; then
echo "== T3: two processes, one ring"
"$BIN/receiver" > "$WORK/receiver.txt" 2>&1 &
PIDS+=("$!"); disown
# The receiver prints its line once the daemon has acknowledged its
# subscription, so a job the sender publishes after it is delivered.
wait_for "$WORK/receiver.txt" "receiver: attached and subscribed to 'jobs'" 100 || { echo "receiver did not attach"; cat "$WORK/receiver.txt"; exit 1; }
"$BIN/sender" > "$WORK/sender.txt" 2>&1 &
PIDS+=("$!"); disown
wait_for "$WORK/receiver.txt" "receiver: got 'job-2'" 150 || true
grep "receiver: got 'job-0'" "$WORK/receiver.txt"
grep -q "receiver: got 'job-2'" "$WORK/receiver.txt"
ran="$ran T3"
fi
fi

if has example-03-embed-ring-rust; then
echo "== T5: the ring embedded"
"$BIN/embed-ring" | tee "$WORK/embed.txt"
grep -q '^OK' "$WORK/embed.txt"
ran="$ran T5"
fi
if has example-02-loopback-udp-rust; then
echo "== T6: UDP loopback"
"$BIN/loopback-udp" | tee "$WORK/loop.txt"
grep -q '^OK' "$WORK/loop.txt"
ran="$ran T6"
fi
echo "docs smoke: ok (${ran# })"
