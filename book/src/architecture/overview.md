# Workspace Layout

<p class="pm-kind"><b>Reference</b> · the three rings and the leaves beside them, for anyone finding their way around the tree</p>

This repository holds three tier-3 rings of the Pulse Mesh stack. A tier depends only on the tiers below it: everything here depends on [the standard](/docs/architecture/overview.html), tiers 0 and 1, which is a sibling checkout (`../pulse-mesh`) while developing and a pinned tag at a release. Nothing here depends on the daemon, and nothing in the standard depends on anything here.

| Tier | Path | What lives here |
|---|---|---|
| 3 — rings | `rings/` | `enterprise_ring` (the TLS transport stage; TLS over TCP, with DTLS and the QUIC tunnel deferred), `hft_ring` (wait strategies, the MultiLane ring, the SPMC ring, NUMA placement) and `robotics_ring` (the large-payload pool, the io_uring transport and the autoscale stage), each four crates: `<name>-core`, `<name>-functions`, `<name>-stages`, `<name>-hub` |

Beside the rings sit the leaves: they consume the tiers and are consumed by nothing.

| Path | What lives here |
|---|---|
| `plugins/` | `hft-xdp` (crate `pulse-wire-xdp`): the UDP transport's frames over AF_XDP on Linux, a sender into a NIC queue's transmit ring and a receiver whose XDP program hands it the queue's packets for its port; every constructor returns `PlatformUnsupported` off Linux. [The AF_XDP transport](../reference/xdp-transport.md) is its reference. |
| `tools/` | `hft-telemetry`, the HFT ring's telemetry tool |
| `examples/` | one program per ring that has a runnable stage: the enterprise ring's TLS stage and the robotics ring's large-payload stage |
| `modules.toml` | the registry for satellite repositories and their maturity tier: the field definitions today, no entries |

## The four-crate ring pattern

Each ring under `rings/<name>_ring/` is decomposed into four crates:

- `<name>-core` — shared types and primitives, no state and no I/O.
- `<name>-functions` — stateless transformations over the core types.
- `<name>-stages` — stateful, hot-path primitives with invariants.
- `<name>-hub` — the composition surface; it re-exports the inner layers and registers them via the `register_stages!` macro, which the standard holds in `core/mesh-stage-registry`.

Downstream consumers depend on a ring's `*-hub` crate, not on its inner layers directly. A `core` or `functions` crate may be an empty placeholder when a ring's primitives all classify as stages — it's kept so the shape is uniform across rings. Two of the three rings here (`enterprise_ring`, `robotics_ring`) have empty `core`/`functions` layers for exactly this reason; see their chapters under [Rings: domain surfaces](rings/overview.md). The fourth ring, the dev ring, keeps the same shape [in the daemon's repository](/docs/daemon/architecture/rings/dev_ring.html).

## Deciding where new code goes

`CONTRIBUTING.md` frames this as a four-question test: does the code touch the canonical Pulse atom, touch the canonical envelope, cross a ring boundary, or sit on the hot path? Code that touches the atom or the envelope belongs in the standard, and is made there first. A stage that crosses a ring boundary, or a hot-path primitive that belongs to one domain, goes in that ring, routed to `core`, `functions`, `stages` or `hub` by its role. Anything that only uses the tiers (a tool, a plugin, an example) is a leaf here. Record any non-obvious classification call as a new ADR under `decisions/` rather than only justifying it in the PR.
