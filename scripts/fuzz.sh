#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# Run the fuzz harness as its manifest documents it, from the repository
# root: cargo +nightly fuzz run <target> --fuzz-dir peripheral/fuzz.
#
#   scripts/fuzz.sh                      every target, 60 seconds each
#   scripts/fuzz.sh <target> [seconds]   one target
#   scripts/fuzz.sh all <seconds>        every target, that long each
#
# The targets are the [[bin]] entries of peripheral/fuzz/Cargo.toml, as
# `cargo fuzz list` reads them, run one after another. Sixty seconds is
# the default: on an arm64 Mac it gives a target from about 150 thousand
# executions (fuzz_fec_decoder, the slowest) to 30 million
# (fuzz_wire_decode), enough to get past the shallow paths a change
# breaks first, and a sweep of every target stays near a quarter of an
# hour, short enough to run before a review. A search for deeper bugs
# gives one target hours. Each target's corpus grows in
# peripheral/fuzz/corpus/<target>, where the next run starts, and each
# run's output goes to target/fuzz/<target>.log.
#
# The first target that fails stops the run: the script prints the end of
# its log, names the input libFuzzer saved in
# peripheral/fuzz/artifacts/<target>/ (a crash, a timeout or a memory
# blow-up) with the command that replays it, and exits 1. Needs the
# nightly toolchain and cargo-fuzz.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
# The harness: under peripheral/ in the integrated tree, at the top level in
# a repository cut from it.
FUZZ_DIR=fuzz
[ -f peripheral/fuzz/Cargo.toml ] && FUZZ_DIR=peripheral/fuzz
LOGS=target/fuzz

usage() { echo "usage: scripts/fuzz.sh [target | all] [seconds]" >&2; exit 2; }
is_count() { case $1 in '' | *[!0-9]*) return 1 ;; *) [ "$1" -gt 0 ] ;; esac; }
target=all
seconds=60
case $# in
  0) ;;
  1) target=$1 ;;
  2) target=$1; seconds=$2 ;;
  *) usage ;;
esac
case $target in
  -h | --help) awk 'NR > 4 && /^#/ { sub(/^# ?/, ""); print; next } NR > 4 { exit }' "$0"; exit 0 ;;
esac
is_count "$seconds" || usage

missing() { echo "fuzz: $1" >&2; exit 127; }
cargo +nightly --version >/dev/null 2>&1 ||
  missing "the nightly toolchain is not installed: rustup toolchain install nightly"
command -v cargo-fuzz >/dev/null 2>&1 || missing "cargo-fuzz is not installed: cargo install cargo-fuzz --locked"

known=$(cargo +nightly fuzz list --fuzz-dir "$FUZZ_DIR") || exit 1
if [ "$target" = all ]; then
  targets=$known
elif printf '%s\n' "$known" | grep -qxF -- "$target"; then
  targets=$target
else
  echo "fuzz: no target named $target; the targets are:" >&2
  printf '  %s\n' $known >&2
  exit 2
fi
count=$(printf '%s\n' "$targets" | wc -l | tr -d ' ')

# The inputs libFuzzer has saved for a target, one path per line.
artifacts() {
  local f
  for f in "$FUZZ_DIR/artifacts/$1"/*; do [ -e "$f" ] && printf '%s\n' "$f"; done
  return 0
}

mkdir -p "$LOGS"
trap 'echo "fuzz: interrupted" >&2; exit 130' INT
echo "fuzz: $count target(s), ${seconds}s each; logs in $LOGS/"
for t in $targets; do
  log="$LOGS/$t.log"
  before=$(artifacts "$t")
  start=$SECONDS
  rc=0
  cargo +nightly fuzz run "$t" --fuzz-dir "$FUZZ_DIR" -- -max_total_time="$seconds" \
    >"$log" 2>&1 </dev/null || rc=$?
  took=$((SECONDS - start))
  if [ "$rc" -eq 0 ]; then
    runs=$(sed -n 's/^Done \([0-9]*\) runs in .*/\1/p' "$log" | tail -n 1)
    printf 'ok    %-32s %5ss  %s runs\n' "$t" "$took" "${runs:-?}"
    continue
  fi
  printf 'FAIL  %-32s %5ss  exit %s\n' "$t" "$took" "$rc"
  tail -n 30 "$log" | sed 's/^/  | /'
  saved=""
  for f in $(artifacts "$t"); do
    case "
$before
" in
      *"
$f
"*) ;;
      *) saved="$saved $f" ;;
    esac
  done
  if [ -z "$saved" ]; then
    echo "fuzz: $t failed without saving an input; the log is $log"
  fi
  for f in $saved; do
    echo "fuzz: $t saved $f"
    echo "fuzz: replay it with: cargo +nightly fuzz run $t --fuzz-dir $FUZZ_DIR $f"
  done
  exit 1
done
echo "fuzz: $count target(s) ran ${seconds}s each; nothing failed"
