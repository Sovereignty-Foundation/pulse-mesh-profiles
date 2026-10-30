#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# Build the WebAssembly binding's npm package into
# target/package/pulse-wire-wasm-<version>.tgz. It publishes nothing and
# uploads nothing: npm pack writes the tarball and does no more, and the
# package.json written here says "private": true, which npm publish
# refuses.
#
#   scripts/package/wasm.sh
#
# The binding's own build.sh builds the module (cargo, release, for
# wasm32-unknown-unknown; then wasm-bindgen --target web) into
# target/package/stage/wasm/, beside the binding's README.md and a
# package.json written here:
#   name     pulse-wire-wasm
#   version  the crate's (Cargo.toml)
#   private  true
#   type     module: pulse_wasm.js is an ES module
#   main, module
#            pulse_wasm.js, the generated module, for Node and for
#            bundlers
#   types    pulse_wasm.d.ts, its generated declarations
#   files    the generated files and README.md
# and nothing else. The module is the same on every platform, so one
# tarball serves all of them. --target web loads pulse_wasm_bg.wasm by
# URL when init() is given nothing; Node has to read the file and pass
# its bytes, as tests/smoke.mjs and scripts/package/smoke.sh do.
#
# Prerequisites: cargo, the wasm32-unknown-unknown target (rustup target
# add wasm32-unknown-unknown), wasm-bindgen-cli at the version Cargo.toml
# pins (build.sh refuses another), node and npm.
. "$(dirname "$0")/lib.sh"

DIR="$BINDINGS/wasm"
need cargo "install Rust: https://rustup.rs"
need node "install Node.js 18 or later"
need npm "install Node.js 18 or later"
need_target wasm32-unknown-unknown
PIN=$(sed -n 's/^wasm-bindgen = "=\(.*\)"/\1/p' "$DIR/Cargo.toml")
need wasm-bindgen "cargo install wasm-bindgen-cli --version $PIN --locked"
VERSION=$(crate_version "$DIR/Cargo.toml")

rm -rf "$STAGE/wasm"
mkdir -p "$STAGE/wasm"
# build.sh runs cargo without --locked; this build comes first and leaves
# it nothing to do, so the lockfile is held as the gate holds it.
(cd "$DIR" && cargo build --locked --release --target wasm32-unknown-unknown --quiet)
"$DIR/build.sh" "$STAGE/wasm" >/dev/null
cp "$DIR/README.md" "$STAGE/wasm/"
license_files "$STAGE/wasm"
node - "$STAGE/wasm" "$VERSION" <<'EOF'
const { existsSync, readdirSync, writeFileSync } = require('node:fs');
const { join } = require('node:path');
const [dir, version] = process.argv.slice(2);
const files = readdirSync(dir).filter((f) => /^pulse_wasm/.test(f)).sort();
for (const f of ['pulse_wasm.js', 'pulse_wasm.d.ts', 'pulse_wasm_bg.wasm']) {
  if (!files.includes(f)) throw new Error(`wasm-bindgen wrote no ${f} in ${dir}`);
}
const pkg = {
  name: 'pulse-wire-wasm',
  version,
  private: true,
  type: 'module',
  main: 'pulse_wasm.js',
  module: 'pulse_wasm.js',
  types: 'pulse_wasm.d.ts',
  license: 'UNLICENSED',
  files: [...files, 'README.md', ...['LICENSE', 'NOTICE', 'THIRD_PARTY_NOTICES.md'].filter((f) => existsSync(join(dir, f)))],
};
writeFileSync(join(dir, 'package.json'), JSON.stringify(pkg, null, 2) + '\n');
EOF
rm -f "$OUT/pulse-wire-wasm-$VERSION.tgz"
tgz=$(cd "$STAGE/wasm" && npm pack --silent --pack-destination "$OUT" | tail -n 1)
echo "wasm: $tgz"
list_artifacts "$OUT/$tgz"
