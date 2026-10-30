#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# Install each artifact in target/package/ into a clean environment and
# run a short round trip through it. It publishes nothing and fetches no
# package: every install is from the local file, with pip --no-index and
# npm --offline.
#
#   scripts/package/smoke.sh [host] [linux-aarch64] [linux-x86_64]
#
# With no leg named: host, and each Linux architecture target/package/
# holds a wheel or a Node module for.
#   host           each wheel for this platform, with the host's CPython
#                  of its version (see host_pythons in lib.sh; a wheel for
#                  a version the host lacks is skipped, saying so); the
#                  Node package, when it holds this platform's module; the
#                  WebAssembly package
#   linux-<arch>   in Docker: each manylinux_2_28_<arch> wheel with the
#                  CPython of its version in the manylinux_2_28 image, and
#                  the Node and WebAssembly packages in Node's slim Debian
#                  image, which has no Rust (see lib.sh)
# A wheel goes into a new virtual environment (pip install --no-index
# --no-deps), a package into a new npm project (npm install --offline),
# each in a temporary directory; the checks run from there, with
# scripts/package/smoke.py, smoke-node.cjs and smoke-wasm.mjs, and use
# the specification's raw envelope fixture,
# pulse/pulse-spec/test-vectors/envelope-raw-known-good. Each check prints
# one ok or FAIL line; the run ends with the count and exits 1 if any
# failed, or if it found nothing to check.
#
# Prerequisites: the artifacts (scripts/package/all.sh, or the script
# that makes each); for host, the interpreters, and node and npm; for the
# Linux legs, Docker and the images (pulled the first time).
. "$(dirname "$0")/lib.sh"

FIXTURE=pulse/pulse-spec/test-vectors/envelope-raw-known-good
[ -f "$FIXTURE.json" ] || die "no $FIXTURE.json in this tree"
PY_VERSION=$(crate_version "$BINDINGS/python/Cargo.toml")
NODE_VERSION=$(crate_version "$BINDINGS/node/Cargo.toml")
WASM_VERSION=$(crate_version "$BINDINGS/wasm/Cargo.toml")
NODE_TGZ="$OUT/pulse-wire-$NODE_VERSION.tgz"
WASM_TGZ="$OUT/pulse-wire-wasm-$WASM_VERSION.tgz"
HERE="$ROOT/scripts/package"

# node_modules_in <tgz>: the Node modules the package holds, one per line.
node_modules_in() {
  [ -f "$1" ] && tar tzf "$1" | sed -n 's|^package/\(index\..*\.node\)$|\1|p'
  return 0
}

legs=host
for arch in aarch64 x86_64; do
  case $arch in aarch64) node_arch=arm64 ;; x86_64) node_arch=x64 ;; esac
  if ls "$OUT"/pulse_wire-*-manylinux_2_28_"$arch".whl >/dev/null 2>&1 ||
    node_modules_in "$NODE_TGZ" | grep -qx "index.linux-$node_arch-gnu.node"; then
    legs="$legs linux-$arch"
  fi
done
[ $# -eq 0 ] || legs="$*"
for leg in $legs; do
  case $leg in host | linux-aarch64 | linux-x86_64) ;; *) die "no such leg: $leg (host, linux-aarch64, linux-x86_64)" ;; esac
done

passed=0
failed=0
skipped=0
# tally <output>: print a check's lines and count them.
tally() {
  [ -n "$1" ] || return 0
  printf '%s\n' "$1"
  passed=$((passed + $(printf '%s\n' "$1" | grep -c '^ok ' || true)))
  failed=$((failed + $(printf '%s\n' "$1" | grep -c '^FAIL ' || true)))
  skipped=$((skipped + $(printf '%s\n' "$1" | grep -c '^skip ' || true)))
}
# check <label> <command>...: run one check, its output tallied; a check
# that fails without a FAIL line of its own gets one.
check() {
  local label=$1 output rc=0
  shift
  output=$("$@" 2>&1) || rc=$?
  if [ "$rc" -ne 0 ] && ! printf '%s\n' "$output" | grep -q '^FAIL '; then
    output="$output
FAIL  $label (exit $rc)"
  fi
  tally "$output"
}

TMP=$(mktemp -d "${TMPDIR:-/tmp}/pulse-mesh-smoke.XXXXXXXX")
trap 'rm -rf "$TMP"' EXIT

# A wheel, in a new virtual environment of <python>, checked from $TMP.
smoke_wheel() {
  local py=$1 whl=$2 venv
  venv=$(mktemp -d "$TMP/venv.XXXXXX")
  "$py" -m venv "$venv" &&
    "$venv/bin/python" -m pip install --quiet --no-index --no-deps --disable-pip-version-check "$whl" &&
    (cd "$TMP" && "$venv/bin/python" "$HERE/smoke.py" "$PY_VERSION" "$ROOT/$FIXTURE.json")
}
# A package, in a new npm project, checked by <program> copied into it.
smoke_package() {
  local tgz=$1 program=$2 project
  shift 2
  project=$(mktemp -d "$TMP/npm.XXXXXX")
  printf '{ "private": true }\n' >"$project/package.json"
  (cd "$project" && npm install --offline --no-audit --no-fund --no-package-lock --loglevel=error "$tgz" >/dev/null) &&
    cp "$HERE/$program" "$project/" &&
    (cd "$project" && node "$program" "$@")
}

