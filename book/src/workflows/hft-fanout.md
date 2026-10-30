# HFT: one feed, many readers, no copy per reader

<p class="pm-kind"><b>Workflow</b> · for a developer feeding one stream to several consumers at the lowest latency the host allows; about twenty minutes, in Rust, with the profiles repository beside the standard</p>

<figure class="pm-art"><svg class="uc-diagram" viewBox="0 0 380 240" role="img" aria-label="One write, read by every reader, no copy per reader"><circle cx="50" cy="120" r="18" class="uc-node-strong"/><text x="50" y="160" class="uc-label">writer</text><circle cx="160" cy="120" r="34" class="uc-ring"/><circle cx="160" cy="120" r="24" class="uc-ring-dash"/><text x="160" y="172" class="uc-label">one ring</text><line x1="68" y1="120" x2="126" y2="120" class="uc-edge"/><line x1="194" y1="120" x2="290" y2="40" class="uc-edge"/><rect x="290" y="31" width="40" height="18" rx="2" class="uc-node"/><line x1="194" y1="120" x2="290" y2="80" class="uc-edge"/><rect x="290" y="71" width="40" height="18" rx="2" class="uc-node"/><line x1="194" y1="120" x2="290" y2="120" class="uc-edge"/><rect x="290" y="111" width="40" height="18" rx="2" class="uc-node"/><line x1="194" y1="120" x2="290" y2="160" class="uc-edge"/><rect x="290" y="151" width="40" height="18" rx="2" class="uc-node"/><line x1="194" y1="120" x2="290" y2="200" class="uc-edge"/><rect x="290" y="191" width="40" height="18" rx="2" class="uc-node"/><text x="310" y="226" class="uc-label">readers</text><circle class="uc-pulse" r="3"><animateMotion dur="1.6s" begin="0s" repeatCount="indefinite" path="M68,120 L126,120"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.6s" begin="0.8s" repeatCount="indefinite" path="M194,120 L290,40"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.6s" begin="0.8200000000000001s" repeatCount="indefinite" path="M194,120 L290,80"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.6s" begin="0.8400000000000001s" repeatCount="indefinite" path="M194,120 L290,120"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.6s" begin="0.8600000000000001s" repeatCount="indefinite" path="M194,120 L290,160"/></circle><circle class="uc-pulse" r="2.5"><animateMotion dur="1.6s" begin="0.88s" repeatCount="indefinite" path="M194,120 L290,200"/></circle></svg><figcaption>One write, read by every reader, no copy per reader</figcaption></figure>

**You will have:** a program in which one writer publishes a million messages into a ring in process memory and four reader threads each read every one of them, with no copy per reader; each reader's count of what it read and what it lost; the wait strategy detected for your host; and the API path from here to several writers and to NUMA placement on Linux.

**You need:** `pulse-mesh` and `pulse-mesh-profiles` side by side, as [Start here](../start/onboard-your-agent.md) describes, and the Rust toolchain. Nothing here needs Linux; the hardware wait strategies and NUMA placement do, and the page says where.

## Hand this to your agent

```text
Run the workflow "HFT: one feed, many readers" from book/src/workflows/hft-fanout.md in the pulse-mesh-profiles repository:
1. Beside the two repositories, make a crate: cargo new fanout, and in its Cargo.toml add the dependency hft-hub = { path = "../pulse-mesh-profiles/rings/hft_ring/hft-hub" }.
2. Save the program from the page as src/main.rs and run cargo run --release.
3. Show me the output. Expect one "wait strategy for this host" line, four "reader N: read R, lost L, lagged T time(s)" lines, and a final line starting with OK. For every reader, R plus L must equal 1000000; on an idle machine with at least five cores, expect L to be 0.
4. Change the ring's capacity from 1 << 16 to 1 << 10, run again twice, and report whether any reader lost publishes. R plus L must still equal 1000000 for every reader. Put the capacity back.
5. Run cargo test -p hft-stages --release in pulse-mesh-profiles and show me the test result lines; they are the ring's own guarantees under test.
Explain in plain words what the ring did, what "lagged" means, and why the writer never waited. Stop and tell me if anything fails, and say which step.
```

## Or do it by hand

### 1. Make a crate beside the repositories

```bash
cargo new fanout
```

In `fanout/Cargo.toml`, under `[dependencies]`:

```toml
hft-hub = { path = "../pulse-mesh-profiles/rings/hft_ring/hft-hub" }
```

`hft-hub` is the HFT ring's one import point: it re-exports the ring's `core`, `functions` and `stages` layers and declares them in the ring's manifest. Code outside the ring reaches it only through the hub.

### 2. The program

Save as `fanout/src/main.rs`:

```rust
// fanout.rs: one writer, four readers, one ring in process memory.
use hft_hub::functions::wait::detect_best_for_host;
use hft_hub::stages::spmc::{SpmcError, SpmcRing};
use std::thread;

const READERS: usize = 4;
const PUBLISHES: u64 = 1_000_000;

fn main() {
    println!("wait strategy for this host: {}", detect_best_for_host().name());

    // One ring of 65,536 slots; the writer is the ring's one producer.
    let (mut writer, ring) = SpmcRing::new(1 << 16);

    // Each reader claims a cursor before the writer starts, so it sees
    // every publish from the first one on.
    let readers: Vec<_> = (0..READERS)
        .map(|i| {
            let mut reader = ring.register_reader().expect("a reader cursor");
            thread::spawn(move || {
                let mut slot = [0u8; 120];
                let (mut read, mut lagged) = (0u64, 0u64);
                while read + reader.lost() < PUBLISHES {
                    match reader.recv(&mut slot) {
                        Ok(Some(_len)) => read += 1,
                        Ok(None) => std::hint::spin_loop(),
                        Err(SpmcError::Lagged { .. }) => lagged += 1,
                        Err(e) => panic!("reader {i}: {e}"),
                    }
                }
                (i, read, reader.lost(), lagged)
            })
        })
        .collect();

    // The writer never waits for a reader: a reader it laps is told so.
    for n in 0..PUBLISHES {
        writer.publish(&n.to_le_bytes());
    }

    for handle in readers {
        let (i, read, lost, lagged) = handle.join().expect("reader thread");
        println!("reader {i}: read {read}, lost {lost}, lagged {lagged} time(s)");
    }
    println!("OK — {PUBLISHES} publishes, one write each, seen by {READERS} readers");
}
```

### 3. Run it

```bash
cargo run --release
```

```text
wait strategy for this host: busy-spin
reader 0: read 1000000, lost 0, lagged 0 time(s)
reader 1: read 1000000, lost 0, lagged 0 time(s)
reader 2: read 1000000, lost 0, lagged 0 time(s)
reader 3: read 1000000, lost 0, lagged 0 time(s)
OK — 1000000 publishes, one write each, seen by 4 readers
```

The writer wrote each message once. Four readers each read all million through a cursor of their own, so no message was copied once per reader. `lost 0` says no reader fell a full ring behind the writer; on a machine with fewer than five cores, or a busy one, a reader can, and the next step shows what that looks like.

### 4. Make the ring too small

Change `SpmcRing::new(1 << 16)` to `SpmcRing::new(1 << 10)`, 1,024 slots, and run it twice. On some runs every reader still reads everything; on others a reader is lapped:

```text
reader 0: read 995033, lost 4967, lagged 3 time(s)
```

`lost` is the number of publishes the writer overwrote before this reader read them, and `lagged` the number of times `recv` reported a jump. Read plus lost is 1,000,000 on every reader, every run: a lapped reader loses publishes, it never receives one twice or under another's position. Put the capacity back.

## Check it worked

| Check | Command | Pass |
|---|---|---|
| Every reader accounted for every publish | `cargo run --release` | Read plus lost equals 1,000,000 on every reader; expect `lost 0` on an idle machine with at least five cores |
| A lapped reader loses, never duplicates | Capacity `1 << 10`, run twice | Read plus lost equals 1,000,000 for every reader, every run |
| The host's wait strategy was chosen | First output line | `busy-spin` unless the crate was built with a hardware wait feature on a host that has it |
| The ring's own guarantees hold | `cargo test -p hft-stages --release` in `pulse-mesh-profiles` | Every result line `ok`, including the one-writer-N-readers, overrun and concurrent-overrun tests |

If `register_reader` fails, you asked for more than sixteen readers: `SpmcError::TooManyReaders`. If a reader panics with `Lagged` in a program of your own, you matched on `Err(_)` before `Err(SpmcError::Lagged { .. })`; lagging is a report, not a failure.

## What happened

- **One write, many reads.** `SpmcRing` is a single-producer, multi-consumer ring in process memory: a 256-byte header holding the write cursor and sixteen reader cursors, then slots of 128 bytes, each a SeqLock-protected Pulse-sized record with 120 bytes of payload. `publish` writes a slot once; each `SpmcReader` owns one cursor and reads the slot without copying it for anyone else.
- **The writer never waits.** It overwrites the oldest slot when the ring is full. A reader that fell more than a capacity behind, or whose slot the writer took during its copy, gets `Lagged` once, its cursor moves to the oldest publish the ring still holds, and the publishes passed over are counted in `lost`. The ring's tests hold it to this: a reader never receives a publish twice or under another's position. Size the ring for the slowest reader's longest stall; that is the only knob, because the writer will not slow down.
- **How a reader waits is a choice.** `detect_best_for_host` returns the best `WaitStrategy` the build and the host allow: `BusySpin` (lowest latency, a core and memory bandwidth spent) and `Park` (lowest power, highest jitter) are always built; `UMWaitDeadline` (Intel WAITPKG), `MWaitX` (AMD Zen 2 and later) and `WfeArm` (AArch64) are behind the `umwait`, `mwaitx` and `wfe` features of `hft-functions` and wait on the slot's cache line in hardware. This run used `busy-spin`, which is what the loop above does by hand with `spin_loop`.
- **The same rule as the standard's ring.** The SeqLock protocol is the one [the ring chapter](/docs/reference/spec/04-ring.html) states for `PulseRing`, and [The ring inside your process](/docs/workflows/ring-in-process.html) is the same guarantee across processes through shared memory.

