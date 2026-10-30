# `robotics_ring`

<p class="pm-kind"><b>Reference</b> · the robotics ring, crate by crate</p>

Large-payload delivery and adaptive transport for robotics workloads such as LIDAR and video streams: a shared-memory pool for payloads too large for a Pulse, a UDP transport over io_uring with a pool of persistent TCP flows, and ring sizing that follows receiver lag. Downstream code should depend on `robotics-hub`, not the inner crates directly.

| Crate | Layer | Contents |
|---|---|---|
| `robotics-core` | core | **Empty by design.** The ring's three primitives each carry state and live in `stages`; the crate keeps the four-crate shape uniform and is where shared core types land. |
| `robotics-functions` | functions | **Empty by design.** The ring has no stateless primitive yet; the crate keeps the four-crate shape uniform. |
| `robotics-stages` | stages | `large_payload` — an out-of-band shared-memory segment pool for multi-megabyte payloads: one process creates it (`LargePayloadPool::create`), others map it by name (`open`), any handle writes a payload into a free segment (`write`, or `alloc` and `SegmentMut::finish` to write in place) and gets back a 16-byte `SegmentRef`, which is what crosses the ring in the Pulse; the reader checks the reference and reads the payload (`read`), and frees the segment (`free`). `net_uring` — a UDP transport over io_uring, Linux only: `UringSpsc` binds one endpoint (`bind_udp`), fixes its peer (`connect_udp`) and sends and receives (`send`, `send_batch`, `recv`, `recv_timeout`), with kernel-side submission polling where the kernel allows it, and `FlowPool` keeps one persistent TCP flow per peer; for AF_XDP, see the `pulse-wire-xdp` plugin. `autoscale` — ring sizing driven by observed receiver lag: a `LagObserver` decides from the write and read cursors when the next ring should grow or shrink within `AutoscaleConfig`'s bounds, and a `ResizingRing` applies it by replacing the ring under its name through the ring's superseded mark (ADR-0015), which a `FollowingReader` follows. |
| `robotics-hub` | hub | The ring's single import point; re-exports `core`/`functions`/`stages`. Functions list is empty, so it registers only the three stages: `large_payload`, `net_uring`, `autoscale`. |

`net_uring` sends and receives the datagrams its caller hands it, over the kernel's stack; it does not frame them. For Pulse frames past the kernel's stack, see [The AF_XDP transport](../../reference/xdp-transport.md), the `pulse-wire-xdp` crate in `plugins/hft-xdp`, whose frames interoperate with `pulse_wire_udp`'s `Sender` and `Receiver`.
