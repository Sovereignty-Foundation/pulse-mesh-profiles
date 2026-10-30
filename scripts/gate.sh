#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# The publish gate. It needs no `just`, and runs on macOS's bash 3.2 and
# on Linux.
#
#   scripts/gate.sh           what a change passes before it merges
#   scripts/gate.sh --quick   the per-commit subset
#   scripts/gate.sh --full    the gate, plus the bindings, the fuzz build,
#                             the thin clients and the Linux and x86 legs
#   ... --skip <check>[,<check>...]
#                             the same, less the checks named, where they
#                             do not apply (see CI below)
#
# Every check runs, whatever the others did, and is judged by its exit
# status alone. Its output goes to target/gate/<check>.log. The gate prints
# one line per check with its time, a failed line ending with the last line
# of its log, then a summary, and exits 1 if any check failed. A check
# whose tool is missing fails and names the command that installs it;
# nothing is skipped but what --skip names. The gate refuses a --skip name
# that is not a check of its mode, and its first and last lines name the
# checks it left out. A check the tree leaves out (below) may be named in
# --skip, so one --skip list serves the integrated tree and every
# repository cut from it. A check over a surface the tree does not hold (the
# fuzz harness, the bindings, the thin clients, the daemon, the basket
# query tool, the comparator harnesses: each in the integrated tree, some
# in a repository cut from it) is left out too, and the first line names
# it.
#
# CI: the workflows run this script rather than restate its checks: in
# the integrated tree .gitea/workflows/ci.yml (and ci-compat.yml, the gate
# on macOS), and in each repository cut from it .github/workflows/ci.yml.
# Each runs the gate on a pull request, and on a push the full gate with
# --skip linux-test-1g,linux-test-64m,linux-net,linux-clippy: those four
# bring Linux to a Mac, in Docker, and a Linux runner needs none but
# linux-net, its test and clippy-all-features checks running on Linux
# already, with the /dev/shm the runner has; linux-net needs a privileged
# container, which the runner does not give.
#
# The gate runs in a git checkout or in a tree without .git, such as an
# export, where every check runs as in a checkout but license: the license
# gate reads the tracked files with git ls-files, so without .git that
# check fails, saying it needs a checkout. The versions check walks the
# tree where git cannot list its files, and in a checkout checks that the
# walk finds the files git lists.
#
# The gate:
#   fmt                  cargo fmt --check, in the workspace and in the
#                        fuzz harness, which is a workspace of its own
#   license              scripts/license-gate.sh, also the pre-push hook
#   versions             scripts/check-versions.py: every version string
#                        in the tree is the workspace's, and every
#                        rust-version agrees with the toolchain
#                        rust-toolchain.toml pins
#   arch-check           scripts/arch-check.py: the tier, import-point,
#                        consumer, module-table, non_exhaustive and pin
#                        rules, then the check's own self-test
#   clippy               clippy -D warnings on every target
#   clippy-all-features  the same with every feature on
#   check-all-features   cargo check with every feature and no test
#                        targets, so that a feature only a dev-dependency
#                        turns on cannot hide a broken build
#   hub-no-default       mesh-hub without its default features
#   rustdoc              cargo doc with warnings and broken or private
#                        intra-doc links denied
#   test                 the workspace tests, in the debug profile, so
#                        overflow checks and debug assertions run
#   multicast            pulse-wire-udp's multicast delivery tests, which
#                        a plain cargo test ignores: on a host that cannot
#                        deliver multicast they print SKIP: and check
#                        nothing, so this check fails on a SKIP: line and is
#                        green only when frames were delivered (on macOS a
#                        process without Local Network access cannot send;
#                        the loopback path usually still delivers)
#   canon-serde          pulse-canon's tests with its serde feature on;
#                        nothing in the workspace turns the feature on, so
#                        the test check alone never runs them
#   hub-otel             mesh-hub's tests with its otel feature on, the
#                        OpenTelemetry export the test check never builds
#   pms-query            tools/pms-query's tests, on basket files made
#                        with the schema mesh-basket gives the daemon's
#                        (they read it from the crate); here and not only
#                        in --full, as they take a tenth of a second, need
#                        only python3, and fail when a change to the
#                        crate's schema breaks the tool
#   deny                 cargo deny: advisories, bans and sources, with
#                        every feature on
#   docs                 scripts/docs/check.sh: the book's checks, mdbook
#                        build and the tutorials' smoke run
#
# --quick runs fmt, license, versions, arch-check, clippy and docs-structure
# (the book's version, SUMMARY, link and term checks). Each takes seconds,
# clippy re-checks only what changed, and together they catch what a commit most
# often breaks. The rest build a second configuration or release binaries,
# run the suites or need the network, and are left to the gate.
#
# --full adds:
#   bindings-<name>      the c, node, python and wasm (wasm32) bindings,
#                        each linted with clippy -D warnings, every
#                        target, in its own workspace against its
#                        committed lockfile (--locked)
#   bindings-<name>-test each binding built against its lockfile and run
#                        through its own tests:
#                        c       tests/run.sh, the C smoke test against the
#                                release library; the header the build
#                                script generates must be the one in the
#                                tree
#                        node    the module built with cargo and put where
#                                index.js loads it from, then npm test
#                        python  the module built with cargo for the
#                                python3 on PATH, then tests/ under pytest
#                        wasm    build.sh (cargo, then wasm-bindgen), then
#                                node tests/smoke.mjs
#                        They need a C compiler; Node.js, with the node
#                        binding's packages installed (npm ci); pytest,
#                        blake3 and numpy for python3; and wasm-bindgen
#                        at the version the wasm crate pins.
#   fuzz-build           the fuzz targets on nightly, once `cargo metadata
#                        --locked` has shown the harness's lockfile current
#   bench                the comparator harnesses (peripheral/bench), a
#                        workspace of their own, linted with clippy
#                        -D warnings, every target, against their
#                        committed lockfile (--locked)
#   bench-test           the harnesses' own tests: small bursts that check
#                        delivery and the summary's fields, never a figure
#   thin-python          the thin Python client's tests
#   thin-node            the thin Node client's tests, which, like
#                        thin-python's, stand a fake in for the daemon
#   thin-e2e             both thin clients against a real daemon
#                        (scripts/thin-e2e.py): mesh-hub on a socket, a
#                        port and a hub name of its own, the Python and
#                        the Node client attached, each subscribed and
#                        sending to the other, every payload byte, type
#                        name, origin and envelope id checked on arrival;
#                        then the daemon must stop cleanly on SIGTERM.
#                        Needs websocket-client for python3 and the Node
#                        client's ws installed (npm in its directory)
#   linux-test-1g        the workspace tests on Linux, in a container
#                        (scripts/linux-verify.sh), with a 1 GB /dev/shm
#   linux-test-64m       the same with Docker's default 64 MB /dev/shm
#   linux-net            the AF_XDP transport's tests (pulse-wire-xdp),
#                        which build veth pairs and namespaces and load
#                        XDP programs, in a privileged container
#                        (LINUX_VERIFY_NET=1); a test that skips for want
#                        of privilege fails the check
#   linux-clippy         clippy -D warnings on Linux with every feature on
#   x86-clippy           clippy -D warnings for x86_64 Linux with every
#                        feature on (scripts/cross-check.sh)
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)

