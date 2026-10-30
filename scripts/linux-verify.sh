#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# Run a cargo command on Linux, in a Docker container, against this
# worktree's files as they stand: uncommitted and untracked files are
# included, ignored ones are not.
#
#   scripts/linux-verify.sh [cargo args...]      default: test --workspace
#   SHM_SIZE=64m scripts/linux-verify.sh test --workspace
#   LINUX_VERIFY_NET=1 scripts/linux-verify.sh test -p pulse-wire-xdp
#
# On a Mac this is where the Linux-only code (NUMA placement, io_uring,
# /dev/shm sizing) compiles and runs. `scripts/gate.sh --full` runs the
# workspace tests with a 1 GB and with a 64 MB /dev/shm and the
# all-features clippy through it.
#
# The cache is $HOME/.cache/pulse-mesh-linux-verify (LINUX_VERIFY_CACHE
# moves it):
#   registry/      the cargo registry, shared by every worktree
#   <key>/src      the copy of the worktree the container builds
#   <key>/target   the container's target directory
# <key> is the worktree's directory name and a checksum of its path, so every
# worktree has a copy and a target of its own (LINUX_VERIFY_KEY names
# another), and runs on one key take turns. The copy lives under the home
# directory because colima and Docker Desktop share only that with their
# Linux VM. rsync --checksum without -t copies a file only when its
# content changed, and the copy gets the current time, so cargo rebuilds
# exactly what changed; a preserved older timestamp could leave a stale
# binary in the target cache. The copy leaves out what .gitignore lists.
#
# The container:
#   image      rust:<channel>-bookworm, where <channel> is the toolchain
#              rust-toolchain.toml pins, so Linux builds with the host's
#              compiler (LINUX_VERIFY_IMAGE names another image). The
#              image has no clippy or rustfmt: rustup adds the components
#              the toolchain file lists in each new container, over the
#              network.
#   --shm-size SHM_SIZE, default 1g, what /dev/shm offers on a normal
#              host. 64m reproduces Docker's default; 8m is small enough
#              for core-region's numa_no_space test to check its case.
#   --security-opt seccomp=unconfined --cap-add SYS_NICE
#              Docker's default seccomp profile refuses mbind to a
#              container without CAP_SYS_NICE, and io_uring to every
#              container, so without these hft-stages' NUMA
#              bind-and-detect test prints SKIP and robotics-stages'
#              io_uring test skips too. Either flag alone lets mbind
#              through; io_uring needs the profile off.
#   --init     a PID 1 that passes Ctrl-C on to cargo and reaps the
#              processes tests leave behind.
#   LINUX_VERIFY_NET=1
#              for the tests that build their own network: veth pairs,
#              namespaces, XDP programs and AF_XDP sockets
#              (pulse-wire-xdp). The container runs --privileged, so its
#              root holds CAP_NET_ADMIN, CAP_BPF and CAP_SYS_ADMIN in a
#              network namespace of its own, and from an image built once
#              from the usual one with iproute2 added
#              (pulse-mesh-lv-net:<channel>), since the tests create the
#              links with `ip`. Without it those tests find no privilege
#              and skip.
#   --name     pulse-mesh-lv-<key>-<pid>, removed when the run ends.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$(pwd)
[ $# -gt 0 ] || set -- test --workspace

missing() { echo "linux-verify: $1" >&2; exit 127; }
command -v docker >/dev/null 2>&1 ||
  missing "docker is not installed: install Docker (on macOS: brew install colima docker, then colima start)"
docker info >/dev/null 2>&1 || missing "docker does not answer: start its daemon (on macOS: colima start)"
command -v rsync >/dev/null 2>&1 || missing "rsync is not installed: install rsync"

CACHE="${LINUX_VERIFY_CACHE:-$HOME/.cache/pulse-mesh-linux-verify}"
KEY="${LINUX_VERIFY_KEY:-$(basename "$ROOT")-$(printf '%s' "$ROOT" | cksum | cut -d ' ' -f 1)}"
KEY=$(printf '%s' "$KEY" | tr -c 'A-Za-z0-9_.-' '-')
CHANNEL=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
IMAGE="${LINUX_VERIFY_IMAGE:-rust:$CHANNEL-bookworm}"
PRIVILEGE=()
if [ "${LINUX_VERIFY_NET:-0}" = 1 ]; then
  NET_IMAGE="pulse-mesh-lv-net:$CHANNEL"
  if ! docker image inspect "$NET_IMAGE" >/dev/null 2>&1; then
    echo "linux-verify: building $NET_IMAGE from $IMAGE, once" >&2
    printf 'FROM %s\nRUN apt-get update && apt-get install -y --no-install-recommends iproute2 && rm -rf /var/lib/apt/lists/*\n' "$IMAGE" |
      docker build -q -t "$NET_IMAGE" - >/dev/null
  fi
  IMAGE=$NET_IMAGE
  PRIVILEGE=(--privileged)
fi
SHM="${SHM_SIZE:-1g}"
BASE="$CACHE/$KEY"
NAME="pulse-mesh-lv-$KEY-$$"
mkdir -p "$BASE/src" "$BASE/target" "$CACHE/registry"

# A second run on the key waits; a lock whose run has died is taken over.
LOCK="$BASE/lock"
waiting=""
until mkdir "$LOCK" 2>/dev/null; do
  holder=$(cat "$LOCK/pid" 2>/dev/null || true)
  if [ -n "$holder" ] && ! kill -0 "$holder" 2>/dev/null; then
    rm -rf "$LOCK"
    continue
  fi
  [ -n "$waiting" ] || echo "linux-verify: waiting for run ${holder:-?} on $KEY" >&2
  waiting=1
  sleep 2
done
echo "$$" >"$LOCK/pid"
cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  rm -rf "$LOCK"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# A repository cut from the integrated tree depends on the standard as a
# sibling checkout, ../pulse-mesh, which its manifests name; the sibling is
# then copied beside the tree, both under /src, and the container works in
# the tree's own directory. The integrated tree, and the standard itself,
# name no sibling and are copied alone, as before.
SIBLING=""
WORK=/src
if [ "$(basename "$ROOT")" != pulse-mesh ] && [ -f "$ROOT/../pulse-mesh/Cargo.toml" ] &&
  grep -rqs --include=Cargo.toml -- '\.\./pulse-mesh/' "$ROOT" 2>/dev/null; then
  SIBLING=$(cd "$ROOT/../pulse-mesh" && pwd)
  WORK="/src/$(basename "$ROOT")"
fi
sync_tree() {
  rsync -rl --checksum --delete --delete-excluded --filter=':- .gitignore' \
    --exclude .git --exclude /peripheral/fuzz/coverage/ --exclude /fuzz/coverage/ "$1"/ "$2/"
}
if [ -n "$SIBLING" ]; then
  mkdir -p "$BASE/src/$(basename "$ROOT")" "$BASE/src/pulse-mesh"
  sync_tree "$ROOT" "$BASE/src/$(basename "$ROOT")"
  sync_tree "$SIBLING" "$BASE/src/pulse-mesh"
  echo "linux-verify: $IMAGE, /dev/shm $SHM, $BASE, with the standard from $SIBLING beside it: cargo $*" >&2
else
  sync_tree "$ROOT" "$BASE/src"
  echo "linux-verify: $IMAGE, /dev/shm $SHM, $BASE: cargo $*" >&2
fi
rc=0
docker run --rm --init --name "$NAME" --shm-size="$SHM" \
  --security-opt seccomp=unconfined --cap-add SYS_NICE ${PRIVILEGE[@]+"${PRIVILEGE[@]}"} \
  -v "$BASE/src":/src -v "$BASE/target":/target -v "$CACHE/registry":/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/target -w "$WORK" "$IMAGE" cargo "$@" || rc=$?
exit "$rc"
