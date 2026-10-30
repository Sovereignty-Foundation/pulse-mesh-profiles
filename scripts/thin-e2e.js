// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//
// The Node half of scripts/thin-e2e.py, which runs it:
//
//     node scripts/thin-e2e.js <to-python channel> <to-node channel> <count>
//
// Attaches the thin Node client as e2e-node to the daemon PMS_DAEMON names,
// subscribes to the to-node channel and prints `subscribed`, then sends
// <count> envelopes on the to-python channel, printing `sent <envelope id>
// <payload hex>` for each, and prints `got <envelope id> <channel> <type
// name> <origin> <payload hex>` for each event, until <count> have come.
// Then it detaches and exits 0; on an error it prints `error <what>` and
// exits 1, and after 20 seconds it gives up the same way.

'use strict';

const fs = require('fs');
const path = require('path');

// The client: under peripheral/bindings/dev-thin in the integrated tree,
// under clients/ in the daemon's own repository.
const { attach } = require(
  ['../peripheral/bindings/dev-thin/pms-node-thin/index.js', '../clients/pms-node-thin/index.js']
    .map((rel) => path.resolve(__dirname, rel))
    .find((file) => fs.existsSync(file)),
);

const [toPython, toNode, countArg] = process.argv.slice(2);
const count = Number(countArg);

const fail = (what) => {
  console.log(`error ${String(what).replace(/\s+/g, ' ')}`);
  process.exit(1);
};
const giveUp = setTimeout(() => fail('20 s passed'), 20_000);

(async () => {
  const mesh = await attach('e2e-node');
  let received = 0;
  let allReceived;
  const done = new Promise((resolve) => {
    allReceived = resolve;
  });
  await mesh.on(toNode, (event) => {
    const fields = [event.envelopeId, event.channel, event.typeName, event.origin];
    console.log(`got ${fields.join(' ')} ${event.payload.toString('hex')}`);
    received += 1;
    if (received === count) allReceived();
  });
  console.log('subscribed');
  for (let i = 0; i < count; i += 1) {
    const payload = Buffer.concat([Buffer.from([0xff, 0x00, i]), Buffer.from('node to python, é')]);
    const id = await mesh.send(toPython, payload, 'e2e.from-node');
    console.log(`sent ${id} ${payload.toString('hex')}`);
  }
  await done;
  await mesh.detach();
  clearTimeout(giveUp);
})().catch((err) => fail(err && err.stack ? err.stack : err));