usage() { echo "usage: scripts/gate.sh [--quick | --full] [--skip <check>[,<check>...]]" >&2; exit 2; }
MODE=default
SKIP=""
while [ $# -gt 0 ]; do
  case $1 in
    --quick | --full) [ "$MODE" = default ] || usage; MODE=${1#--} ;;
    --skip) [ $# -ge 2 ] || usage; SKIP="$SKIP $(printf '%s' "$2" | tr ',' ' ')"; shift ;;
    -h | --help) awk 'NR > 4 && /^#/ { sub(/^# ?/, ""); print; next } NR > 4 { exit }' "$0"; exit 0 ;;
    *) usage ;;
  esac
  shift
done
SKIP=$(printf '%s' "$SKIP" | tr -s ' ' | sed 's/^ //; s/ $//')

LOGDIR="$ROOT/target/gate"

# The surfaces a tree may hold beside its workspace: the fuzz harness, the
# bindings and the thin clients (under peripheral/ in the integrated tree,
# at the top level in a repository cut from it), the daemon, the basket
# query tool and the comparator harnesses (the integrated tree's alone).
# Each is its directory, or empty where the tree has it not.
FUZZ=""; for d in peripheral/fuzz fuzz; do [ -f "$d/Cargo.toml" ] && FUZZ=$d && break; done
BINDINGS=""; for d in peripheral/bindings bindings; do [ -d "$d/c" ] && BINDINGS=$d && break; done
THIN=""; for d in peripheral/bindings/dev-thin clients; do [ -d "$d/pms-py-thin" ] && THIN=$d && break; done
BENCH=""; [ -f peripheral/bench/Cargo.toml ] && BENCH=peripheral/bench
HUB=""; [ -f mesh/mesh-hub/Cargo.toml ] && HUB=mesh/mesh-hub
XDP=""; for d in plugins/hft-xdp plugins/hft-xdp; do [ -f "$d/Cargo.toml" ] && XDP=$d && break; done
PMS_QUERY=""; [ -d tools/pms-query ] && PMS_QUERY=tools/pms-query
CANON=""; [ -f pulse/pulse-canon/Cargo.toml ] && CANON=pulse/pulse-canon
UDP=""; [ -f core/pulse-wire-udp/Cargo.toml ] && UDP=core/pulse-wire-udp
# surface <checks> <dir>: whether the tree holds the surface the checks
# named are over; when not, they are left out and named.
left_out=""
surface() {
  [ -n "$2" ] && return 0
  left_out="$left_out $1"
  return 1
}

