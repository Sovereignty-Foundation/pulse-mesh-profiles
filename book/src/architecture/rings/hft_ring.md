# `hft_ring`

<p class="pm-kind"><b>Reference</b> · the hft ring, crate by crate</p>

High-frequency-trading latency primitives. Downstream code should depend on `hft-hub`, not the inner crates directly. It is the one ring whose four crates are all populated; the other rings keep one or two empty placeholders.

| Crate | Layer | Contents |
|---|---|---|
| `hft-core` | core | `prefetch` — adaptive, inline-always cache-line prefetch hints. `wire_v2` — wire-format v2 layout constants with cache-line separation. |
| `hft-functions` | functions | `wait` — pluggable wait strategies (`BusySpin`/`UMWAIT`/`MWAITX`/`WFE`/`Park`) behind a `WaitStrategy` trait, with runtime CPU-feature detection and fallback; the hardware backends are opt-in Cargo features. |
| `hft-stages` | stages | `numa` — NUMA-aware hugepage SHM allocation plus thread/memory binding. `multilane` — a sharded SeqLock MPSC multiplexer built on `numa`, whose `create` returns the reader and one `LaneWriter` per lane, each publishing through `&mut`; a lane cursor the writer has lapped is moved to the oldest pulse the lane still holds and the loss is counted per lane (`lost(lane)`), and a poll that discarded a copy returns nothing for that pass, meaning retry. `spmc` — a single-producer multi-consumer ring with N independent reader cursors; a reader the writer has lapped gets `SpmcError::Lagged { skipped }` once, is moved to the oldest publish still held, never sees a publish twice, and `lost(reader_id)` counts what it missed. |
| `hft-hub` | hub | The single import point: re-exports `core`/`functions`/`stages` and registers `wait` as a function, `multilane`/`spmc` as stages, via `register_stages!`. |

If you're adding a new latency-sensitive primitive here, check `hft-core`'s `#[inline(always)]` conventions and `hft-functions::wait`'s CPU-feature-detection pattern before introducing a new dependency — this ring is latency-budget-sensitive by design.
