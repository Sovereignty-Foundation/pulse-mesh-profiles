# Onboard your agent

<p class="pm-kind"><b>Start here</b> · what a coding agent needs to set up Pulse Mesh, run it, and put other agents on the mesh</p>

Pulse Mesh is built to be set up and driven by a coding agent. This page is what you hand it: one prompt to get running, what it needs to know before it changes anything, the MCP server that gives it a live mesh, and how agents talk to each other through one.

## 1. Paste this

Open an empty folder in your agent and paste:

```text
Set up Pulse Mesh for me. In one empty folder, clone these three repositories side by side, from the GitHub account that publishes Pulse Mesh (the account this repository belongs to; if you are already inside one of them, its git remote names it):
  pulse-mesh
  pulse-mesh-daemon
  pulse-mesh-profiles
Install Rust with rustup if it is missing; each repository pins its version in rust-toolchain.toml.
Then in each repository read AGENTS.md and run its "Prove the tree works" steps up to scripts/gate.sh --quick.
Finally show me it working:
  1. in pulse-mesh, encode, decode and verify one envelope with the pulse command line;
  2. in pulse-mesh-daemon, run pms demo;
  3. in pulse-mesh-profiles, run the robotics example.
Explain in plain words what each step showed. Stop and tell me if anything fails.
```

## 2. What your agent needs to know

- **Three repositories, side by side.** `pulse-mesh` is the standard; `pulse-mesh-daemon` and `pulse-mesh-profiles` build on it and find it at `../pulse-mesh`.
- **`AGENTS.md` in each one** is the agent's brief: setup, the order of the first commands, a map of the tree, and the rules every change keeps.
- **What is true, in order:** the specification, then the test vectors and the port protocol's fixtures, then the rustdoc, then the decision records, then the rest of this book. When two disagree, one is wrong, and the agent should say so rather than pick one.
- **Done means the gate passes.** `scripts/gate.sh` in each repository is the check a change passes before it merges.
- **What it must never do:** edit a test vector or a fixture to make a test pass, make a deferred feature look implemented, or rewrite an accepted decision record.

## 3. Give your agent the mesh: MCP

`pms-mcp`, in the daemon's repository, is a Model Context Protocol server. With a daemon running, it lets an agent attach to the mesh, send envelopes, check health, query and replay what the basket captured, read a ring's slots, and round-trip the codec on input it supplies.

```bash
cargo install --path tools/pms-mcp --locked   # from pulse-mesh-daemon
claude mcp add pms-mcp -- pms-mcp             # Claude Code; any MCP client that can launch a command works
```

[Use the MCP server from an agent](/docs/daemon/guides/mcp-with-an-agent.html) registers it with other clients and tries it by hand, and [the MCP tools](/docs/daemon/reference/mcp-tools.html) lists every tool's input and output.

## 4. Put agents on the mesh

Agents coordinate the way any programs on the mesh do: each attaches to the daemon by name, subscribes to the channels it cares about, and sends envelopes that reach every subscriber attached at that moment. One agent can plan and others can work:

```bash
pms start --foreground                   # one daemon for the machine
pms tap --channel 'jobs/*'               # a worker listens for jobs
pms push jobs/new 'summarise report.pdf' # a planner hands one out
```

An agent with MCP does the same through `mesh_attach` and `mesh_send`. One without it speaks the [port protocol](/docs/daemon/reference/spec/07-port-protocol.html) directly over the daemon's WebSocket, through the Python or Node thin client or plain JSON frames. Every envelope is named by a hash of its content, so a receiving agent can check it got exactly what was sent. [Two programs talk through the daemon](/docs/daemon/examples/two-programs.html) walks through it by hand.

Keep the daemon on loopback, which is its default. There its Unix socket admits only its own user's processes, but its WebSocket is plaintext and any user on the machine can connect to it, and any attached agent may publish and subscribe on any channel. On a machine other users share, add `--attach-keys`, so that a WebSocket client must prove an Ed25519 key, which today only the Rust client does, or `--no-ws` to serve the Unix socket alone. Off loopback the daemon refuses its WebSocket without TLS (`--ws-tls-cert`, `--ws-tls-key`) and attach keys together, unless `--ws-allow-remote` says the gap is meant; [Trust boundaries](/docs/daemon/explanation/trust-boundaries.html) says what each protects, and that no layer authorises per channel.

[An agent swarm](/docs/daemon/examples/agent-swarm.html) builds this in ten minutes: a planner hands out jobs, two workers answer, and every result is checked against its job by hash.

## Next

- [Developer onboarding](../contributing/onboarding.md): the same setup by hand, in five minutes.
- [Use the MCP server from an agent](/docs/daemon/guides/mcp-with-an-agent.html): every client, and the server's limits.