host() {
  local arch pattern whl tag v py module
  arch=$(uname -m)
  case $(uname -s) in
    Darwin) pattern="macosx_*_$arch" ;;
    *) pattern="manylinux_*_$arch" ;;
  esac
  pythons=$(host_pythons)
  for whl in "$OUT"/pulse_wire-*-$pattern.whl; do
    [ -f "$whl" ] || continue
    tag=$(basename "$whl" | cut -d - -f 3)
    v="${tag:2:1}.${tag:3}"
    py=$(printf '%s\n' "$pythons" | awk -v v="$v" '$2 == v { print $1; exit }')
    if [ -z "$py" ]; then
      tally "skip  $(basename "$whl"): no CPython $v on this host"
      continue
    fi
    check "$(basename "$whl")" smoke_wheel "$py" "$whl"
  done
  if [ -f "$NODE_TGZ" ] && command -v node >/dev/null 2>&1; then
    module=$(node -p 'const { platform, arch } = process; platform === "linux"
      ? `index.${platform}-${arch}-${process.report.getReport().header.glibcVersionRuntime ? "gnu" : "musl"}.node`
      : `index.${platform}-${arch}.node`')
    if node_modules_in "$NODE_TGZ" | grep -qx "$module"; then
      check "$(basename "$NODE_TGZ")" smoke_package "$NODE_TGZ" smoke-node.cjs "$NODE_VERSION" "$ROOT/$FIXTURE.json"
    else
      tally "skip  $(basename "$NODE_TGZ"): it holds no $module"
    fi
  fi
  if [ -f "$WASM_TGZ" ] && command -v node >/dev/null 2>&1; then
    check "$(basename "$WASM_TGZ")" smoke_package "$WASM_TGZ" smoke-wasm.mjs "$WASM_VERSION" \
      "$ROOT/$FIXTURE.json" "$ROOT/$FIXTURE.hex"
  fi
}

linux() {
  local arch=$1 node_arch files platform out f
  case $arch in aarch64) node_arch=arm64 ;; x86_64) node_arch=x64 ;; esac
  platform=$(docker_platform "$arch")
  files="$CACHE/$KEY/smoke/$arch"
  rm -rf "$files"
  mkdir -p "$files"
  cp "$HERE/smoke.py" "$HERE/smoke-node.cjs" "$HERE/smoke-wasm.mjs" "$FIXTURE.json" "$FIXTURE.hex" "$files/"
  for f in "$OUT"/pulse_wire-*-manylinux_2_28_"$arch".whl; do [ -f "$f" ] && cp "$f" "$files/"; done
  if ls "$files"/*.whl >/dev/null 2>&1; then
    out=$(docker run --rm --init --quiet --platform "$platform" -v "$files":/smoke:ro -e VERSION="$PY_VERSION" \
      "${MANYLINUX_IMAGE}_$arch" bash -uo pipefail -c '
        cd /tmp
        for whl in /smoke/*.whl; do
          name=$(basename "$whl"); t=$(printf "%s" "$name" | cut -d - -f 3)
          py=/opt/python/$t-$t/bin/python
          [ -x "$py" ] || { echo "FAIL  $name: no $t in the image"; continue; }
          { "$py" -m venv "/tmp/$t" &&
              "/tmp/$t/bin/python" -m pip install --quiet --no-index --no-deps --disable-pip-version-check "$whl" &&
              "/tmp/$t/bin/python" /smoke/smoke.py "$VERSION" /smoke/envelope-raw-known-good.json; } 2>&1 ||
            echo "FAIL  $name (exit $?)"
        done' 2>&1) || true
    tally "$out"
  fi
  if node_modules_in "$NODE_TGZ" | grep -qx "index.linux-$node_arch-gnu.node"; then
    cp "$NODE_TGZ" "$files/"
  fi
  [ -f "$WASM_TGZ" ] && cp "$WASM_TGZ" "$files/"
  if ls "$files"/*.tgz >/dev/null 2>&1; then
    out=$(docker run --rm --init --quiet --platform "$platform" -v "$files":/smoke:ro \
      -e NODE_VERSION="$NODE_VERSION" -e WASM_VERSION="$WASM_VERSION" -e NPM_CONFIG_UPDATE_NOTIFIER=false "$NODE_IMAGE" bash -uo pipefail -c '
        run() {
          local tgz=$1 program=$2 dir; shift 2
          dir=$(mktemp -d)
          cd "$dir" && printf "{ \"private\": true }\n" > package.json &&
            npm install --offline --no-audit --no-fund --no-package-lock --loglevel=error "$tgz" > /dev/null &&
            cp "/smoke/$program" . && node "$program" "$@" 2>&1 || echo "FAIL  $(basename "$tgz") (exit $?)"
        }
        f=/smoke/envelope-raw-known-good
        for tgz in /smoke/pulse-wire-[0-9]*.tgz; do [ -f "$tgz" ] && run "$tgz" smoke-node.cjs "$NODE_VERSION" $f.json; done
        for tgz in /smoke/pulse-wire-wasm-*.tgz; do [ -f "$tgz" ] && run "$tgz" smoke-wasm.mjs "$WASM_VERSION" $f.json $f.hex; done
        true' 2>&1) || true
    tally "$out"
  fi
}

for leg in $legs; do
  echo "smoke: $leg"
  case $leg in
    host) host ;;
    linux-*)
      need_docker
      linux "${leg#linux-}"
      ;;
  esac
done
echo "smoke: $passed passed, $failed failed, $skipped skipped"
[ "$failed" -eq 0 ] && [ "$passed" -gt 0 ]
