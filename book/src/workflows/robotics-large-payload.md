# Robotics: a 4 MiB scan through shared memory, and the Pulse carries the reference

<p class="pm-kind"><b>Workflow</b> · for a developer moving LIDAR scans, camera frames or anything else too large for a Pulse between processes on one machine; about fifteen minutes, in Rust, with the profiles repository beside the standard</p>

<figure class="pm-art"><svg class="uc-diagram" viewBox="0 0 380 240" role="img" aria-label="The payload in a pool, its reference through the ring"><rect x="150" y="30" width="44" height="180" rx="3" class="uc-ring"/><text x="172" y="228" class="uc-label">pool + ring</text><circle cx="50" cy="60" r="12" class="uc-node-strong"/><text x="50" y="86" class="uc-label">lidar</text><line x1="62" y1="60" x2="150" y2="60" class="uc-edge"/><circle cx="50" cy="120" r="12" class="uc-node-strong"/><text x="50" y="146" class="uc-label">camera</text><line x1="62" y1="120" x2="150" y2="120" class="uc-edge"/><circle cx="50" cy="180" r="12" class="uc-node-strong"/><text x="50" y="206" class="uc-label">imu</text><line x1="62" y1="180" x2="150" y2="180" class="uc-edge"/><line x1="194" y1="50" x2="262" y2="50" class="uc-edge"/><rect x="262" y="41" width="18" height="18" rx="2" class="uc-node"/><text x="288" y="54" class="uc-label uc-label-left">planning</text><line x1="194" y1="100" x2="262" y2="100" class="uc-edge"/><rect x="262" y="91" width="18" height="18" rx="2" class="uc-node"/><text x="288" y="104" class="uc-label uc-label-left">perception</text><line x1="194" y1="150" x2="262" y2="150" class="uc-edge"/><rect x="262" y="141" width="18" height="18" rx="2" class="uc-node"/><text x="288" y="154" class="uc-label uc-label-left">safety</text><line x1="194" y1="200" x2="262" y2="200" class="uc-edge"/><rect x="262" y="191" width="18" height="18" rx="2" class="uc-node"/><text x="288" y="204" class="uc-label uc-label-left">logging</text><circle class="uc-pulse" r="2.5"><animateMotion dur="1.4s" begin="0.0s" repeatCount="indefinite" path="M62,60 L150,60"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.4s" begin="0.35s" repeatCount="indefinite" path="M62,120 L150,120"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.4s" begin="0.7s" repeatCount="indefinite" path="M62,180 L150,180"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.4s" begin="0.7s" repeatCount="indefinite" path="M194,50 L262,50"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.4s" begin="0.75s" repeatCount="indefinite" path="M194,100 L262,100"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.4s" begin="0.7999999999999999s" repeatCount="indefinite" path="M194,150 L262,150"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.4s" begin="0.85s" repeatCount="indefinite" path="M194,200 L262,200"/></circle></svg><figcaption>The payload in a pool, its reference through the ring</figcaption></figure>

**You will have:** a 4 MiB scan written into a shared-memory pool by one process and read, checked and freed by another, with only a 16-byte reference crossing between them in a Pulse; the pool's limits seen rather than read about (a ninth payload on eight segments, a payload larger than a segment, a reference to a freed segment); and the path to the ring's io_uring transport and lag-driven ring sizing on Linux.

**You need:** `pulse-mesh` and `pulse-mesh-profiles` side by side, as [Start here](../start/onboard-your-agent.md) describes, and the Rust toolchain. Linux or macOS: the pool is POSIX shared memory. The io_uring transport needs Linux.

## Hand this to your agent

```text
Run the workflow "Robotics: a 4 MiB scan through shared memory" from book/src/workflows/robotics-large-payload.md in the pulse-mesh-profiles repository:
1. Run cargo run --release -p example-robotics-01-large-payload-rust and show me its output. Expect a "robotics ring manifest" line, a writer line saying it wrote 4194304 bytes to segment 0x0 and that the Pulse carries 16 bytes of reference, a reader line from a different process id saying it read 4194304 bytes and that the CRC-32C 0xe4cdb1f8 checks, "reader: freed the segment", "writer: 0 segments in use after the reader", "writer: the freed reference 0x0 is refused", and a last line saying the next scan reused segment 0 at generation 1.
2. Read examples/robotics-01-large-payload-rust/src/main.rs and explain in plain words how the scan crossed processes without being copied through the ring, and what the reference's generation is for.
3. Beside the two repositories, make a crate (cargo new pool-limits) depending on robotics-hub = { path = "../pulse-mesh-profiles/rings/robotics_ring/robotics-hub" }, save the limits program from the page as src/main.rs, and run it. Expect the ninth write to fail with "pool exhausted: 8/8 segments in flight", the oversize write to fail with "payload too large", "held after freeing: 0", and the read of a freed reference to fail with "invalid segment id 0x0".
Stop and tell me if anything fails, and say which step.
```

