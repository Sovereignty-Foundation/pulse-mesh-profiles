# Developer Onboarding

<p class="pm-kind"><b>Start here</b> · five minutes from a fresh checkout to two rings you have watched run, by hand</p>

Working with a coding agent? Start with [Onboard your agent](../start/onboard-your-agent.md): one prompt sets up all three repositories. This page is the same setup by hand.

## By hand

1. **Set up.** Check out the standard at `../pulse-mesh`; every manifest here reaches it there. `rust-toolchain.toml` pins the Rust version, and `rustup` installs it on the first `cargo` command.
2. **See two rings run.**
   ```bash
   cargo run --release -p example-robotics-01-large-payload-rust
   cargo run --release -p example-enterprise-01-tls-stage-rust
   ```
   The first passes a large payload through shared memory. The second sends one frame over TLS 1.3 on loopback, then asks for the DTLS mode, which this release defers, and prints what the ring says.
3. **Read the shape.** [Rings: domain surfaces](../architecture/rings/overview.md) explains the four-crate pattern, and each ring's page says what its example shows.
4. **Prove the tree.** `scripts/gate.sh --quick` runs the per-commit checks; `scripts/gate.sh` runs the gate a change passes before it merges. A check whose tool is missing fails and prints the command that installs it. Some code runs only on Linux; on another host, `just linux-verify` runs a cargo command in a Linux container.

## Before your first change

- `CONTRIBUTING.md` says what a contribution means during the beta, holds the table of every check the gate runs, and gives the four-question test for where new code goes, which [Workspace Layout](../architecture/overview.md) explains.
- `AGENTS.md` lists the rules every change keeps, for a person as much as for an agent: every ring keeps four crates and is reached only through its hub; a partial crate keeps its label until it works; Linux-only code is tested on Linux; decision records are added and never rewritten.
- The specification is normative, the rustdoc is the API contract, and the [Decision log](../explanation/decision-log.md) records why the non-obvious calls were made. This book explains and guides; where it and the code disagree, one of them is wrong.

## Where to read first

- `rings/hft_ring/hft-stages/src/multilane/` and `src/spmc/`: the multilane rings and the SPMC ring, the two hot-path structures with the most in them.
- `rings/hft_ring/hft-functions/src/wait/`: the wait strategies, from busy-spin to the hardware monitor waits behind features.
- `rings/enterprise_ring/enterprise-stages/src/tls/`: the TLS transport stage, and what a partial stage's deferred modes look like.
- `rings/robotics_ring/robotics-stages/src/large_payload/`: a stage that moves what does not fit a Pulse.
- `plugins/hft-xdp/`: the UDP transport's frames over AF_XDP on Linux.
