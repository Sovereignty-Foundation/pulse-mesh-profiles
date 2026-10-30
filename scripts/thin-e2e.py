#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""The two thin clients against a real daemon.

    scripts/thin-e2e.py <mesh-hub binary>

Starts the daemon on endpoints of its own: a socket in a new temporary
directory, a WebSocket port the kernel picks (the daemon logs it) and the
hub name e2e-<pid>, within the 15 characters a hub name may have. The
Python thin client attaches and subscribes to one channel; the Node thin
client (scripts/thin-e2e.js, run by node) attaches, subscribes to another
and sends three envelopes on the first; once the Node client has
subscribed, the Python client sends three on the second. Each side checks
what it received against what the other sent: the payload bytes (binary,
so base64 must carry every byte value), the channel, the type name, the
sender's name in the origin, and the envelope identifier the sender's send
returned. Then both detach, and the daemon must exit 0 on SIGTERM and
remove its socket. Exits 1 naming what went wrong, with the end of the
daemon's log.

Needs websocket-client for python3 and ws installed in the thin Node
client (the gate's thin-e2e check says how to install either). The
suites under the clients' own directories stub the transport; this is
the one run of either against a daemon.
"""
import os
import queue
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
NODE_HALF = REPO / "scripts" / "thin-e2e.js"
COUNT = 3
WAIT = 20.0


class Failed(Exception):
    pass


def payload_from_python(i):
    return bytes([0x00, 0xFF, i]) + "python to node, é".encode("utf-8")


def plain(text):
    """`text` without the colour codes the daemon's log carries."""
    return re.sub(r"\x1b\[[0-9;]*[A-Za-z]", "", text)


def start_daemon(binary, work, hub):
    """The daemon on work's socket and a port the kernel picks: its process,
    its log file and the port."""
    log_path = work / "hub.log"
    sock = work / "hub.sock"
    with open(log_path, "wb") as log:
        daemon = subprocess.Popen(
            [binary, "--hub-name", hub, "--uds-path", str(sock), "--ws-bind", "127.0.0.1:0"],
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
    deadline = time.monotonic() + WAIT
    while time.monotonic() < deadline:
        text = plain(log_path.read_text(errors="replace"))
        m = re.search(r"websocket listening.*?127\.0\.0\.1:(\d+)", text)
        if m:
            return daemon, log_path, int(m.group(1))
        if daemon.poll() is not None:
            raise Failed(f"the daemon exited with status {daemon.returncode} before it listened")
        time.sleep(0.05)
    raise Failed(f"the daemon did not report its WebSocket within {WAIT:.0f} s")


def lines_of(stream, into):
    """Hand each line of `stream` to the queue `into`, then None."""
    for line in stream:
        into.put(line.rstrip("\n"))
    into.put(None)


def next_line(lines, deadline, what):
    try:
        line = lines.get(timeout=max(0.0, deadline - time.monotonic()))
    except queue.Empty:
        raise Failed(f"the Node client did not {what} within {WAIT:.0f} s") from None
    if line is None:
        raise Failed(f"the Node client ended before it could {what}")
    if line.startswith("error "):
        raise Failed(f"the Node client failed: {line[6:]}")
    return line


def exchange(pms, env, to_py, to_node):
    """Both directions, checked: how many envelopes went from Node to
    Python, and from Python to Node."""
    got = queue.Queue()
    py = pms.attach("e2e-py")
    try:
        py.on(to_py, got.put)
        node = subprocess.Popen(
            ["node", str(NODE_HALF), to_py, to_node, str(COUNT)],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            lines = queue.Queue()
            threading.Thread(target=lines_of, args=(node.stdout, lines), daemon=True).start()
            deadline = time.monotonic() + WAIT
            while next_line(lines, deadline, "subscribe") != "subscribed":
                pass
            sent_py = []
            for i in range(COUNT):
                payload = payload_from_python(i)
                sent_py.append((py.send(to_node, payload, type_name="e2e.from-python"), payload))
            sent_node, got_node = [], []
            while len(sent_node) < COUNT or len(got_node) < COUNT:
                kind, *fields = next_line(lines, deadline, "send and receive").split(" ")
                (sent_node if kind == "sent" else got_node).append(fields)
            try:
                status = node.wait(timeout=max(0.1, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                raise Failed("the Node client did not exit once done") from None
            if status != 0:
                raise Failed(f"the Node client exited with status {status}")
        finally:
            if node.poll() is None:
                node.kill()
                node.wait()
        received = []
        while len(received) < COUNT:
            try:
                received.append(got.get(timeout=max(0.0, deadline - time.monotonic())))
            except queue.Empty:
                raise Failed(f"the Python client received {len(received)} of {COUNT} envelopes") from None
    finally:
        py.detach()

    # Node to Python: the events the Python client received.
    want = [(eid, bytes.fromhex(hexed)) for eid, hexed in sent_node]
    have = [(e.envelope_id, e.payload) for e in received]
    if have != want:
        raise Failed(f"node to python: sent {want}, received {have}")
    for e in received:
        if (e.channel, e.type_name) != (to_py, "e2e.from-node") or not e.origin.startswith("e2e-node@"):
            raise Failed(f"node to python: an event came as {e}")
    # Python to Node: the events the Node client printed.
    want = [(eid, payload.hex(), to_node, "e2e.from-python") for eid, payload in sent_py]
    have = [(eid, hexed, channel, type_name) for eid, channel, type_name, _origin, hexed in got_node]
    if have != want:
        raise Failed(f"python to node: sent {want}, received {have}")
    if not all(origin.startswith("e2e-py@") for _, _, _, origin, _ in got_node):
        raise Failed(f"python to node: an origin is not the Python client's: {got_node}")
    return len(received), len(got_node)


def main(argv):
    if len(argv) != 1:
        sys.exit("usage: scripts/thin-e2e.py <mesh-hub binary>")
    # The client: under peripheral/bindings/dev-thin in the integrated tree, under clients/ in the daemon's own
    # repository.
    client = next(d for d in (REPO / "peripheral/bindings/dev-thin/pms-py-thin", REPO / "clients/pms-py-thin") if d.is_dir())
    sys.path.insert(0, str(client))
    import pms

    hub = f"e2e-{os.getpid()}"
    # A socket path holds at most 103 bytes on macOS.
    base = tempfile.gettempdir()
    work = Path(tempfile.mkdtemp(prefix="pms-e2e.", dir=base if len(base) < 60 else "/tmp"))
    daemon = log_path = None
    try:
        daemon, log_path, port = start_daemon(argv[0], work, hub)
        url = f"ws://127.0.0.1:{port}"
        os.environ["PMS_DAEMON"] = url
        print(f"thin-e2e: daemon on hub {hub}, socket {work / 'hub.sock'}, {url}")
        channel = f"e2e/{os.getpid()}"
        n2p, p2n = exchange(pms, dict(os.environ), f"{channel}/to-py", f"{channel}/to-node")
        print(f"thin-e2e: node to python {n2p} of {COUNT}, python to node {p2n} of {COUNT}")
        daemon.send_signal(signal.SIGTERM)
        try:
            status = daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            raise Failed("the daemon did not exit within 10 s of SIGTERM") from None
        if status != 0:
            raise Failed(f"the daemon exited with status {status} on SIGTERM")
        if (work / "hub.sock").exists():
            raise Failed("the daemon left its socket behind")
        print("thin-e2e: ok")
        return 0
    except Failed as e:
        print(f"thin-e2e: FAILED: {e}")
        if log_path is not None:
            print("thin-e2e: the daemon's log ends:")
            print("".join(plain(log_path.read_text(errors="replace")).splitlines(keepends=True)[-20:]), end="")
        return 1
    finally:
        if daemon is not None and daemon.poll() is None:
            daemon.kill()
            daemon.wait()
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
