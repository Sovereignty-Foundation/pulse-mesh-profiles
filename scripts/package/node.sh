#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# Build the Node binding's package into
# target/package/pulse-wire-<version>.tgz. It publishes nothing and
# uploads nothing: npm pack writes the tarball and does no more, and
# package.json keeps "private": true, which npm publish refuses.
#
#   scripts/package/node.sh [darwin-arm64] [darwin-x64] [linux-arm64] [linux-x64]
#
# With no target named: all four on a Mac, the two Linux ones elsewhere.
# The tarball holds package.json, index.js, index.d.ts and README.md from
# the binding, and the modules this run built: index.js loads
# index.<platform>-<arch>[-gnu].node from beside itself, so one package
# serves each platform it holds.
#
# Each module is built by the binding's `npm run build` (napi build
# --platform --release, with @napi-rs/cli 2 and against the lockfile) into
# a directory of its own under target/package/stage/, so the tree's
# index.js and index.d.ts are left as they are. napi writes its own copies
# of the two beside each module, and the run fails if they differ from the
# tree's, which would mean the committed loader or typings no longer match
# the Rust source. napi writes the two from a file the napi-derive macros
# fill while the crate compiles, in the temporary directory, so a build
# cargo finds fresh, as in a new container, would leave them out; the
# crate's own release build is therefore cleaned first (cargo clean -p),
# and only it is compiled again, not its dependencies.
#   darwin-arm64   natively, on an arm64 Mac
#   darwin-x64     cross-compiled on a Mac (--target x86_64-apple-darwin);
#                  skipped, saying so, when that target's standard
#                  library is not installed (rustup target add
#                  x86_64-apple-darwin). The smoke test cannot run it
#                  without an x64 Node.
#   linux-<arch>   in Docker, in rust:<channel>-bookworm with Node copied
#                  in (see lib.sh), of the Docker host's architecture: the
#                  module for that one natively, the other cross-compiled
#                  (--target, with Debian's cross gcc), as cargo under
#                  emulation is not reliable. The module needs the glibc
#                  symbol versions it was linked against there (Debian
#                  12's glibc 2.36); the run prints the newest it needs.
#
# Prerequisites: node 18 or later and npm, with the binding's packages
# installed (the script runs npm ci in the binding when @napi-rs/cli is
# missing); cargo; for the Linux targets, Docker and rsync, and the
# network, for the images the first time, for npm ci in each container and
# for crates the cargo cache lacks.
. "$(dirname "$0")/lib.sh"

need node "install Node.js 18 or later"
need npm "install Node.js 18 or later"
need cargo "install Rust: https://rustup.rs"
DIR="$BINDINGS/node"
VERSION=$(node -p 'require(process.argv[1]).version' "$ROOT/$DIR/package.json")
case $(uname -s) in
  Darwin) targets="darwin-arm64 darwin-x64 linux-arm64 linux-x64" ;;
  *) targets="linux-arm64 linux-x64" ;;
esac
[ $# -eq 0 ] || targets="$*"
for t in $targets; do
  case $t in
    darwin-arm64 | darwin-x64) [ "$(uname -s)" = Darwin ] || die "$t is built on a Mac" ;;
    linux-arm64 | linux-x64) ;;
    *) die "no such target: $t (darwin-arm64, darwin-x64, linux-arm64, linux-x64)" ;;
  esac
done

rm -rf "$STAGE/node" "$STAGE/node-build"
mkdir -p "$STAGE/node" "$STAGE/node-build"
cp "$DIR/package.json" "$DIR/index.js" "$DIR/index.d.ts" "$DIR/README.md" "$STAGE/node/"
license_files "$STAGE/node"
# napi writes into a directory relative to the binding's.
UP=$(printf '%s' "$DIR" | sed 's|[^/][^/]*|..|g')

# same_loader <dir>: napi's index.js and index.d.ts in <dir> are the tree's.
same_loader() {
  local f
  for f in index.js index.d.ts; do
    cmp -s "$1/$f" "$DIR/$f" ||
      die "the build's $f differs from $DIR/$f: the committed file does not match the Rust source; run npm run build in $DIR and commit it"
  done
}

