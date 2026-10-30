#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# What the packaging scripts beside this file share. Each of them sources
# it; it is not run on its own, and like them it publishes nothing.
#
# Once sourced, the current directory is the tree's root, and:
#   ROOT       that root
#   BINDINGS   the bindings' directory: peripheral/bindings in the
#              integrated tree, bindings in the repository cut from it
#   OUT        target/package, where the artifacts land
#   STAGE      target/package/stage, the directories they are made from
#   CHANNEL    the toolchain rust-toolchain.toml pins, which the Linux
#              containers build with
#
# The Linux artifacts are built and smoke-tested in Docker. colima and
# Docker Desktop share only the home directory with their Linux VM, so a
# container cannot see a tree under /tmp; as scripts/linux-verify.sh does,
# the tree is copied (rsync, leaving out what .gitignore lists) to a cache
# under the home directory, $HOME/.cache/pulse-mesh-package
# (PACKAGE_CACHE moves it):
#   registry/            the cargo registry, shared by every container
#   <key>/<job>/src      the copy of the tree one script's containers use
#   <key>/<job>/...      that script's target directories and output
# <key> is the tree's directory name and a checksum of its path, so each
# worktree has its own; <job> is wheels, node or smoke, so the three
# scripts can run at once. Two runs of one script on one tree cannot.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
ROOT=$(pwd)
OUT="$ROOT/target/package"
STAGE="$OUT/stage"
CHANNEL=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
BINDINGS=""
for d in peripheral/bindings bindings; do [ -d "$d/python" ] && BINDINGS=$d && break; done
[ -n "$BINDINGS" ] || { echo "package: this tree holds no bindings" >&2; exit 1; }
mkdir -p "$OUT"

CACHE="${PACKAGE_CACHE:-$HOME/.cache/pulse-mesh-package}"
KEY=$(printf '%s' "$(basename "$ROOT")-$(printf '%s' "$ROOT" | cksum | cut -d ' ' -f 1)" | tr -c 'A-Za-z0-9_.-' '-')

# The images the Linux legs start from (PACKAGE_MANYLINUX_IMAGE,
# PACKAGE_RUST_IMAGE and PACKAGE_NODE_IMAGE name others; manylinux's
# takes the architecture as a suffix):
#   quay.io/pypa/manylinux_2_28_<arch>   the wheels: CPython 3.10 to 3.13
#        under /opt/python on glibc 2.28, so a wheel built there runs on
#        any Linux with glibc 2.28 or later; the build adds rustup
#   rust:<channel>-bookworm              the Node modules, with Node
#        copied in from the Node image
#   node:20-bookworm-slim                that Node, and the Node
#        package's smoke runs, in an image with no Rust in it
MANYLINUX_IMAGE="${PACKAGE_MANYLINUX_IMAGE:-quay.io/pypa/manylinux_2_28}"
RUST_IMAGE="${PACKAGE_RUST_IMAGE:-rust:$CHANNEL-bookworm}"
NODE_IMAGE="${PACKAGE_NODE_IMAGE:-node:20-bookworm-slim}"

die() { echo "package: $*" >&2; exit 1; }
# need <command> <how to install it>
need() {
  command -v "$1" >/dev/null 2>&1 || { echo "package: $1 is not installed: $2" >&2; exit 127; }
}
need_target() {
  [ -d "$(rustc --print sysroot)/lib/rustlib/$1" ] ||
    { echo "package: the $1 target is not installed: rustup target add $1" >&2; exit 127; }
}
need_docker() {
  need docker "install Docker (on macOS: brew install colima docker, then colima start)"
  docker info >/dev/null 2>&1 || die "docker does not answer: start its daemon (on macOS: colima start)"
  need rsync "install rsync"
}

# crate_version <Cargo.toml>: the [package] version.
crate_version() {
  sed -n '/^\[package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$1" | head -n 1
}

# host_pythons: the CPython 3 interpreters on this host, one per minor
# version, as "<path> <major.minor>" lines. PACKAGE_PYTHONS lists the
# paths to use; without it they are /opt/homebrew/bin/python3.N,
# /usr/local/bin/python3.N, /usr/bin/python3 and the python3 on PATH, the
# first of each version found.
host_pythons() {
  local candidates py info v seen=" "
  if [ -n "${PACKAGE_PYTHONS:-}" ]; then
    candidates=$PACKAGE_PYTHONS
  else
    candidates="$(ls /opt/homebrew/bin/python3.* /usr/local/bin/python3.* 2>/dev/null |
      grep -E '/python3\.[0-9]+$' || true) /usr/bin/python3 $(command -v python3 || true)"
  fi
  for py in $candidates; do
    [ -x "$py" ] || continue
    info=$("$py" -c 'import sys; print(sys.implementation.name, "%d.%d" % sys.version_info[:2])' 2>/dev/null) ||
      continue
    case $info in "cpython 3."*) ;; *) continue ;; esac
    v=${info#cpython }
    case $seen in *" $v "*) continue ;; esac
    seen="$seen$v "
    echo "$py $v"
  done
}

