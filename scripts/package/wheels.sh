#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# Build the Python binding's wheels into target/package/. It publishes
# nothing and uploads nothing: the wheels stay in target/package/, for
# scripts/package/smoke.sh and for whoever installs them from there.
#
#   scripts/package/wheels.sh [macos] [linux-aarch64] [linux-x86_64]
#
# With no target named, it builds every one its host can: all three on a
# Mac, the two Linux ones elsewhere. A run replaces the wheels of the
# targets it builds and leaves the others.
#
# maturin fails on this crate's manifest as it stands, so the wheels are
# made without it, as the gate's bindings-python-test builds the module:
# cargo builds the extension (the crate's cdylib, release profile, against
# its lockfile) for one interpreter at a time, PYO3_PYTHON naming it, and
# scripts/package/wheel.py writes a wheel around each build: the module,
# pulse_wire.pyi, py.typed and the dist-info (see wheel.py).
#
#   macos           arm64 or x86_64, as the host is, for each CPython 3 on
#                   the host (PACKAGE_PYTHONS lists them; else
#                   /opt/homebrew/bin/python3.N, /usr/local/bin/python3.N,
#                   /usr/bin/python3 and the python3 on PATH, one per
#                   version). The platform tag is the deployment target
#                   the linker wrote into the module (LC_BUILD_VERSION's
#                   minos; 11.0 for arm64 by Rust's default, unless
#                   MACOSX_DEPLOYMENT_TARGET says otherwise), e.g.
#                   macosx_11_0_arm64. The module's install name is set to
#                   @rpath/<its file name>, as maturin sets it, rather than
#                   the build directory's path.
#   linux-<arch>    manylinux_2_28_<arch>, built in Docker in the PyPA's
#                   manylinux_2_28 image with rustup and the pinned
#                   toolchain added (see lib.sh), for CPython 3.10 to 3.13
#                   (PACKAGE_LINUX_PYTHONS, e.g. "3.12 3.13";
#                   PACKAGE_LINUX_PYTHONS_x86_64 for that architecture
#                   alone). auditwheel show, in the same container, then
#                   reads each wheel and must accept it. The other
#                   architecture than the Docker host's runs under
#                   emulation, several times slower; QEMU's (colima's
#                   default on an arm64 Mac) now and then crashes a
#                   process or stalls one, so there each step is given
#                   30 minutes and up to three attempts
#                   (PACKAGE_EMULATED_ATTEMPTS).
#
# Prerequisites: cargo and python3 3.11 or later (for wheel.py) on the
# host; for macos, the interpreters and Xcode's command line tools; for
# the Linux targets, Docker (colima on a Mac) and rsync, and the network
# the first time, to pull the images and install rustup in them, and
# whenever the lockfile names a crate the cargo cache lacks.
. "$(dirname "$0")/lib.sh"

CRATE="$BINDINGS/python"
VERSION=$(crate_version "$CRATE/Cargo.toml")
case $(uname -s) in Darwin) targets="macos linux-aarch64 linux-x86_64" ;; *) targets="linux-aarch64 linux-x86_64" ;; esac
[ $# -eq 0 ] || targets="$*"
for t in $targets; do
  case $t in
    macos) [ "$(uname -s)" = Darwin ] || die "macos wheels are built on a Mac" ;;
    linux-aarch64 | linux-x86_64) ;;
    *) die "no such target: $t (macos, linux-aarch64, linux-x86_64)" ;;
  esac
done
need cargo "install Rust: https://rustup.rs"
need python3 "install Python 3.11 or later"
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || die "python3 is older than 3.11, which wheel.py needs"