duration() {
  if [ "$1" -ge 60 ]; then printf '%dm%02ds' $(($1 / 60)) $(($1 % 60)); else printf '%ds' "$1"; fi
}
# The sum of a log's `test result:` lines, for the report only.
test_totals() {
  awk '/^test result:/ { p += $4; f += $6; i += $8; n++ }
       END { if (n) printf "%d passed, %d failed, %d ignored", p, f, i }' "$1"
}
# A log's last non-blank line, without colour codes, cut to 120 bytes.
last_line() {
  local esc
  esc=$(printf '\033')
  sed "s/${esc}\[[0-9;]*[A-Za-z]//g" "$1" |
    awk 'NF { l = $0 } END { sub(/^[ \t]+/, "", l); print substr(l, 1, 120) }'
}

# need <command> <how to install it>: fail the check when it is missing.
need() {
  command -v "$1" >/dev/null 2>&1 && return 0
  echo "$1 is not installed: $2"
  return 127
}
need_nightly() {
  cargo +nightly --version >/dev/null 2>&1 && return 0
  echo "the nightly toolchain is not installed: rustup toolchain install nightly"
  return 127
}
# need_target <triple>: the target's standard library, for the toolchain
# rust-toolchain.toml selects in the current directory.
need_target() {
  [ -d "$(rustc --print sysroot 2>/dev/null)/lib/rustlib/$1" ] && return 0
  echo "the $1 target is not installed: rustup target add $1"
  return 127
}
PYTHON="install Python 3"
NODE="install Node.js 18 or later"
# need_py <module>[=<package>]...: fail when python3 cannot import one of
# the modules, naming the packages that provide the missing ones.
need_py() {
  local spec missing=""
  for spec in "$@"; do
    python3 -c "import ${spec%%=*}" >/dev/null 2>&1 || missing="$missing ${spec#*=}"
  done
  [ -z "$missing" ] && return 0
  echo "python3 lacks$missing: python3 -m pip install$missing"
  return 127
}
# need_npm <dir> <package>...: fail when a package that <dir>'s
# package.json names is not installed in <dir>/node_modules.
need_npm() {
  local dir=$1 package how="npm ci"
  shift
  [ -f "$dir/package-lock.json" ] || how="npm install"
  for package in "$@"; do
    [ -f "$dir/node_modules/$package/package.json" ] && continue
    echo "$package is not installed in $dir: (cd $dir && $how)"
    return 127
  done
}
# The suffix of the shared libraries cargo builds on this host.
case $(uname -s) in Darwin) DYLIB=dylib ;; *) DYLIB=so ;; esac

