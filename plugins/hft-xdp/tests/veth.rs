// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! AF_XDP end to end over veth pairs this test makes with `ip`: XDP to XDP
//! on one pair, and between XDP and the kernel's UDP stack across two
//! network namespaces, one end in each. The half in the other namespace is
//! this test binary run again under `ip netns exec`, its role in the
//! environment.
//!
//! The tests need `CAP_NET_ADMIN`, `CAP_BPF` and `CAP_SYS_ADMIN` and the
//! `ip` tool: `LINUX_VERIFY_NET=1 scripts/linux-verify.sh test -p
//! pulse-wire-xdp` runs them so. Without those, each prints SKIP and
//! passes.
#![cfg(xdp)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::io::Write;
use std::net::{SocketAddrV4, UdpSocket};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use pulse_canon::{Instruction, Pulse};
use pulse_wire_xdp::frame::{parse_mac, Mac};
use pulse_wire_xdp::{XdpConfig, XdpReceiver, XdpSender};

const ROLE: &str = "XDP_TEST_ROLE";
const ARG: &str = "XDP_TEST_ARG";
const PORT: u16 = 9000;
const OTHER_PORT: u16 = 9001;
const FRAMES: u64 = 200;

fn pulse(n: u64) -> Pulse {
    let mut p = Pulse::zeroed();
    p.instruction = Instruction::Data as u16;
    p.chain_step = n;
    p
}

fn envelope(n: u64) -> Vec<u8> {
    (0..(n % 300) as usize)
        .map(|i| (i as u8) ^ (n as u8))
        .collect()
}

fn ip(args: &[&str]) -> Output {
    Command::new("ip").args(args).output().expect("run ip")
}

