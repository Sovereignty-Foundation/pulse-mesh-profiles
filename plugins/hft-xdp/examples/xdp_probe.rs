// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! A probe for the AF_XDP transport between two hosts, beside the kernel's
//! UDP stack for comparison: one end sends as fast as it can, the other
//! counts what arrives, and each prints one line of JSON.
//!
//! ```text
//! xdp_probe recv --iface ens6 --listen 10.0.1.20:9000 [--secs 10] [--generic] [--udp]
//!                [--batch 64] [--busy-poll 50]
//! xdp_probe send --iface ens6 --local 10.0.1.10:40000 --peer 10.0.1.20:9000 \
//!                --mac 0a:1b:2c:3d:4e:5f [--count 1000000] [--size 64] [--udp]
//! ```
//!
//! Round trips, for latency, since two hosts' clocks do not compare: `ping`
//! sends a frame and waits for it back, `--count` times, and `pong` sends
//! each frame back as it comes. AF_XDP takes one socket per queue, so each
//! end receives on one interface and sends from another:
//!
//! ```text
//! xdp_probe pong --iface ens6 --listen B6:9000 --tx-iface ens7 --local B7:40000 \
//!                --peer A6:9000 --mac <A6's MAC> [--busy-poll 50] [--udp]
//! xdp_probe ping --iface ens6 --listen A6:9000 --tx-iface ens7 --local A7:40000 \
//!                --peer B6:9000 --mac <B6's MAC> [--count 100000] [--size 64] [--busy-poll 50] [--udp]
//! ```
//!
//! `--udp` uses `pulse_wire_udp` over the kernel's stack instead; `--generic`
//! attaches the receiver's program in the kernel's generic hook. `--batch N`
//! sends with `send_batch`, N frames a call, and receives with
//! `recv_batch_with`, up to N a call; `--busy-poll US` has the receiver
//! busy-poll its queue. The
//! receiver stops `--secs` after its first frame, or after a second with
//! none.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::time::{Duration, Instant};

use pulse_canon::{Instruction, Pulse};
use pulse_wire_xdp::{frame::parse_mac, AttachMode, XdpConfig, XdpReceiver, XdpSender};

fn args() -> (String, HashMap<String, String>) {
    let mut it = std::env::args().skip(1);
    let mode = it.next().unwrap_or_default();
    let mut map = HashMap::new();
    while let Some(k) = it.next() {
        let key = k.trim_start_matches("--").to_owned();
        match key.as_str() {
            "generic" | "udp" => {
                map.insert(key, String::new());
            }
            _ => {
                map.insert(key, it.next().unwrap_or_default());
            }
        }
    }
    (mode, map)
}