c_fmt() {
  cargo fmt --all -- --check &&
    { [ -z "$FUZZ" ] || cargo fmt --manifest-path "$FUZZ/Cargo.toml" --all -- --check; }
}
# The license gate reads the tracked files with `git ls-files`, so outside
# a checkout it cannot run; the check fails saying so.
c_license() {
  [ -e .git ] || {
    echo "the license gate needs a git checkout: it reads the tracked files with git ls-files, and this tree has no .git"
    return 1
  }
  need python3 "$PYTHON" && need git "install git" && scripts/license-gate.sh
}
c_versions() { need python3 "$PYTHON" && python3 scripts/check-versions.py; }
c_arch() {
  need python3 "$PYTHON" &&
    python3 scripts/arch-check.py && python3 scripts/arch-check.py --self-test
}
c_clippy() { cargo clippy --workspace --all-targets -- -D warnings; }
c_clippy_all() { cargo clippy --workspace --all-targets --all-features -- -D warnings; }
c_check_all() { cargo check --workspace --all-features; }
c_hub_no_default() { cargo check -p mesh-hub --no-default-features; }
c_rustdoc() {
  RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links -D rustdoc::private-intra-doc-links" \
    cargo doc --workspace --no-deps
}
c_test() { cargo test --workspace --no-fail-fast; }
c_multicast() {
  cargo test -p pulse-wire-udp --test multicast -- --ignored --nocapture 2>&1 |
    tee /dev/stderr | { ! grep -q 'SKIP:'; }
}
c_canon_serde() { cargo test -p pulse-canon --features serde; }
c_hub_otel() { cargo test -p mesh-hub --features otel --test otel; }
c_pms_query() {
  need python3 "$PYTHON" && cd "$PMS_QUERY" &&
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests
}
c_deny() {
  need cargo-deny "cargo install cargo-deny --locked" &&
    cargo deny --all-features check advisories bans sources
}
c_docs() {
  need python3 "$PYTHON" && need mdbook "cargo install mdbook --version 0.5.4 --locked" &&
    scripts/docs/check.sh
}
c_docs_structure() {
  need python3 "$PYTHON" &&
    python3 scripts/docs/check-version.py && python3 scripts/docs/check-summary.py &&
    python3 scripts/docs/check-links.py && python3 scripts/docs/lint-terms.py
}
c_binding() {
  cd "$BINDINGS/$1" && cargo clippy --locked --all-targets -- -D warnings
}
c_binding_wasm() {
  cd "$BINDINGS/wasm" && need_target wasm32-unknown-unknown &&
    cargo clippy --locked --all-targets --target wasm32-unknown-unknown -- -D warnings
}
# Each binding's test script builds with plain cargo, which would rewrite a
# stale lockfile, so a --locked build comes first and leaves it nothing to do.
# The C binding's build script writes include/pulse_wire.h when what cbindgen
# generates differs from it, but runs only when one of its inputs changed;
# touching cbindgen.toml makes it run every time.
c_binding_c_test() {
  local cc=${CC:-cc} header=include/pulse_wire.h
  need "${cc%% *}" "install a C compiler" && cd "$BINDINGS/c" &&
    cp "$header" "$LOGDIR/pulse_wire.h" && touch cbindgen.toml &&
    cargo build --locked --release && tests/run.sh || return
  cmp -s "$LOGDIR/pulse_wire.h" "$header" && return 0
  echo "the build rewrote $header: the one in the tree is not what cbindgen generates; commit the new one"
  return 1
}
c_binding_node_test() {
  local platform
  need node "$NODE" && need npm "$NODE" &&
    need_npm "$BINDINGS/node" @noble/hashes || return
  # The name index.js looks for: index.<platform>-<arch>[-gnu|-musl].node.
  platform=$(node -p 'const { platform, arch } = process; platform === "linux"
    ? `${platform}-${arch}-${process.report.getReport().header.glibcVersionRuntime ? "gnu" : "musl"}`
    : `${platform}-${arch}`') &&
    cd "$BINDINGS/node" && cargo build --locked &&
    cp "target/debug/libpulse_wire.$DYLIB" "index.$platform.node" && npm test
}
c_binding_python_test() {
  need python3 "$PYTHON" && need_py pytest blake3 numpy &&
    cd "$BINDINGS/python" &&
    PYO3_PYTHON=$(command -v python3) cargo build --locked &&
    rm -rf target/module && mkdir target/module &&
    cp "target/debug/libpulse_wire.$DYLIB" target/module/pulse_wire.so &&
    PYTHONPATH=target/module PYTHONDONTWRITEBYTECODE=1 python3 -m pytest -p no:cacheprovider tests
}
c_binding_wasm_test() {
  local pin
  pin=$(sed -n 's/^wasm-bindgen = "=\(.*\)"/\1/p' "$BINDINGS/wasm/Cargo.toml")
  need node "$NODE" && cd "$BINDINGS/wasm" && need_target wasm32-unknown-unknown || return
  if [ "$(wasm-bindgen --version 2>/dev/null)" != "wasm-bindgen $pin" ]; then
    echo "wasm-bindgen $pin is not installed: cargo install wasm-bindgen-cli --version $pin --locked"
    return 127
  fi
  cargo build --locked --release --target wasm32-unknown-unknown && ./build.sh && node tests/smoke.mjs
}
c_fuzz_build() {
  need_nightly && need cargo-fuzz "cargo install cargo-fuzz --locked" &&
    cargo +nightly metadata --locked --format-version 1 --manifest-path "$FUZZ/Cargo.toml" >/dev/null &&
    cargo +nightly fuzz build --fuzz-dir "$FUZZ"
}
c_bench() {
  cd "$BENCH" && cargo clippy --locked --all-targets -- -D warnings
}
c_bench_test() {
  cd "$BENCH" && cargo test --locked
}
c_thin_python() {
  need python3 "$PYTHON" && cd "$THIN/pms-py-thin" &&
    python3 -m unittest discover -s tests -t .
}
c_thin_node() {
  need node "$NODE" &&
    cd "$THIN/pms-node-thin" && node --test
}
c_thin_e2e() {
  need python3 "$PYTHON" && need node "$NODE" && need_py websocket=websocket-client &&
    need_npm "$THIN/pms-node-thin" ws &&
    cargo build --release -p mesh-hub &&
    python3 scripts/thin-e2e.py "${CARGO_TARGET_DIR:-target}/release/mesh-hub"
}
c_linux_test() { SHM_SIZE=$1 scripts/linux-verify.sh test --workspace --no-fail-fast; }
c_linux_clippy() { scripts/linux-verify.sh clippy --workspace --all-targets --all-features -- -D warnings; }
c_linux_net() {
  LINUX_VERIFY_NET=1 scripts/linux-verify.sh test -p pulse-wire-xdp --no-fail-fast -- --nocapture 2>&1 |
    tee /dev/stderr | { ! grep -q 'SKIP:'; }
}
c_x86_clippy() { scripts/cross-check.sh; }

# Every check of the mode, in order, each through run().
checks() {
  run fmt c_fmt
  run license c_license
  run versions c_versions
  run arch-check c_arch
  run clippy c_clippy
  if [ "$MODE" = quick ]; then
    run docs-structure c_docs_structure
  else
    run clippy-all-features c_clippy_all
    run check-all-features c_check_all
    if surface hub-no-default "$HUB"; then run hub-no-default c_hub_no_default; fi
    run rustdoc c_rustdoc
    run test c_test
    if surface multicast "$UDP"; then run multicast c_multicast; fi
    if surface canon-serde "$CANON"; then run canon-serde c_canon_serde; fi
    if surface hub-otel "$HUB"; then run hub-otel c_hub_otel; fi
    if surface pms-query "$PMS_QUERY"; then run pms-query c_pms_query; fi
    run deny c_deny
    run docs c_docs
  fi
  if [ "$MODE" = full ]; then
    if surface "bindings-<name> bindings-<name>-test" "$BINDINGS"; then
      run bindings-c c_binding c
      run bindings-c-test c_binding_c_test
      run bindings-node c_binding node
      run bindings-node-test c_binding_node_test
      run bindings-python c_binding python
      run bindings-python-test c_binding_python_test
      run bindings-wasm c_binding_wasm
      run bindings-wasm-test c_binding_wasm_test
    fi
    if surface fuzz-build "$FUZZ"; then run fuzz-build c_fuzz_build; fi
    if surface "bench bench-test" "$BENCH"; then
      run bench c_bench
      run bench-test c_bench_test
    fi
    if surface "thin-python thin-node thin-e2e" "$THIN"; then
      run thin-python c_thin_python
      run thin-node c_thin_node
      if surface thin-e2e "$HUB"; then run thin-e2e c_thin_e2e; fi
    fi
    run linux-test-1g c_linux_test 1g
    run linux-test-64m c_linux_test 64m
    if surface linux-net "$XDP"; then run linux-net c_linux_net; fi
    run linux-clippy c_linux_clippy
    run x86-clippy c_x86_clippy
  fi
}

# The names first, so that a --skip naming no check stops the gate before
# anything runs.
names=""
run() { names="$names $1"; }
checks
LEFT_OUT=$left_out
left_out=""
set -f
for name in $SKIP; do
  case " $names $LEFT_OUT " in
    *" $name "*) ;;
    *) echo "gate: --skip $name: the $MODE gate has no such check; it has:$names" >&2; exit 2 ;;
  esac
