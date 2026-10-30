# Working in this repository

This file is for a coding agent opening the Pulse Mesh profiles for the first time, and for the person working with it. Read it whole before changing anything; it is short so that you can.

## What this repository is

Three domain rings built on the Pulse Mesh standard: enterprise, HFT and robotics, each the same four crates (`<ring>-core`, `-functions`, `-stages`, `-hub`), plus the AF_XDP transport, the HFT telemetry tool and one example per runnable ring. The standard, the repository `pulse-mesh`, holds the Pulse, the envelope, their codec, the ring, the store and the UDP transport. Nothing here depends on the daemon, which holds the fourth ring, the dev ring.

## Set up

- Check out the standard beside this repository, so that `../pulse-mesh` exists: every manifest here points at it. Without it nothing builds.
- Rust: `rust-toolchain.toml` pins the version, and `rustup` installs it on the first `cargo` command.
- The gate also uses `cargo-deny`, `mdbook` and `python3`. A check whose tool is missing fails and prints the command that installs it, so install what it names and run it again.
- Some code runs only on Linux: the AF_XDP transport, the io_uring transport and the NUMA placement. On another host, `just linux-verify` runs a cargo command in a Linux container against this checkout.
- `just` is optional: every recipe in `justfile` is a thin wrapper, and `scripts/gate.sh` runs the gate without it.

## Prove the tree works, in this order

1. `cargo run --release -p example-robotics-01-large-payload-rust` runs the robotics ring's large-payload stage.
2. `scripts/gate.sh --quick` runs the per-commit checks.
3. `scripts/gate.sh` runs the gate a change passes before it merges. `--full` adds the Linux and x86 legs.

The gate prints one line per check and a summary, and writes each check's output to `target/gate/<check>.log`. A change is done when the gate passes, not when the tests you chose pass.

## Map

| Path | What it holds |
|---|---|
| `rings/enterprise_ring` | The enterprise ring: the wire transport's frames over TLS 1.3 on TCP |
| `rings/hft_ring` | The HFT ring: NUMA placement, the striped multilane rings and the SPMC ring |
| `rings/robotics_ring` | The robotics ring: the shared-memory large-payload pool and the io_uring transport |
| `plugins/hft-xdp` | `pulse-wire-xdp`, the UDP transport's frames over AF_XDP on Linux |
| `tools/hft-telemetry` | The HFT ring's telemetry |
| `examples/` | One program per ring that has a runnable stage |
| `modules.toml` | The registry for satellite repositories |
| `book/` | Each ring crate by crate, and the explanation of the rings' shape; rendered at <https://pulse-mesh.net/docs/profiles/>, every page listed in <https://pulse-mesh.net/llms.txt> |
| `decisions/` | The architectural decision records |

## What is true, in order of authority

1. The specification, in the standard's book. A MUST is normative.
2. The standard's test vectors.
3. The rustdoc, the API contract: `cargo doc --workspace --no-deps --open`.
4. The decision records in `decisions/`, for why a call was made.
5. This repository's book, which explains and guides.

When two of them disagree, one is wrong. Report it; do not pick the one that suits the change.

## Rules that hold for every change

- Every ring keeps the four-crate shape, and code outside a ring reaches it only through its `-hub` crate. `scripts/arch-check.py`, in the gate, enforces both.
- A stage is declared in its ring's hub with `register_stages!` and shows up in the hub's manifest test; adding one follows the guide *Add a stage to a ring*.
- A change to the standard belongs in the standard's repository. Do not patch `../pulse-mesh` from here.
- `enterprise-stages` is marked `status = "partial"`: its TLS-over-TCP mode works and its DTLS and QUIC-tunnel modes return deferred errors on purpose (ADR-0017). Leave the label until they work; never make a deferred path look implemented.
- Test Linux-only code on Linux, not by reasoning about it: run it through `just linux-verify` or the gate's `--full` legs.
- `unsafe` is allowed only with a `// SAFETY:` comment on the block. A crate with no `unsafe` forbids it at its root, and keeps doing so.
- Every public item carries a doc comment, and the gate treats warnings as errors.
- Decision records are append-only: add a record, never rewrite an accepted one.
- No measured figures in docs, comments or read-mes. A guide gives the command; the reader measures on their own machine.
- No links to a source-code host or server. Book pages link to each other relatively; links that leave the repository go to `https://pulse-mesh.net/docs/`. `scripts/docs/check-hosts.py` enforces this in the gate.
- Leave `LICENSE`, `STATUS.md`, the notices and the file headers exactly as they are.

## Making a change

Follow `CONTRIBUTING.md`: its gate table is the list of checks, and its four-question test says where new code goes. Code contributions are closed until the Proving Grounds' full launch, expected in the fourth quarter of 2026, and are then routed through it, so for now a change you make is for the person you work with, and for a report of what did not hold. Put tests beside the code they cover. If the change is visible to someone using a ring, update that ring's page in the book and its read-me in the same change. If you made a call a later contributor would have to reverse-engineer, add a decision record. Then run the gate.

## Where to look

| Question | Read |
|---|---|
| Why does every ring have four crates? | `book/src/architecture/rings/overview.md` |
| What does a ring hold, crate by crate? | Its page under `book/src/architecture/rings/` |
| How do I add a stage? | The guide *Add a stage to a ring*, in the daemon's book |
| What does an error mean? | `book/src/reference/errors.md` |

## Reporting back

When you hand work back, say what changed and why, paste the gate's summary line, and name anything you skipped or could not run.