fn get<T: std::str::FromStr>(a: &HashMap<String, String>, k: &str, default: Option<T>) -> T {
    match a.get(k) {
        Some(v) => v
            .parse()
            .unwrap_or_else(|_| fail(&format!("--{k}: cannot read {v}"))),
        None => default.unwrap_or_else(|| fail(&format!("--{k} is needed"))),
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("xdp_probe: {msg}");
    std::process::exit(2)
}

fn main() {
    let (mode, a) = args();
    match mode.as_str() {
        "recv" => recv(&a),
        "send" => send(&a),
        "ping" => round_trips(&a, true),
        "pong" => round_trips(&a, false),
        _ => fail("the first argument is recv, send, ping or pong"),
    }
}

/// What a receiver has counted.
#[derive(Default)]
struct Tally {
    frames: u64,
    bytes: u64,
    highest_seq: u64,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Tally {
    fn record(&mut self, len: usize, seq: u64) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.frames += 1;
        self.bytes += len as u64;
        self.highest_seq = self.highest_seq.max(seq);
    }

    fn done(&self, secs: u64) -> bool {
        self.first
            .is_some_and(|f| f.elapsed() > Duration::from_secs(secs))
    }

    fn span(&self) -> f64 {
        match (self.first, self.last) {
            (Some(f), Some(l)) => l.duration_since(f).as_secs_f64(),
            _ => 0.0,
        }
    }
}

fn recv(a: &HashMap<String, String>) {
    let listen: SocketAddrV4 = get(a, "listen", None);
    let secs: u64 = get(a, "secs", Some(10));
    let mut buf = vec![0u8; 2048];
    let mut t = Tally::default();
    // Without a first frame in this long, nothing is coming.
    let started = Instant::now();
    let given_up = |t: &Tally| t.first.is_none() && started.elapsed() > Duration::from_secs(30);
    let (mode, native, zero_copy, refused, kernel_dropped);
    if a.contains_key("udp") {
        let mut rx =
            pulse_wire_udp::Receiver::bind(listen).unwrap_or_else(|e| fail(&e.to_string()));
        rx.set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap_or_else(|e| fail(&e.to_string()));
        println!("{{\"ready\":true}}");
        while !t.done(secs) {
            match rx.recv_into(&mut buf) {
                Ok((_, n, meta)) => t.record(n, meta.seq),
                Err(e) if e.is_timeout() && (t.first.is_some() || given_up(&t)) => break,
                Err(e) if e.is_timeout() => {}
                Err(e) => fail(&e.to_string()),
            }
        }
        (mode, native, zero_copy, refused, kernel_dropped) = ("udp", false, false, 0, 0);
    } else {
        let iface: String = get(a, "iface", None);
        let mut cfg = XdpConfig::default();
        cfg.read_timeout = Some(Duration::from_secs(1));
        if a.contains_key("generic") {
            cfg.attach = AttachMode::Generic;
        }
        if let Some(us) = a.get("busy-poll") {
            let us: u64 = us
                .parse()
                .unwrap_or_else(|_| fail("--busy-poll: microseconds"));
            cfg.busy_poll = Some(Duration::from_micros(us));
        }
        cfg.frames = get(a, "frames", Some(cfg.frames));
        cfg.ring = get(a, "ring", Some(cfg.ring));
        let batch: usize = get(a, "batch", Some(1));
        let mut rx =
            XdpReceiver::bind(&iface, listen, &cfg).unwrap_or_else(|e| fail(&e.to_string()));
        println!(
            "{{\"ready\":true,\"native\":{},\"zero_copy\":{}}}",
            rx.native(),
            rx.zero_copy()
        );
        while !t.done(secs) {
            let got = if batch > 1 {
                rx.recv_batch_with(batch, |_, env, meta| t.record(env.len(), meta.seq))
                    .map(|_| ())
            } else {
                rx.recv_into(&mut buf)
                    .map(|(_, n, meta)| t.record(n, meta.seq))
            };
            match got {
                Ok(()) => {}
                Err(pulse_wire_xdp::XdpError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::TimedOut =>
                {
                    if t.first.is_some() || given_up(&t) {
                        break;
                    }
                }
                Err(e) => fail(&e.to_string()),
            }
        }
        (mode, native, zero_copy, refused, kernel_dropped) = (
            "xdp",
            rx.native(),
            rx.zero_copy(),
            rx.refused_count(),
            rx.kernel_dropped_count(),
        );
    }
    let span = t.span();
    let (batch, busy) = (
        a.get("batch").map_or("1", String::as_str),
        a.get("busy-poll").map_or("0", String::as_str),
    );
    println!(
        "{{\"role\":\"recv\",\"mode\":\"{mode}\",\"batch\":{batch},\"busy_poll_us\":{busy},\"native\":{native},\"zero_copy\":{zero_copy},\"frames\":{},\"bytes\":{},\"highest_seq\":{},\"span_s\":{span:.6},\"frames_per_s\":{:.0},\"refused\":{refused},\"kernel_dropped\":{kernel_dropped}}}",
        t.frames,
        t.bytes,
        t.highest_seq,
        if span > 0.0 { t.frames as f64 / span } else { 0.0 }
    );
}

fn send(a: &HashMap<String, String>) {
    let local: SocketAddrV4 = get(a, "local", None);
    let peer: SocketAddrV4 = get(a, "peer", None);
    let count: u64 = get(a, "count", Some(1_000_000));
    let size: usize = get(a, "size", Some(64));
    let envelope: Vec<u8> = (0..size).map(|i| i as u8).collect();
    let mut pulse = Pulse::zeroed();
    pulse.instruction = Instruction::Data as u16;
    let start;
    let mode;
    if a.contains_key("udp") {
        let mut tx = pulse_wire_udp::Sender::bind(local).unwrap_or_else(|e| fail(&e.to_string()));
        tx.connect(peer).unwrap_or_else(|e| fail(&e.to_string()));
        start = Instant::now();
        for n in 0..count {
            pulse.chain_step = n;
            tx.send(0, &pulse, &envelope)
                .unwrap_or_else(|e| fail(&e.to_string()));
        }
        mode = "udp";
    } else {
        let iface: String = get(a, "iface", None);
        let mac_text: String = get(a, "mac", None);
        let mac = parse_mac(&mac_text).unwrap_or_else(|| fail("--mac: six hex pairs"));
        let cfg = XdpConfig::default();
        let batch: u64 = get(a, "batch", Some(1));
        let mut tx = XdpSender::bind(&iface, local, peer, Some(mac), &cfg)
            .unwrap_or_else(|e| fail(&e.to_string()));
        start = Instant::now();
        if batch > 1 {
            let mut pulses = vec![pulse; batch as usize];
            let mut sent = 0;
            while sent < count {
                let n = batch.min(count - sent) as usize;
                for (i, p) in pulses.iter_mut().take(n).enumerate() {
                    p.chain_step = sent + i as u64;
                }
                let frames: Vec<(&Pulse, &[u8])> = pulses[..n]
                    .iter()
                    .map(|p| (p, envelope.as_slice()))
                    .collect();
                tx.send_batch(0, &frames)
                    .unwrap_or_else(|e| fail(&e.to_string()));
                sent += n as u64;
            }
        } else {
            for n in 0..count {
                pulse.chain_step = n;
                tx.send(0, &pulse, &envelope)
                    .unwrap_or_else(|e| fail(&e.to_string()));
            }
        }
        tx.flush(Duration::from_secs(5))
            .unwrap_or_else(|e| fail(&e.to_string()));
        mode = "xdp";
    }
    let span = start.elapsed().as_secs_f64();
    println!(
        "{{\"role\":\"send\",\"mode\":\"{mode}\",\"batch\":{},\"frames\":{count},\"size\":{size},\"span_s\":{span:.6},\"frames_per_s\":{:.0}}}",
        a.get("batch").map_or("1", String::as_str),
        count as f64 / span
    );
}

/// One end of a round trip, over AF_XDP or the kernel's UDP. One is made
/// per run, so the variants' sizes do not matter.
#[allow(clippy::large_enum_variant)]
enum Ends {
    Xdp(XdpReceiver, XdpSender),
    Udp(pulse_wire_udp::Receiver, pulse_wire_udp::Sender),
}

impl Ends {
    fn open(a: &HashMap<String, String>, timeout: Duration) -> Self {
        let listen: SocketAddrV4 = get(a, "listen", None);
        let local: SocketAddrV4 = get(a, "local", None);
        let peer: SocketAddrV4 = get(a, "peer", None);
        if a.contains_key("udp") {
            let rx =
                pulse_wire_udp::Receiver::bind(listen).unwrap_or_else(|e| fail(&e.to_string()));
            rx.set_read_timeout(Some(timeout))
                .unwrap_or_else(|e| fail(&e.to_string()));
            let mut tx =
                pulse_wire_udp::Sender::bind(local).unwrap_or_else(|e| fail(&e.to_string()));
            tx.connect(peer).unwrap_or_else(|e| fail(&e.to_string()));
            return Ends::Udp(rx, tx);
        }
        let iface: String = get(a, "iface", None);
        let tx_iface: String = get(a, "tx-iface", None);
        let mac_text: String = get(a, "mac", None);
        let mac = parse_mac(&mac_text).unwrap_or_else(|| fail("--mac: six hex pairs"));
        let mut cfg = XdpConfig::default();
        cfg.read_timeout = Some(timeout);
        if let Some(us) = a.get("busy-poll") {
            let us: u64 = us
                .parse()
                .unwrap_or_else(|_| fail("--busy-poll: microseconds"));
            cfg.busy_poll = Some(Duration::from_micros(us));
        }
        let rx = XdpReceiver::bind(&iface, listen, &cfg).unwrap_or_else(|e| fail(&e.to_string()));
        let tx = XdpSender::bind(&tx_iface, local, peer, Some(mac), &XdpConfig::default())
            .unwrap_or_else(|e| fail(&e.to_string()));
        Ends::Xdp(rx, tx)
    }

    /// Receive one frame into `buf`: its Pulse, length and send stamp.
    fn recv(&mut self, buf: &mut [u8]) -> Option<(Pulse, usize, u64)> {
        match self {
            Ends::Xdp(rx, _) => rx.recv_into(buf).ok().map(|(p, n, m)| (p, n, m.send_ns)),
            Ends::Udp(rx, _) => rx.recv_into(buf).ok().map(|(p, n, m)| (p, n, m.send_ns)),
        }
    }

    fn send(&mut self, send_ns: u64, pulse: &Pulse, envelope: &[u8]) {
        let sent = match self {
            Ends::Xdp(_, tx) => tx
                .send(send_ns, pulse, envelope)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Ends::Udp(_, tx) => tx
                .send(send_ns, pulse, envelope)
                .map(|_| ())
                .map_err(|e| e.to_string()),
        };
        sent.unwrap_or_else(|e| fail(&e));
    }
}

/// `ping` (true) or `pong` (false).
fn round_trips(a: &HashMap<String, String>, ping: bool) {
    let mut buf = vec![0u8; 2048];
    if !ping {
        let mut ends = Ends::open(a, Duration::from_secs(1));
        println!("{{\"ready\":true}}");
        // Echo until a minute passes with nothing to echo.
        let mut idle = Instant::now();
        while idle.elapsed() < Duration::from_secs(60) {
            if let Some((pulse, n, send_ns)) = ends.recv(&mut buf) {
                let envelope = buf[..n].to_vec();
                ends.send(send_ns, &pulse, &envelope);
                idle = Instant::now();
            }
        }
        return;
    }
    let count: usize = get(a, "count", Some(100_000));
    let size: usize = get(a, "size", Some(64));
    let warmup = count / 10;
    let envelope: Vec<u8> = (0..size).map(|i| i as u8).collect();
    let mut ends = Ends::open(a, Duration::from_millis(100));
    let mut pulse = Pulse::zeroed();
    pulse.instruction = Instruction::Data as u16;
    let clock = Instant::now();
    let mut rtts: Vec<u64> = Vec::with_capacity(count);
    let mut lost = 0u64;
    for n in 0..(warmup + count) {
        pulse.chain_step = n as u64;
        let t0 = clock.elapsed().as_nanos() as u64;
        ends.send(t0, &pulse, &envelope);
        // Wait for this frame's echo; an older one late is skipped.
        let rtt = loop {
            match ends.recv(&mut buf) {
                Some((p, _, stamp)) if p.chain_step == n as u64 && stamp == t0 => {
                    break Some(clock.elapsed().as_nanos() as u64 - t0)
                }
                Some(_) => continue,
                None => break None,
            }
        };
        match rtt {
            Some(r) if n >= warmup => rtts.push(r),
            Some(_) => {}
            None => lost += 1,
        }
    }
    rtts.sort_unstable();
    let q = |f: f64| {
        rtts.get(((rtts.len() as f64 - 1.0) * f).round() as usize)
            .copied()
            .unwrap_or(0)
    };
    let mode = if a.contains_key("udp") { "udp" } else { "xdp" };
    println!(
        "{{\"role\":\"ping\",\"mode\":\"{mode}\",\"busy_poll_us\":{},\"size\":{size},\"round_trips\":{},\"lost\":{lost},\"rtt_ns\":{{\"min\":{},\"p50\":{},\"p90\":{},\"p99\":{},\"p999\":{},\"max\":{}}}}}",
        a.get("busy-poll").map_or("0", String::as_str),
        rtts.len(),
        q(0.0),
        q(0.5),
        q(0.9),
        q(0.99),
        q(0.999),
        q(1.0)
    );
}
