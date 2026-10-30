#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""The Python wheel's smoke run, in a virtual environment it was installed in.

    smoke.py <version> <envelope-raw-known-good.json>

scripts/package/smoke.sh installs a wheel into a fresh virtual environment,
then runs this with that environment's interpreter, from a directory with no
module of the binding in it. Checks that the module imported is the one
installed, at <version>, with its type stubs and py.typed beside it; that
PulseEnvelope decodes the specification's raw fixture to its id and encodes
it back to its bytes, and round-trips an envelope of its own; and that a
bind_pair on loopback carries a pulse naming that envelope, with the
envelope, to another pair and back to the address it came from. Exits 1
with the check that failed. Standard library only, with Python 3.9.
"""
import json
import os
import sys
import sysconfig

import pulse_wire as pw


def main():
    version, fixture = sys.argv[1], sys.argv[2]
    site = sysconfig.get_paths()["platlib"]
    where = os.path.dirname(os.path.realpath(pw.__file__))
    assert where == os.path.realpath(site), f"pulse_wire came from {where}, not the environment's {site}"
    assert pw.__version__ == version, f"pulse_wire {pw.__version__}, expected {version}"
    for name in ("pulse_wire.pyi", "py.typed"):
        assert os.path.isfile(os.path.join(site, name)), f"{name} is not installed beside the module"

    expected = json.load(open(fixture))["expected"]
    canonical = bytes.fromhex(expected["canonical_hex"])
    decoded = pw.PulseEnvelope.from_bytes(canonical)
    assert decoded.id.hex() == expected["id_hex"], f"fixture id {decoded.id.hex()}"
    assert decoded.to_bytes() == canonical, "the fixture does not encode back to its bytes"

    env = pw.PulseEnvelope(0x0001, "smoke/hello", b"payload", flags=pw.EnvelopeFlags.BROADCAST)
    wire = env.to_bytes()
    back = pw.PulseEnvelope.from_bytes(wire)
    assert back == env and back.id == env.id, "the envelope did not round-trip"
    assert (back.type_name, back.payload, back.flags) == ("smoke/hello", b"payload", pw.EnvelopeFlags.BROADCAST)

    a_tx, a_rx = pw.bind_pair("127.0.0.1:0")
    b_tx, b_rx = pw.bind_pair("127.0.0.1:0")
    a_rx.set_read_timeout_ms(2000)
    b_rx.set_read_timeout_ms(2000)
    pulse = pw.Pulse()
    pulse.instruction = pw.Instruction.DATA
    pulse.set_envelope_id(env.id)
    a_tx.send_to(1, pulse, wire, b_rx.local_addr())
    got, envelope, meta = b_rx.recv()
    assert envelope == wire and got.envelope_id() == env.id, "the datagram arrived altered"
    assert meta.peer == a_rx.local_addr(), f"the datagram came from {meta.peer}, not the pair's {a_rx.local_addr()}"
    b_tx.send_to(2, pulse, b"pong", meta.peer)
    _, envelope, meta = a_rx.recv()
    assert envelope == b"pong" and meta.peer == b_rx.local_addr(), "the reply did not come back"

    print(
        f"ok    python {sys.version.split()[0]} {sysconfig.get_platform()}: pulse_wire {pw.__version__}, "
        f"fixture id {decoded.id.hex()}, envelope {len(wire)} bytes, bind_pair loopback both ways"
    )


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"FAIL  python {sys.version.split()[0]}: {e}")
        sys.exit(1)