macos() {
  local arch lib minos platform py v suffix whl built=0
  arch=$(uname -m)
  rm -f "$OUT"/pulse_wire-*-macosx_*_"$arch".whl
  while read -r py v; do
    [ -n "$py" ] || continue
    suffix=$("$py" -c 'import sysconfig; print(sysconfig.get_config_var("EXT_SUFFIX"))')
    echo "wheels: macos, CPython $v ($py)" >&2
    # cargo rustc passes the link argument to this crate's link alone, so
    # the dependencies' builds are shared with plain cargo build.
    (cd "$CRATE" && PYO3_PYTHON=$py cargo rustc --locked --release --lib --quiet -- \
      -C "link-arg=-Wl,-install_name,@rpath/pulse_wire$suffix")
    lib="$CRATE/target/release/libpulse_wire.dylib"
    minos=$(otool -l "$lib" | awk '/LC_BUILD_VERSION/ { f = 1 } f && $1 == "minos" { print $2; exit }')
    [ -n "$minos" ] || die "no deployment target (LC_BUILD_VERSION minos) in $lib"
    case $minos in *.*) ;; *) minos="$minos.0" ;; esac
    platform="macosx_$(printf '%s' "$minos" | cut -d . -f 1)_$(printf '%s' "$minos" | cut -d . -f 2)_$arch"
    whl=$(python3 scripts/package/wheel.py --crate "$CRATE" --python "$py" --module "$lib" --platform "$platform" --out "$OUT")
    echo "wheels: $(basename "$whl")" >&2
    built=$((built + 1))
  done <<EOF
$(host_pythons)
EOF
  [ "$built" -gt 0 ] || die "macos: no CPython 3 interpreter found (PACKAGE_PYTHONS lists them)"
}

linux() {
  local arch=$1 image platform pythons attempts=1
  eval "pythons=\${PACKAGE_LINUX_PYTHONS_$arch:-}"
  [ -n "$pythons" ] || pythons=${PACKAGE_LINUX_PYTHONS:-3.10 3.11 3.12 3.13}
  platform="manylinux_2_28_$arch"
  image=$(wheels_image "$arch")
  [ "$arch" = "$(docker_arch)" ] || attempts=${PACKAGE_EMULATED_ATTEMPTS:-3}
  echo "wheels: linux-$arch, CPython $pythons, in $image" >&2
  rm -rf "$JOB/out-$arch"
  mkdir -p "$JOB/out-$arch" "$JOB/target-$arch"
  # In the container: one cargo build per interpreter, each into the same
  # target directory (only PyO3 and this crate rebuild between them), then
  # the wheel, written by the image's CPython 3.12, and auditwheel's
  # reading of it; each step under step(), which gives it 30 minutes and
  # $ATTEMPTS attempts.
  docker run --rm --init --platform "$(docker_platform "$arch")" \
    -v "$JOB/src":/src -v "$JOB/target-$arch":/target -v "$CACHE/registry":/usr/local/cargo/registry \
    -v "$JOB/out-$arch":/out -e CARGO_TARGET_DIR=/target -e RUSTUP_TOOLCHAIN="$CHANNEL" \
    -e PYTHONS="$pythons" -e PLATFORM="$platform" -e CRATE="/src/$CRATE" -e ATTEMPTS="$attempts" \
    -w "/src/$CRATE" "$image" bash -euo pipefail -c '
      step() {
        local n=1
        until timeout 1800 "$@"; do
          [ "$n" -lt "$ATTEMPTS" ] || return 1
          n=$((n + 1))
          echo "wheels: \"$*\" failed under emulation; attempt $n of $ATTEMPTS" >&2
        done
      }
      for v in $PYTHONS; do
        t=cp${v/./}
        py=/opt/python/$t-$t/bin/python
        [ -x "$py" ] || { echo "no CPython $v in the image (/opt/python/$t-$t)" >&2; exit 1; }
        echo "wheels: CPython $v" >&2
        step env PYO3_PYTHON="$py" cargo build --locked --release --lib --quiet
        whl=$(step /opt/python/cp312-cp312/bin/python /src/scripts/package/wheel.py --crate "$CRATE" \
          --python "$py" --module /target/release/libpulse_wire.so --platform "$PLATFORM" --out /out)
        step auditwheel show "$whl"
      done'
  rm -f "$OUT"/pulse_wire-*-"$platform".whl
  cp "$JOB/out-$arch"/*.whl "$OUT"/
}

synced=""
for t in $targets; do
  case $t in
    macos) macos ;;
    linux-*)
      if [ -z "$synced" ]; then
        need_docker
        sync_tree wheels
        synced=1
      fi
      linux "${t#linux-}"
      ;;
  esac
done
echo "wheels: pulse_wire $VERSION, in ${OUT#"$ROOT"/}:"
list_artifacts "$OUT"/pulse_wire-*.whl