# The Docker platform of a Linux architecture, in Rust's, Python's or
# Node's spelling.
docker_platform() {
  case $1 in
    aarch64 | arm64) echo linux/arm64 ;;
    x86_64 | x64) echo linux/amd64 ;;
    *) die "no Docker platform for the architecture $1" ;;
  esac
}

# sync_tree <job>: copy the tree to $CACHE/$KEY/<job>/src and set JOB to
# $CACHE/$KEY/<job>. Without -t, rsync gives a copied file the current
# time, so cargo rebuilds exactly what changed (see linux-verify.sh).
sync_tree() {
  JOB="$CACHE/$KEY/$1"
  mkdir -p "$JOB/src" "$CACHE/registry"
  rsync -rl --checksum --delete --delete-excluded --filter=':- .gitignore' \
    --exclude .git --exclude /peripheral/fuzz/coverage/ --exclude /fuzz/coverage/ "$ROOT"/ "$JOB/src/"
}

# image <name> <platform> <Dockerfile text>: the local image built from
# the text, built the first time it is asked for. The tag carries a
# checksum of the text, so a changed recipe makes a new image;
# PACKAGE_REBUILD_IMAGES=1 rebuilds it anyway (to take newer base
# images). Prints the tag.
image() {
  local tag
  tag="pulse-mesh-package-$1:$(printf '%s' "$3" | cksum | cut -d ' ' -f 1)"
  if [ -n "${PACKAGE_REBUILD_IMAGES:-}" ] || ! docker image inspect "$tag" >/dev/null 2>&1; then
    echo "package: building the image $tag ($2)" >&2
    printf '%s\n' "$3" | docker build --platform "$2" -t "$tag" - >&2
  fi
  echo "$tag"
}

# The Linux legs' build images, <arch> in Rust's spelling. The
# containers set RUSTUP_TOOLCHAIN to the pinned toolchain, so rustup does
# not fetch the components rust-toolchain.toml lists (rustfmt, clippy) in
# every new container, as it would for the file.
#   wheels-<arch>   manylinux_2_28 with rustup and the pinned toolchain
#                   (minimal profile), installed with rustup's installer
#   node-<arch>     the Rust image of the Docker host's own architecture,
#                   with the Node image's node and npm, and Debian's cross
#                   compiler and the Rust target for the other
#                   architecture, so both modules build without emulation
#                   (cargo under QEMU's x86_64 emulation crashed and hung
#                   on an arm64 Mac)
wheels_image() {
  image "wheels-$1" "$(docker_platform "$1")" "FROM ${MANYLINUX_IMAGE}_$1
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:\$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain $CHANNEL && rustc --version"
}
node_build_image() {
  local other cross
  case $1 in
    aarch64) other=x86_64 cross="gcc-x86-64-linux-gnu libc6-dev-amd64-cross" ;;
    x86_64) other=aarch64 cross="gcc-aarch64-linux-gnu libc6-dev-arm64-cross" ;;
    *) die "no Node build image for the architecture $1" ;;
  esac
  image "node-$1" "$(docker_platform "$1")" "FROM $NODE_IMAGE AS node
FROM $RUST_IMAGE
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm && node --version && npm --version
RUN apt-get update && apt-get install -y --no-install-recommends $cross && rm -rf /var/lib/apt/lists/*
RUN rustup target add $other-unknown-linux-gnu && rustc --version"
}
# The Docker host's architecture, in Rust's spelling.
docker_arch() {
  case $(docker info --format '{{.Architecture}}') in
    aarch64 | arm64) echo aarch64 ;;
    x86_64 | amd64) echo x86_64 ;;
    *) die "Docker runs on an architecture the packaging does not know: $(docker info --format '{{.Architecture}}')" ;;
  esac
}

# list_artifacts <file>...: each artifact's size and name, for the report.
list_artifacts() {
  local f
  for f in "$@"; do
    [ -f "$f" ] && printf '  %10s  %s\n' "$(wc -c <"$f" | tr -d ' ')" "${f#"$ROOT"/}"
  done
  return 0
}

# license_files <dir>: the repository's license material into a package's
# staging directory: LICENSE always, NOTICE and THIRD_PARTY_NOTICES.md where
# the repository carries them (a release cut does). Every package carries the
# terms it is licensed under and the third-party notices its contents need.
license_files() {
  [ -f "$ROOT/LICENSE" ] || die "no LICENSE at $ROOT"
  cp "$ROOT/LICENSE" "$1/"
  local f
  for f in NOTICE THIRD_PARTY_NOTICES.md; do
    [ ! -f "$ROOT/$f" ] || cp "$ROOT/$f" "$1/"
  done
}