## Or do it by hand

### 1. Run the example

From the repository root:

```bash
cargo run --release -p example-robotics-01-large-payload-rust
```

```text
robotics ring manifest: [("functions", []), ("stages", ["robotics_stages::large_payload", "robotics_stages::net_uring", "robotics_stages::autoscale"])]
writer (pid …): wrote 4194304 bytes to segment 0x0; the Pulse carries 16 bytes of reference
reader (pid …): read 4194304 bytes from segment 0x0, CRC-32C 0xe4cdb1f8 checks
reader: freed the segment
writer: 0 segments in use after the reader
writer: the freed reference 0x0 is refused
writer: the next scan reused segment 0 at generation 1 (id 0x100000000)
```

Two process ids: the program writes the scan, then runs itself again as the reader, which maps the pool by name. The scan is synthetic, so its CRC-32C is the same on every run.

### 2. Read the program

The reader and the writer, from the example:

```rust
{{#include ../../../examples/robotics-01-large-payload-rust/src/main.rs:46:}}
```

The writer creates a `LargePayloadPool` from a `PoolConfig` (here one 4 MiB segment, under a name of its own), writes the scan with `write` and gets back a `SegmentRef`: sixteen bytes naming the segment and its generation, the payload's length and its CRC-32C. `pack_segment_ref` puts those sixteen bytes into a Pulse's data area, which is all a ring would carry. The reader opens the pool by name, takes the reference out of the Pulse with `unpack_segment_ref`, and `read` copies the payload out once the segment is held under the reference's generation and the CRC matches. The reader frees the segment; the writer sees it free, finds the old reference refused, and the next scan takes the same segment at generation 1.

### 3. See the limits

Beside the repositories, `cargo new pool-limits`, with `robotics-hub = { path = "../pulse-mesh-profiles/rings/robotics_ring/robotics-hub" }` under `[dependencies]`, and this as `src/main.rs`:

```rust
use robotics_hub::stages::large_payload::{LargePayloadError, LargePayloadPool, PoolConfig};

fn main() {
    let mut cfg = PoolConfig::default();
    cfg.name = format!("/pms_lp_limits_{}", std::process::id());
    println!("default pool: {} segments of {} bytes", cfg.segment_count, cfg.segment_bytes);
    let pool = LargePayloadPool::create(cfg).expect("create");

    let payload = vec![7u8; 4 << 20];
    let mut refs = Vec::new();
    for i in 0..9 {
        match pool.write(&payload) {
            Ok(r) => refs.push(r),
            Err(e) => {
                println!("write {}: {e} (Exhausted: {})", i + 1, matches!(e, LargePayloadError::Exhausted { .. }));
                break;
            }
        }
    }
    println!("held after the loop: {}", pool.in_use());

    let big = vec![1u8; (4 << 20) + 1];
    if let Err(e) = pool.write(&big) {
        println!("oversize write: {e}");
    }

    for r in &refs {
        pool.free(r.id).expect("free");
    }
    println!("held after freeing: {}", pool.in_use());

    let mut out = Vec::new();
    if let Err(e) = pool.read(&refs[0], &mut out) {
        println!("read of a freed reference: {e}");
    }
}
```

```bash
cargo run --release
```

```text
default pool: 8 segments of 4194304 bytes
write 9: pool exhausted: 8/8 segments in flight (Exhausted: true)
held after the loop: 8
oversize write: payload too large: 4194305 bytes > segment size 4194304
held after freeing: 0
read of a freed reference: invalid segment id 0x0
```

## Check it worked

| Check | Command | Pass |
|---|---|---|
| The scan crossed processes and checked | `cargo run --release -p example-robotics-01-large-payload-rust` | Two different pids; `CRC-32C 0xe4cdb1f8 checks`; `0 segments in use after the reader` |
| A freed reference is refused, and the segment is reused with a new generation | Same run | `the freed reference 0x0 is refused`; `reused segment 0 at generation 1 (id 0x100000000)` |
| The pool's limits hold | The limits program | `write 9: pool exhausted: 8/8`; `payload too large`; `held after freeing: 0`; `invalid segment id 0x0` |

If `create` fails with an I/O error of kind `AlreadyExists` ("File exists"), a pool of that name is left from an earlier run that did not exit cleanly; pick another name or remove the shared-memory object. On Linux, `NoSpace` at `create` means `/dev/shm` cannot hold the pool: the default is 32 MiB, which fits Docker's default 64 MB `/dev/shm`.