What this ring does not do: it lives in one process's memory, so it does not cross processes (the standard's `PulseRing` and the multilane ring below do, through POSIX shared memory); it takes at most sixteen readers; a payload is at most 120 bytes, the data area of a Pulse, and larger content travels by reference, as the envelope store or the robotics ring's payload pool hold it; and it applies no backpressure, by design: there is no blocking publish here. Lossless fan-out, where the writer waits for the slowest reader, is the standard's ring with a reader table (ADR-0019): `PulseRing::create_with_readers(name, capacity, max_readers)`, each reader's `register_reader`, and `publish_blocking` keeping the slowest registered reader's pace, across processes through shared memory.

## Make it yours

- **Several writers.** `hft_hub::stages::multilane::MultiLaneRing::create("fanout", 1024, 4)` makes four lanes in shared memory, one `LaneWriter` each, and one reader that takes them in turn with `poll_round_robin`, so writers on different cores never share a cache line. A lane cursor the writer laps is counted in `lost(lane)`, as here. On macOS keep the name prefix to 24 bytes.
- **Place lanes on NUMA nodes.** On Linux, `MultiLaneRing::create_with_numa` puts each lane's backing on a chosen node, and `pin_lane_writer(lane, node)` pins its writer's thread; `hft_hub::stages::numa` is `core_region::numa` re-exported, with `allocate_shm_backing`, hugepage backings and `pin_thread_to_numa_node`. Off Linux, `create_with_numa` falls back to `create` and binds nothing, `pin_lane_writer` does nothing, and `pin_thread_to_numa_node`, `allocate_shm_backing` with a node set and `allocate_hugepage_memfd` return `NumaError::LibnumaMissing`.
- **Wait in hardware.** Turn a feature on through the hub, which passes it to `hft-functions`: in `Cargo.toml`, `hft-hub = { path = "../pulse-mesh-profiles/rings/hft_ring/hft-hub", features = ["umwait"] }`, or on the command line `cargo run --release --features hft-hub/umwait`; `mwaitx` for AMD Zen 2 and later, `wfe` for AArch64. The first line then names the strategy on a host that has it (`wfe` on an Apple-silicon Mac), and `busy-spin` where it does not; `wait` stops the core loading the line until it changes.
- **Release the core under load.** `pms-telemetry`'s `AdaptiveWaitStrategy` runs your strategy while the host's scheduling pressure is low and falls through to `Park` when it is not, reading `/proc/pressure/cpu` on Linux.

## Next

- [The ring inside your process](/docs/workflows/ring-in-process.html): the standard's ring and store, across processes, with the same guarantee.
- [`hft_ring`](../architecture/rings/hft_ring.md): the ring crate by crate.
- [The AF_XDP transport](../reference/xdp-transport.md): the same frames past the kernel's network stack, for the network side of a trading path.
- [The SeqLock memory model](/docs/explanation/seqlock-memory-model.html): why a reader never sees a torn record.

## Spoken walkthrough

*A script for the video and audio version of this page, about ninety seconds.*

One feed, four readers, and the question is how many times each message gets copied. Here: once.

The program makes a ring in memory with sixty-five thousand slots. One writer owns it. Four reader threads each register, which gives each one a cursor of its own in the ring's header. Then the writer publishes a million messages as fast as it can, and never once waits for a reader.

Each reader walks the ring at its own pace. When it finishes, it reports what it read and what it lost. On an idle machine: a million read, nothing lost, four times over. One write per message, four reads, no copies.

Now shrink the ring to a thousand slots and run it again. Some runs still lose nothing. On others, a reader falls a full ring behind and the writer laps it. The reader is told once, jumps to the oldest message still held, and counts what it skipped. Read plus lost is still exactly a million. It loses; it never sees a message twice.

The first line of output names how a reader waits: busy-spin here. Build with a hardware wait feature on a host that has it, and the reader waits on the cache line in hardware instead of burning the core. That is the whole trade in high-frequency work: latency against power, and the ring lets you choose.