done
set +f

passed=0
failed=0
failures=""
# run <name> <check>: run a check in a subshell, its output to its log,
# unless --skip names it.
run() {
  local name=$1 log="$LOGDIR/$1.log" start=$SECONDS rc=0 took note=""
  shift
  case " $SKIP " in *" $name "*) printf 'skip  %s\n' "$name"; return ;; esac
  ("$@") >"$log" 2>&1 </dev/null || rc=$?
  took=$(duration $((SECONDS - start)))
  case $name in test | linux-test-* | linux-net) note=$(test_totals "$log") ;; esac
  if [ "$rc" -eq 0 ]; then
    passed=$((passed + 1))
    printf 'ok    %-20s %7s%s\n' "$name" "$took" "${note:+  $note}"
  else
    failed=$((failed + 1))
    failures="$failures $name"
    printf 'FAIL  %-20s %7s  %s%s (%s)\n' "$name" "$took" "${note:+$note; }" \
      "$(last_line "$log")" "${log#"$ROOT"/}"
  fi
}

# The commit the tree is at, or, outside a git checkout, that it is not
# one: asked there, git would name the commit of any checkout the tree
# sits in.
if [ ! -e .git ]; then
  at="a tree without .git"
elif at=$(git log -1 --format='%h %s' 2>/dev/null); then
  [ -z "$(git status --porcelain 2>/dev/null)" ] || at="$at, with local changes"
else
  at="a git checkout that git cannot read"
fi
without=${SKIP:+, without $SKIP}
echo "gate ($MODE$without) on $at; logs in ${LOGDIR#"$ROOT"/}/${LEFT_OUT:+; left out, over surfaces this tree has not:$LEFT_OUT}"

mkdir -p "$LOGDIR" || exit 2
rm -f "$LOGDIR"/*.log
checks

total=$((passed + failed))
if [ "$failed" -eq 0 ]; then
  echo "gate ($MODE$without): all $total checks passed in $(duration "$SECONDS")"
  echo "GATE GREEN"
  exit 0
fi
echo "gate ($MODE$without): $failed of $total checks failed (${failures# }) in $(duration "$SECONDS")"
echo "GATE RED"
exit 1
