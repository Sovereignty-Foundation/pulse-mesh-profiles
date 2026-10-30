// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//
// The Node package's smoke run, in a project it was installed in:
//
//   node smoke-node.cjs <version> <envelope-raw-known-good.json>
//
// scripts/package/smoke.sh installs the tarball into a fresh npm project,
// copies this file there and runs it. Checks, through require('pulse-wire'),
// that the package is the one installed in the project, at <version>; that
// decodeEnvelope reads the specification's raw fixture to its id and
// encodeEnvelope writes it back to its bytes, and that an envelope of its own
// round-trips; and that bindPair on loopback carries a pulse with that
// envelope to another pair and a reply back to the address it came from.
// Exits 1 with the check that failed.
'use strict';
const assert = require('node:assert/strict');
const { readFileSync, realpathSync } = require('node:fs');
const { dirname, join } = require('node:path');

async function main() {
  const [version, fixture] = process.argv.slice(2);
  const where = dirname(require.resolve('pulse-wire'));
  assert.equal(realpathSync(where), realpathSync(join(__dirname, 'node_modules', 'pulse-wire')), `pulse-wire came from ${where}`);
  assert.equal(require('pulse-wire/package.json').version, version);
  const pw = require('pulse-wire');

  const expected = JSON.parse(readFileSync(fixture, 'utf8')).expected;
  const canonical = Buffer.from(expected.canonical_hex, 'hex');
  const decoded = pw.decodeEnvelope(canonical);
  assert.equal(decoded.id.toString('hex'), expected.id_hex, 'the fixture decodes to its id');
  assert.ok(pw.encodeEnvelope(decoded).equals(canonical), 'the fixture encodes back to its bytes');

  const input = { envelopeType: 1, typeName: 'smoke/hello', payload: Buffer.from('payload'), flags: 1 };
  const wire = pw.encodeEnvelope(input);
  const back = pw.decodeEnvelope(wire);
  assert.ok(back.id.equals(pw.envelopeId(input)), 'the id is the content id');
  assert.equal(back.typeName, 'smoke/hello');
  assert.ok(back.payload.equals(Buffer.from('payload')));
  assert.ok(pw.encodeEnvelope(back).equals(wire), 'the envelope round-trips');

  const a = pw.bindPair('127.0.0.1:0');
  const b = pw.bindPair('127.0.0.1:0');
  const pulse = new pw.Pulse();
  pulse.instruction = pw.Instruction.Data;
  a.sender.sendTo(1n, pulse, wire, b.receiver.localAddr());
  const frame = await b.receiver.recvAsync(2000);
  assert.ok(frame, 'nothing arrived within 2 s');
  assert.ok(frame.envelope.equals(wire), 'the datagram arrived altered');
  assert.equal(pw.Pulse.fromBytes(frame.pulseBytes).instruction, pw.Instruction.Data);
  assert.equal(frame.peer, a.receiver.localAddr(), 'the datagram came from the pair\'s port');
  b.sender.sendTo(2n, pulse, Buffer.from('pong'), frame.peer);
  const reply = await a.receiver.recvAsync(2000);
  assert.ok(reply && reply.envelope.equals(Buffer.from('pong')), 'the reply did not come back');

  console.log(`ok    node ${process.version} ${process.platform}-${process.arch}: pulse-wire ${version}, ` +
    `fixture id ${expected.id_hex}, envelope ${wire.length} bytes, bindPair loopback both ways`);
}

main().then(() => process.exit(0), (err) => {
  console.log(`FAIL  node ${process.version} ${process.platform}-${process.arch}: ${err.message}`);
  process.exit(1);
});