fn ip_ok(args: &[&str]) {
    let out = ip(args);
    assert!(
        out.status.success(),
        "ip {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A veth pair, `a` here and `b` here or in a namespace of its own, with
/// 10.<net>.0.1 on `a` and 10.<net>.0.2 on `b`; removed when dropped.
struct Veth {
    a: String,
    b: String,
    ns: Option<String>,
    net: u8,
}

impl Veth {
    /// Make one, or `None` (having printed SKIP) when this host cannot.
    fn new(with_ns: bool) -> Option<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let id = std::process::id() % 10_000;
        let net = 100 + (id % 100) as u8 + n as u8;
        let (a, b) = (format!("pxa{id}x{n}"), format!("pxb{id}x{n}"));
        let made = Command::new("ip")
            .args(["link", "add", &a, "type", "veth", "peer", "name", &b])
            .output();
        match made {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                println!(
                    "SKIP: cannot make a veth pair: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                return None;
            }
            Err(e) => {
                println!("SKIP: no ip tool: {e}");
                return None;
            }
        }
        let ns = with_ns.then(|| format!("pxn{id}x{n}"));
        let veth = Self { a, b, ns, net };
        let (a, b) = (veth.a.clone(), veth.b.clone());
        ip_ok(&["addr", "add", &format!("10.{net}.0.1/24"), "dev", &a]);
        ip_ok(&["link", "set", &a, "up"]);
        if let Some(ns) = &veth.ns {
            ip_ok(&["netns", "add", ns]);
            ip_ok(&["link", "set", &b, "netns", ns]);
            ip_ok(&[
                "-n",
                ns,
                "addr",
                "add",
                &format!("10.{net}.0.2/24"),
                "dev",
                &b,
            ]);
            ip_ok(&[
                "-n",
                ns,
                "addr",
                "add",
                &format!("10.{net}.0.3/24"),
                "dev",
                &b,
            ]);
            ip_ok(&["-n", ns, "link", "set", &b, "up"]);
            ip_ok(&["-n", ns, "link", "set", "lo", "up"]);
        } else {
            ip_ok(&["addr", "add", &format!("10.{net}.0.2/24"), "dev", &b]);
            ip_ok(&["link", "set", &b, "up"]);
        }
        Some(veth)
    }

    fn addr(&self, host: u8, port: u16) -> SocketAddrV4 {
        SocketAddrV4::new([10, self.net, 0, host].into(), port)
    }

    /// The MAC of `b`, wherever it is.
    fn b_mac(&self) -> Mac {
        let path = format!("/sys/class/net/{}/address", self.b);
        let text = match &self.ns {
            None => std::fs::read_to_string(path).unwrap(),
            Some(ns) => {
                let out = Command::new("ip")
                    .args(["netns", "exec", ns, "cat", &path])
                    .output()
                    .unwrap();
                String::from_utf8(out.stdout).unwrap()
            }
        };
        parse_mac(&text).expect("b's MAC")
    }

    /// Run this binary's `role` in `b`'s namespace with `arg`.
    fn child(&self, role: &str, arg: &str) -> std::process::Child {
        let ns = self.ns.as_ref().expect("a namespace");
        Command::new("ip")
            .args(["netns", "exec", ns])
            .arg(std::env::current_exe().unwrap())
            .args([
                "child_role_entry",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ROLE, role)
            .env(ARG, arg)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the child")
    }
}

impl Drop for Veth {
    fn drop(&mut self) {
        // Deleting one end deletes both, wherever the other is.
        let _ = ip(&["link", "del", &self.a]);
        if let Some(ns) = &self.ns {
            let _ = ip(&["netns", "del", ns]);
        }
    }
}

fn config() -> XdpConfig {
    let mut cfg = XdpConfig::default();
    cfg.frames = 1024;
    cfg.ring = 512;
    cfg.read_timeout = Some(Duration::from_secs(5));
    cfg
}

/// Receive `FRAMES` frames and check each, in order.
fn receive_all(rx: &mut XdpReceiver) {
    let mut buf = vec![0u8; 2048];
    for n in 0..FRAMES {
        let (p, len, meta) = rx.recv_into(&mut buf).expect("a frame");
        assert_eq!((meta.seq, p.chain_step), (n, n), "frame {n}");
        assert!(buf[..len] == envelope(n), "envelope {n} changed");
    }
}

fn wait_ok(child: std::process::Child, what: &str) {
    let out = child.wait_with_output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{what} failed:\n{text}");
    assert!(text.contains("child done"), "{what}:\n{text}");
}

/// The other namespace's half: `xdp-rx` receives over XDP on `b` and a
/// datagram on the kernel's socket on the other port; `udp-rx` receives
/// over the kernel's UDP stack.
#[test]
fn child_role_entry() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let (iface, listen) = {
        let arg = std::env::var(ARG).unwrap();
        let (iface, addr) = arg.split_once(' ').unwrap();
        (iface.to_owned(), addr.parse::<SocketAddrV4>().unwrap())
    };
    match role.as_str() {
        "xdp-rx" => {
            let other = UdpSocket::bind(SocketAddrV4::new(*listen.ip(), OTHER_PORT)).unwrap();
            other
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            // The same port on the interface's other address.
            let mut beside = listen.ip().octets();
            beside[3] += 1;
            let beside = UdpSocket::bind(SocketAddrV4::new(beside.into(), PORT)).unwrap();
            beside
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut rx = XdpReceiver::bind(&iface, listen, &config()).expect("bind over XDP");
            println!("ready native={} zero_copy={}", rx.native(), rx.zero_copy());
            std::io::stdout().flush().unwrap();
            receive_all(&mut rx);
            // The program passes what is not for its port to the kernel.
            let mut b = [0u8; 16];
            let (n, _) = other.recv_from(&mut b).expect("the other port's datagram");
            assert_eq!(&b[..n], b"to the kernel");
            // And what is for the port on another address.
            let (n, _) = beside
                .recv_from(&mut b)
                .expect("the other address's datagram");
            assert_eq!(&b[..n], b"beside");
        }
        "udp-rx" => {
            let mut rx = pulse_wire_udp::Receiver::bind(listen).unwrap();
            rx.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            println!("ready");
            std::io::stdout().flush().unwrap();
            let mut buf = vec![0u8; 2048];
            for n in 0..FRAMES {
                let (p, len, meta) = rx.recv_into(&mut buf).expect("a frame");
                assert_eq!((meta.seq, p.chain_step), (n, n), "frame {n}");
                assert!(buf[..len] == envelope(n), "envelope {n} changed");
            }
        }
        other => panic!("unknown role {other}"),
    }
    println!("child done");
}

/// Read the child's first line, which it prints once it is listening.
fn wait_ready(child: &mut std::process::Child) -> String {
    use std::io::BufRead;
    let out = child.stdout.as_mut().unwrap();
    let mut line = String::new();
    let mut reader = std::io::BufReader::new(out);
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap() == 0 {
            let mut err = String::new();
            std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut err).unwrap();
            panic!("the child ended before ready:\n{err}");
        }
        // The test harness may write its own words before the child's on
        // the same line.
        if let Some(at) = line.find("ready") {
            return line[at..].trim().to_owned();
        }
    }
}

#[test]
fn xdp_to_xdp_over_a_veth_pair() {
    let Some(veth) = Veth::new(false) else {
        return;
    };
    let listen = veth.addr(2, PORT);
    let mut rx = XdpReceiver::bind(&veth.b, listen, &config()).expect("bind the receiver");
    let mut tx = XdpSender::bind(
        &veth.a,
        veth.addr(1, 40000),
        listen,
        Some(veth.b_mac()),
        &config(),
    )
    .expect("bind the sender");
    println!(
        "receiver native={} zero_copy={}",
        rx.native(),
        rx.zero_copy()
    );
    let sender = std::thread::spawn(move || {
        for n in 0..FRAMES {
            assert_eq!(tx.send(n, &pulse(n), &envelope(n)).unwrap(), n);
        }
        tx
    });
    receive_all(&mut rx);
    let mut tx = sender.join().unwrap();
    tx.flush(Duration::from_secs(1))
        .expect("the TX ring empties");
    assert_eq!(rx.refused_count(), 0);
}

