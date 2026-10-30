# Rings: domain surfaces

<p class="pm-kind"><b>Reference</b> · the four-crate ring pattern and the three rings here</p>

Domain-specific surfaces composed on the standard's substrate. Each ring follows the four-crate pattern introduced in [Workspace Layout](../overview.md#the-four-crate-ring-pattern):

- `<name>-core` — shared types/primitives, no state, no I/O.
- `<name>-functions` — stateless transformations over the core types.
- `<name>-stages` — stateful, hot-path primitives with invariants.
- `<name>-hub` — the composition surface; re-exports the inner layers and registers them via `mesh_stage_registry::register_stages!`, the macro the standard holds.

Downstream consumers depend on a ring's `*-hub` crate, never its inner layers directly, and in the tree they do: `hft-telemetry` reaches the HFT ring through `hft-hub`, and the enterprise and robotics hubs each have an example built against them alone (`examples/enterprise-01-tls-stage-rust`, `examples/robotics-01-large-payload-rust`). In the daemon's repository, `pms` and `pms-mcp` reach the dev ring the same way, through `dev-hub`.

| Ring | Domain | Core/functions populated? |
|---|---|---|
| [`enterprise_ring`](enterprise_ring.md) | The TLS transport: TLS 1.3 over TCP, with DTLS and the QUIC tunnel deferred. | Both empty placeholders — everything is a stage. |
| [`hft_ring`](hft_ring.md) | High-frequency-trading latency primitives: cache-line prefetch, wait strategies, NUMA-aware SHM, multilane/SPMC rings. | Both populated. |
| [`robotics_ring`](robotics_ring.md) | Large-payload delivery through a shared-memory pool, a UDP transport over io_uring, and ring sizing that follows receiver lag. | Both empty placeholders — everything is a stage. |

The fourth ring, [`dev_ring`](/docs/daemon/architecture/rings/dev_ring.html), keeps the same shape in the daemon's repository, beside the daemon it serves: the ring inspector and the module table in its functions layer, the conflation router in its stages, the Rust client re-exported by its hub.

The next three chapters cover each ring's crates in detail.