## What happened

- **Only the reference crosses the ring.** A Pulse is 128 bytes and cannot carry a scan. The scan goes into a segment of a shared-memory pool that both processes map by name, and the Pulse carries a 16-byte `SegmentRef`: the segment's id, with its generation in the high 32 bits, the payload's length and its CRC-32C.
- **A reference is checked twice.** `read` refuses a reference whose segment is not held under the reference's generation, before and after the copy, and refuses a payload whose CRC does not match. A freed segment's old reference is `InvalidSegment`; the next writer gets the segment at the next generation, so a stale reference can never read a newer payload.
- **The pool is fixed at creation.** `segment_count` is the most payloads in flight at once and `segment_bytes` the largest payload; the defaults are eight segments of 4 MiB, 32 MiB in all, and on Linux the pages are reserved when the pool is created, so running out is `NoSpace` then rather than a fault later; the pool must fit `/dev/shm`. A payload longer than a segment is refused before any segment is claimed, and a ninth payload on eight held segments is `Exhausted`.
- **The reader frees.** `read` leaves the segment held; the reader calls `free` when it is done, and the writer's `in_use` falls. `alloc` and `SegmentMut::finish` write a payload in place instead of copying it in.

What this stage does not do: the pool is one machine's shared memory and does not cross hosts; nothing in it queues or assigns, so the ring or the daemon carries the Pulses and the pool holds only what they point at; and `free` is the reader's duty, since the pool cannot know when a payload has been consumed.

## Make it yours

- **Camera frames.** `segment_bytes = 8 << 20` for raw 1080p frames (1920×1080 RGBA is 8,294,400 bytes); a raw 4K frame needs `12 << 20` in NV12 or `32 << 20` in RGBA. `segment_count` is the frames in flight between capture and consumption, and `segment_count × segment_bytes` is the pool's size, reserved up front on Linux, so it must fit `/dev/shm`: Docker's default 64 MB holds the 32 MiB default, not eight 32 MiB segments.
- **Two real processes.** The writer publishes the packed Pulse into a `PulseRing` ([The ring inside your process](/docs/workflows/ring-in-process.html)) or through the daemon; a reader in another program maps the pool with `LargePayloadPool::open(name)` and unpacks the reference from the Pulse it read.
- **Between hosts, on Linux.** `robotics_hub::stages::net_uring::UringSpsc` is a UDP endpoint on an io_uring instance: `new(UringConfig)`, `bind_udp`, `connect_udp`, then `send`, `send_batch`, `recv` and `recv_timeout`, with the kernel's submission polling where it allows it; `FlowPool` keeps one persistent TCP flow per peer (`acquire(dst)`, `reap_idle`). It moves the bytes you hand it and frames nothing; for Pulse frames, encode with `pulse_wire_udp` first. Off Linux, `UringSpsc::new` returns `NetUringError::Setup`.
- **A ring that follows the load.** `robotics_hub::stages::autoscale`: a `LagObserver` watches the gap between a ring's write and read cursors and decides when the next ring should grow or shrink within `AutoscaleConfig`, and a `ResizingRing` applies it by replacing the ring under its name through the superseded mark (ADR-0015), which a `FollowingReader` follows.

## Next

- [`robotics_ring`](../architecture/rings/robotics_ring.md): the ring crate by crate.
- [The ring inside your process](/docs/workflows/ring-in-process.html): the ring and the store the pool sits beside.
- [Across a network](/docs/examples/across-a-network.html): the UDP transport the frames ride on between machines.

## Spoken walkthrough

*A script for the video and audio version of this page, about ninety seconds.*

A LIDAR scan is four megabytes. A Pulse is a hundred and twenty-eight bytes. So the scan never goes through the ring; a reference to it does.

The writer creates a pool in shared memory, writes the scan into one of its segments, and gets back sixteen bytes: which segment, which generation of it, how long the payload is, and its checksum. Those sixteen bytes go into a Pulse, and the Pulse is what a ring would carry.

The reader is another process. It maps the pool by name, takes the sixteen bytes out of the Pulse, and reads the segment. Before it hands the payload over, it checks that the segment is still held under that generation and that the checksum matches. Then it frees the segment.

Back in the writer: zero segments in use. Try the old reference, and it is refused. Write the next scan, and it takes the same segment at generation one, so no stale reference can ever read a newer payload.

The second program shows the edges. Eight segments, and the ninth write says so. A payload one byte too big is refused before anything is claimed. A freed reference is an invalid segment. Everything the pool promises, it says out loud.