/// Frames sent in batches and received in batches arrive in order and
/// whole, each envelope handed over as it lies in the packet.
#[test]
fn batches_cross_xdp_to_xdp() {
    let Some(veth) = Veth::new(false) else {
        return;
    };
    let listen = veth.addr(2, PORT);
    let mut rx = XdpReceiver::bind(&veth.b, listen, &config()).expect("bind the receiver");
    let mut tx = XdpSender::bind(
        &veth.a,
        veth.addr(1, 40000),
        listen,
        Some(veth.b_mac()),
        &config(),
    )
    .expect("bind the sender");
    let sender = std::thread::spawn(move || {
        let pulses: Vec<Pulse> = (0..FRAMES).map(pulse).collect();
        let envelopes: Vec<Vec<u8>> = (0..FRAMES).map(envelope).collect();
        for chunk in (0..FRAMES as usize).collect::<Vec<_>>().chunks(32) {
            let frames: Vec<(&Pulse, &[u8])> = chunk
                .iter()
                .map(|&n| (&pulses[n], envelopes[n].as_slice()))
                .collect();
            assert_eq!(tx.send_batch(0, &frames).unwrap(), chunk[0] as u64);
        }
        tx.flush(Duration::from_secs(1)).unwrap();
    });
    let mut next = 0;
    while next < FRAMES {
        rx.recv_batch_with(64, |p, env, meta| {
            assert_eq!((meta.seq, p.chain_step), (next, next), "frame {next}");
            assert!(env == envelope(next), "envelope {next} changed");
            next += 1;
        })
        .expect("a batch");
    }
    sender.join().unwrap();
    assert_eq!((rx.refused_count(), rx.decode_error_count()), (0, 0));
}

/// An interface removed under a receiver waiting with no read timeout ends
/// its wait with an error, rather than leaving it waiting for ever.
#[test]
fn a_receiver_whose_interface_goes_away_returns_an_error() {
    let Some(veth) = Veth::new(false) else {
        return;
    };
    let mut cfg = config();
    cfg.read_timeout = None;
    let mut rx = XdpReceiver::bind(&veth.b, veth.addr(2, PORT), &cfg).expect("bind the receiver");
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 2048];
        let _ = done.send(rx.recv_into(&mut buf).map(|_| ()));
    });
    std::thread::sleep(Duration::from_millis(200));
    ip_ok(&["link", "del", &veth.a]);
    let got = finished
        .recv_timeout(Duration::from_secs(10))
        .expect("the receive still waits after its interface went away");
    assert!(got.is_err(), "a frame from an interface that is gone");
    println!("receive ended with {:?}", got.err());
}

#[test]
fn a_kernel_udp_sender_reaches_an_xdp_receiver() {
    let Some(veth) = Veth::new(true) else {
        return;
    };
    let listen = veth.addr(2, PORT);
    let mut child = veth.child("xdp-rx", &format!("{} {listen}", veth.b));
    println!("child {}", wait_ready(&mut child));
    let mut tx = pulse_wire_udp::Sender::bind(veth.addr(1, 0)).unwrap();
    tx.connect(listen).unwrap();
    for n in 0..FRAMES {
        tx.send(n, &pulse(n), &envelope(n)).unwrap();
        // The kernel's stack, unlike the TX ring, drops what the peer's
        // queue cannot take yet.
        if n % 32 == 31 {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let other = UdpSocket::bind(veth.addr(1, 0)).unwrap();
    other
        .send_to(b"to the kernel", veth.addr(2, OTHER_PORT))
        .unwrap();
    other.send_to(b"beside", veth.addr(3, PORT)).unwrap();
    wait_ok(child, "the XDP receiver");
}

#[test]
fn an_xdp_sender_reaches_a_kernel_udp_receiver() {
    let Some(veth) = Veth::new(true) else {
        return;
    };
    let listen = veth.addr(2, PORT);
    let mut child = veth.child("udp-rx", &format!("{} {listen}", veth.b));
    wait_ready(&mut child);
    let mut tx = XdpSender::bind(
        &veth.a,
        veth.addr(1, 40000),
        listen,
        Some(veth.b_mac()),
        &config(),
    )
    .expect("bind the sender");
    for n in 0..FRAMES {
        tx.send(n, &pulse(n), &envelope(n)).unwrap();
        if n % 32 == 31 {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    wait_ok(child, "the UDP receiver");
}
