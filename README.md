# Pulse Mesh

**The transport protocol for the intelligence era.**

**This repository:** `pulse-mesh-profiles`, ready-made rings for enterprise, HFT and robotics.

Pulse Mesh moves messages between programs: inside one machine through shared memory, and between machines over UDP. Use the five-minute onboarding below to put it into robotics, agent swarms, high-frequency trading or distributed mesh compute.

## Unlike typical transport layers, Pulse Mesh gives you

- **Broadcast, listen and fan-out, built in.** One writer, many readers, with no lock between them. By default the writer never waits for a reader, so a reader that falls a whole ring behind loses the oldest messages and is told how many, never handed one twice ([readers that fall behind](https://pulse-mesh.net/docs/reference/spec/04-ring.html#readers-that-fall-behind)); where every message matters, a blocking publish keeps the slowest registered reader's pace instead. Through the daemon, a subscription receives what is sent after it subscribes and no history, and a subscriber that falls behind has its newer events dropped and counted while the others still get them ([the daemon's queue limits](https://pulse-mesh.net/docs/daemon/reference/daemon-reference.html)).
- **One fixed wire format across every language.** Every message is signalled by the same 128-byte Pulse with one canonical encoding, so Rust, C, Python, Node and the browser read the same bytes.
- **Messages you can audit.** Content is named by a hash of itself, and the encoding is pinned by conformance vectors, so any receiver in any language can reproduce exactly what was sent and check it.
- **Take only what you need.** The codec alone, the ring and the UDP transport, a daemon that routes between programs, or a ready-made ring for your domain.

## Built for

- **Robotics:** large sensor payloads through a shared-memory pool, and a UDP transport over io_uring.
- **Agent swarms:** one daemon every language attaches to, and an MCP server that gives a coding agent the mesh directly.
- **High-frequency trading:** NUMA placement, striped rings, and hardware wait instructions in place of spinning.
- **Distributed mesh compute:** UDP between hosts, multicast, and fan-out to many readers.

## What's in the repositories

| Repository | Includes |
|---|---|
| `pulse-mesh`, the standard | The Pulse and envelope codec; conformance vectors; shared-memory ring and store; UDP transport; the `pulse` command line; C, Python, Node and WebAssembly bindings |
| `pulse-mesh-daemon` | The `mesh-hub` daemon; Rust client; `pms` command line; MCP server; Python and Node thin clients |
| `pulse-mesh-profiles` | Enterprise, HFT and robotics rings; AF_XDP transport for Linux |

## Get started in five minutes

**With a coding agent,** paste this:

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

**By hand,** follow [Developer onboarding](https://pulse-mesh.net/docs/contributing/onboarding.html).

## Documentation

The documentation lives at [pulse-mesh.net/docs](https://pulse-mesh.net/docs/), one book per repository, each with examples and workflows that an agent can run and check:

- [The standard](https://pulse-mesh.net/docs/): the Pulse and the envelope, the ring and the store, the UDP transport, the specification and the decisions behind it.
- [The daemon](https://pulse-mesh.net/docs/daemon/): `mesh-hub`, its clients and command line, the MCP server, and links between daemons.
- [The profiles](https://pulse-mesh.net/docs/profiles/): the enterprise, HFT and robotics rings and the AF_XDP transport.

Start at [Onboard your agent](https://pulse-mesh.net/docs/start/onboard-your-agent.html). For an agent, [pulse-mesh.net/llms.txt](https://pulse-mesh.net/llms.txt) lists every page. The same pages are in this repository under `book/src/`, and `mdbook serve book` renders them.

## Status

This repository is pre-production and currently in beta.

- **Join the community** at [theproving.net](https://theproving.net): post your own benchmarks, or refute ours.
- **Early contributors** can apply for pre-release licenses for commercial deployment.
- **The working group** opens in November.

See [pulse-mesh.net](https://pulse-mesh.net) for more information, or find us at [sovfound.org](https://sovfound.org) for certifications and working group information.

## License

Proprietary. All rights reserved. See [`LICENSE`](LICENSE).