darwin() {
  local t=$1 flags="" out
  if [ "$t" = darwin-x64 ]; then
    if [ ! -d "$(rustc --print sysroot)/lib/rustlib/x86_64-apple-darwin" ]; then
      echo "node: skipped darwin-x64: the x86_64-apple-darwin target is not installed (rustup target add x86_64-apple-darwin)" >&2
      return 0
    fi
    flags="--target x86_64-apple-darwin"
  elif [ "$(uname -m)" != arm64 ]; then
    die "darwin-arm64 is built on an arm64 Mac"
  fi
  [ -f "$DIR/node_modules/@napi-rs/cli/package.json" ] || (cd "$DIR" && npm ci --no-audit --no-fund)
  echo "node: $t" >&2
  out="$STAGE/node-build/$t"
  # $flags is empty or two words.
  # shellcheck disable=SC2086
  (cd "$DIR" && cargo clean --locked --quiet --release -p pulse-wire-node $flags &&
    npm run --silent build -- $flags --cargo-flags=--locked "$UP/target/package/stage/node-build/$t")
  same_loader "$out"
  cp "$out"/index.darwin-*.node "$STAGE/node/"
}

linux() {
  local arch=$1 rust_arch native image triple t flags="" env=""
  case $arch in arm64) rust_arch=aarch64 ;; x64) rust_arch=x86_64 ;; esac
  native=$(docker_arch)
  image=$(node_build_image "$native")
  # The Docker host's own architecture builds natively; the other is
  # cross-compiled, with Debian's cross gcc as the linker and as the C
  # compiler of the crates that build C (blake3).
  if [ "$rust_arch" != "$native" ]; then
    triple="$rust_arch-unknown-linux-gnu"
    flags="--target $triple"
    t=$(printf '%s' "$triple" | tr 'a-z-' 'A-Z_')
    env="CARGO_TARGET_${t}_LINKER=$rust_arch-linux-gnu-gcc CC_${triple//-/_}=$rust_arch-linux-gnu-gcc AR_${triple//-/_}=$rust_arch-linux-gnu-ar"
  fi
  echo "node: linux-$arch, in $image${flags:+, cross-compiled}" >&2
  rm -rf "$JOB/out-$arch"
  mkdir -p "$JOB/out-$arch" "$JOB/target-$native"
  docker run --rm --init --platform "$(docker_platform "$native")" \
    -v "$JOB/src":/src -v "$JOB/target-$native":/target -v "$CACHE/registry":/usr/local/cargo/registry \
    -v "$JOB/out-$arch":/out -e CARGO_TARGET_DIR=/target -e RUSTUP_TOOLCHAIN="$CHANNEL" \
    -e FLAGS="$flags" -e CROSS_ENV="$env" -w "/src/$DIR" "$image" bash -euo pipefail -c '
      npm ci --no-audit --no-fund --no-update-notifier --loglevel=error
      rm -rf napi-out
      cargo clean --locked --quiet --release -p pulse-wire-node $FLAGS
      env $CROSS_ENV npm run --silent build -- $FLAGS --cargo-flags=--locked napi-out
      cp napi-out/*.node napi-out/index.js napi-out/index.d.ts /out/
      for f in /out/*.node; do
        echo "node: $(basename "$f") needs glibc $(readelf --dyn-syms -W "$f" | grep -o "GLIBC_[0-9.]*" | sort -uV | tail -n 1)" >&2
      done'
  same_loader "$JOB/out-$arch"
  cp "$JOB/out-$arch"/index.linux-*.node "$STAGE/node/"
}

synced=""
for t in $targets; do
  case $t in
    darwin-*) darwin "$t" ;;
    linux-*)
      if [ -z "$synced" ]; then
        need_docker
        sync_tree node
        synced=1
      fi
      linux "${t#linux-}"
      ;;
  esac
done

ls "$STAGE/node"/*.node >/dev/null 2>&1 || die "no module was built"
rm -f "$OUT/pulse-wire-$VERSION.tgz"
tgz=$(cd "$STAGE/node" && npm pack --silent --pack-destination "$OUT" | tail -n 1)
echo "node: $tgz, with $(cd "$STAGE/node" && ls *.node | tr '\n' ' ')"
list_artifacts "$OUT/$tgz"
