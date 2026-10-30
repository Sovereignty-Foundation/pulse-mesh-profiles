# The AF_XDP transport

<p class="pm-kind"><b>Reference</b> · `pulse-wire-xdp`, the UDP transport's frames sent and received past the kernel's network stack, on Linux</p>

`pulse-wire-xdp`, in `plugins/hft-xdp`, carries the UDP transport's frames over AF_XDP. An `XdpSender` writes each Pulse frame, with its Ethernet, IPv4 and UDP headers, straight into a NIC queue's transmit ring. An `XdpReceiver` loads an XDP program that hands the queue's packets for its UDP port to its own receive ring and lets every other packet go on to the kernel. What travels between the headers is `pulse-wire-udp`'s datagram, byte for byte, so either end can be the kernel's UDP stack: an XDP sender talks to a `pulse_wire_udp::Receiver`, and a `pulse_wire_udp::Sender` to an XDP receiver. Both types implement the `core-transport` traits `PulseSender` and `PulseReceiver`, so they slot in where the UDP transport does.

## What it needs

- 64-bit little-endian Linux, 5.9 or later. On any other host every constructor returns `XdpError::PlatformUnsupported`, so a tool can fall back to the UDP transport.
- `CAP_NET_ADMIN` and `CAP_BPF`, or root. Without them `XdpError::Io` names the step the kernel refused.
- Locked memory for the UMEM: `CAP_IPC_LOCK`, or an `RLIMIT_MEMLOCK` above it. The UMEM is 4 KiB a frame, 16 MiB by default.
- One socket per interface queue. A sender and a receiver on one interface take a queue each, and one receiver's program holds the interface.

## What it is

- One peer per sender, plain IPv4 with no options, no fragmentation.
- None of the UDP transport's options: no backpressure, NACK, FEC, sealing, fragmentation or delivery order. A frame lost is lost.
- The peer's MAC comes from the kernel's neighbour table, or from the caller.
- The longest envelope is what the interface's MTU leaves of `pulse_wire_udp::wire::MAX_ENVELOPE`.
- Copy mode is the tested path. The receiver binds zero-copy when the driver supports it; zero-copy has not been exercised on hardware that offers it, and on AWS's ENA driver both ends run in copy mode.

## Configuration

`XdpConfig` is `non_exhaustive`; start from `XdpConfig::default()` and set fields.

| Field | Default | Meaning |
|---|---|---|
| `queue` | `0` | The NIC queue to bind. |
| `frames` | `4096` | UMEM frames of 4 KiB; a power of two. |
| `ring` | `2048` | Entries in each ring; a power of two. |
| `attach` | `AttachMode::Auto` | Where the receiver's program attaches: the driver's own hook if it has one (`Native`), else the kernel's generic hook (`Generic`), which every interface has and which runs after the driver has built its socket buffer. |
| `zero_copy` | `ZeroCopy::Auto` | The receiver zero-copy when the driver supports it, the sender copy; `Required` fails otherwise, `Off` copies every packet. |
| `read_timeout` | `None` | How long a receive waits for a frame; `None` waits forever. |
| `send_timeout` | 1 s | How long a send waits for a free frame. |
| `busy_poll` | `None` | The receiver busy-polls its queue for this long at a time instead of waiting for its interrupt (`SO_PREFER_BUSY_POLL`, Linux 5.11). It costs a core and can cut the time to the first packet; pair it with the interface's `napi_defer_hard_irqs` and `gro_flush_timeout`. |

## The sender

| Call | What it does |
|---|---|
| `XdpSender::bind(iface, local, peer, peer_mac, &cfg)` | Send from `local` on `iface` to `peer`, whose MAC is `peer_mac` or, if `None`, the neighbour table's entry for it on `iface` (`XdpError::NoNeighbour` if there is none: reach the peer once over the kernel's stack, or pass its MAC). |
| `send(send_ns, &pulse, envelope)` | Send one frame stamped `send_ns`; returns its sequence number. |
| `send_batch(send_ns, &[(&pulse, envelope)])` | Send each pair as the next frame, in order, waking the kernel once per run of free frames rather than once per frame; returns the first sequence number. Fails with `EnvelopeTooLarge` before sending anything, or `TxFull` with the frames before it handed over. |
| `flush(timeout)` | Wake the kernel until it has sent every frame handed to it, or the timeout passes (`TxFull` with frames still waiting). |
| `max_envelope()`, `next_seq()`, `endpoints()`, `zero_copy()` | The longest envelope a send takes; the next sequence number; the MACs and addresses in use; whether the socket shares its frames with the NIC. |

## The receiver

| Call | What it does |
|---|---|
| `XdpReceiver::bind(iface, listen, &cfg)` | Receive the UDP datagrams to `listen` (an unspecified address takes any) that arrive on `iface`'s queue `cfg.queue`. `XdpError::Verifier` carries the kernel's log if it refuses the program. |
| `recv_into(&mut envelope)` | Wait for the next frame and copy its envelope out; returns the Pulse, the envelope's length and an `XdpMeta` (`seq`, `send_ns`, `peer`). `Io` with `TimedOut` after the read timeout; `Decode` for a datagram to this port that is not a Pulse frame; `BufferTooSmall` with the frame consumed. |
| `recv_batch_with(max, f)` | Hand each frame of the next run the kernel delivered, up to `max`, to `f(&pulse, envelope, meta)`, the envelope as it lies in the received packet, with no copy; the frames go back to the kernel together after the last. Returns how many `f` saw. |
| `refused_count()` | Packets the program passed to this socket that were not datagrams for it; skipped. |
| `decode_error_count()` | Datagrams to this receiver that were not Pulse frames; skipped by `recv_batch_with`. |
| `kernel_dropped_count()` | Packets the kernel dropped because the receive ring was full or no frame was lent for them. |
| `listen_addr()`, `native()`, `zero_copy()` | The address received on; whether the program runs in the driver's own hook; whether the socket shares its frames with the NIC. |

## The probe

`xdp_probe`, the crate's example, runs the transport between two hosts beside the kernel's UDP stack for comparison. One end sends as fast as it can, the other counts what arrives, and each prints one line of JSON. It is a tool for checking a deployment, not a benchmark, and this book records none of its figures.

```bash
cargo run --release -p pulse-wire-xdp --example xdp_probe -- recv --iface ens6 --listen 10.0.1.20:9000 --secs 10
cargo run --release -p pulse-wire-xdp --example xdp_probe -- send --iface ens6 --local 10.0.1.10:40000 --peer 10.0.1.20:9000 --mac 0a:1b:2c:3d:4e:5f --count 1000000
```

`ping` and `pong` measure round trips, since two hosts' clocks do not compare; each end receives on one interface and sends from another, because AF_XDP takes one socket per queue. `--udp` runs the same probe over the kernel's stack, `--generic` attaches the receiver's program in the generic hook, `--batch N` uses `send_batch` and `recv_batch_with`, and `--busy-poll US` has the receiver busy-poll. The example's overview lists every flag.

## Wire-format compatibility

`pulse-wire-xdp` emits the same bytes as `pulse_wire_udp::wire::encode_into` inside its UDP payload. The frame carries its own CRC32C, so the UDP checksum is left zero, which IPv4 allows. Any divergence would be a change to the standard, negotiated there first.
