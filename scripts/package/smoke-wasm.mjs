// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//
// The WebAssembly package's smoke run, in Node, in a project it was
// installed in:
//
//   node smoke-wasm.mjs <version> <envelope-raw-known-good.json> <.hex>
//
// scripts/package/smoke.sh installs the tarball into a fresh npm project,
// copies this file there and runs it. Imports pulse-wire-wasm as a package,
// reads its pulse_wasm_bg.wasm and hands the bytes to init() (Node does not
// fetch the module by URL as a browser does); then checks the version, that
// check_fixture passes the specification's raw fixture, that envelope_decode
// reads the fixture's bytes to its id, verified, and that a typed envelope
// round-trips. Exits 1 with the check that failed.
import assert from 'node:assert/strict';
import { readFileSync, realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import init, * as pw from 'pulse-wire-wasm';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const [version, fixtureJson, fixtureHex] = process.argv.slice(2);
try {
  const wasm = require.resolve('pulse-wire-wasm/pulse_wasm_bg.wasm');
  assert.equal(realpathSync(dirname(wasm)), realpathSync(join(here, 'node_modules', 'pulse-wire-wasm')), `pulse-wire-wasm came from ${dirname(wasm)}`);
  await init({ module_or_path: readFileSync(wasm) });
  assert.equal(pw.version(), version);

  const json = readFileSync(fixtureJson, 'utf8');
  const hex = readFileSync(fixtureHex, 'utf8');
  const report = JSON.parse(pw.check_fixture(json, hex));
  assert.ok(report.ok, `check_fixture: ${report.checks.filter((c) => !c.ok).map((c) => c.check).join(', ')}`);
  const expected = JSON.parse(json).expected;
  const decoded = JSON.parse(pw.envelope_decode(Uint8Array.from(Buffer.from(expected.canonical_hex, 'hex'))));
  assert.equal(decoded.id_hex, expected.id_hex);
  assert.equal(decoded.verified, true);

  const bytes = pw.envelope_encode(JSON.stringify({ type_name: 'smoke/hello', payload_text: 'payload' }));
  const env = JSON.parse(pw.envelope_decode(bytes));
  assert.ok(env.verified && env.effective_type_name === 'smoke/hello', 'the typed envelope round-trips');
  assert.equal(pw.envelope_verify(bytes), true);

  console.log(`ok    wasm in node ${process.version}: pulse-wire-wasm ${pw.version()}, ` +
    `fixture ${report.checks.length} checks, id ${decoded.id_hex}, envelope ${bytes.length} bytes`);
} catch (err) {
  console.log(`FAIL  wasm in node ${process.version}: ${err.message}`);
  process.exit(1);
}
