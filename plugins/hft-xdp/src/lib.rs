// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! `pulse-wire-xdp` — the UDP transport's frames over AF_XDP, past the
//! kernel's network stack.
//!
//! An [`XdpSender`] writes each Pulse frame, with its Ethernet, IPv4 and
//! UDP headers, straight into a NIC queue's TX ring; an [`XdpReceiver`]
//! loads an XDP program that hands the queue's packets for its UDP port to
//! its own RX ring, and lets every other packet go on to the kernel. What
//! travels between the headers is [`pulse_wire_udp`]'s datagram, byte for
//! byte, so either end can be the kernel's UDP stack: an XDP sender talks
//! to a [`pulse_wire_udp::Receiver`], and a [`pulse_wire_udp::Sender`] to
//! an XDP receiver.
//!
//! 64-bit little-endian Linux only, from 5.9 (`BPF_LINK_CREATE` for XDP),
//! with `CAP_NET_ADMIN` and `CAP_BPF` (or root), and `CAP_IPC_LOCK` or an
//! `RLIMIT_MEMLOCK` above the UMEM (4 KiB a frame, 16 MiB by default),
//! which is locked memory. Elsewhere every constructor returns
//! [`XdpError::PlatformUnsupported`], and without the privileges
//! [`XdpError::Io`] names the step the kernel refused, so a tool can fall
//! back to the UDP transport. The receiver's program attaches in the
//! driver's own hook when the driver has one, and in the kernel's generic
//! hook otherwise ([`AttachMode`]); the receiver binds in zero-copy mode
//! when the driver supports it and copies otherwise, and the sender copies
//! unless asked for zero-copy ([`ZeroCopy`]).
//!
//! What this transport is: one socket per interface queue (a sender and a
//! receiver on one interface take a queue each, and one receiver's program
//! holds the interface), one peer, plain IPv4 with no options, no
//! fragmentation, and none of the UDP transport's options (backpressure,
//! NACK, FEC, sealing, fragmentation, delivery order). A frame lost is
//! lost. The peer's MAC comes from the kernel's neighbour table, or from
//! the caller; the longest envelope is what the interface's MTU leaves of
//! [`MAX_ENVELOPE`].
//!
//! ## Wire-format compatibility
//!
//! `pulse-wire-xdp` emits the same bytes as
//! [`pulse_wire_udp::wire::encode_into`] inside its UDP payload. Any future
//! divergence must be negotiated at the standard layer.

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
// `deny`, not `forbid`: the system calls (`sys`) and the rings' shared
// memory (`ring`) need `unsafe`, each module under an allow of its own.
#![deny(unsafe_code)]

pub mod frame;
pub mod prog;
#[cfg(xdp)]
mod ring;
#[cfg(xdp)]
mod sys;
#[cfg(xdp)]
mod xsk;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use core_transport::{PulseReceiver, PulseSender, TransportError};
use pulse_canon::Pulse;
use pulse_wire_udp::wire::{WireError, MAX_ENVELOPE};
use thiserror::Error;

use crate::frame::Mac;

/// Errors emitted by this crate.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum XdpError {
    /// The host operating system has no AF_XDP: everything but Linux.
    #[error(
        "AF_XDP is a Linux-only kernel feature; use the UDP transport \
         (pulse-wire-udp) on this host"
    )]
    PlatformUnsupported,
    /// A step the kernel refused, named: `EPERM` without `CAP_NET_ADMIN`
    /// and `CAP_BPF`, `EBUSY` when another XDP program holds the interface,
    /// `EOPNOTSUPP` for a mode the driver lacks.
    #[error("AF_XDP: {op}: {source}")]
    Io {
        /// What was being done.
        op: &'static str,
        /// The kernel's answer.
        #[source]
        source: std::io::Error,
    },
    /// The kernel's verifier refused the XDP program; its log says why.
    #[error("the kernel refused the XDP program: {source}\n{log}")]
    Verifier {
        /// The verifier's log.
        log: String,
        /// The load's error.
        #[source]
        source: std::io::Error,
    },
    /// The kernel's neighbour table has no MAC for the peer on the
    /// interface: reach it once over the kernel's stack (a ping), or pass
    /// its MAC.
    #[error("no neighbour entry for {ip} on {iface}; reach it once, or pass its MAC")]
    NoNeighbour {
        /// The peer's address.
        ip: Ipv4Addr,
        /// The interface.
        iface: String,
    },
    /// A configuration the transport cannot use, said why.
    #[error("AF_XDP: {0}")]
    InvalidConfig(&'static str),
    /// No UMEM frame or TX ring entry came free within the send timeout.
    #[error("AF_XDP: the TX ring stayed full")]
    TxFull,
    /// The envelope is longer than a frame carries.
    #[error("envelope too large: {len} bytes, max {max}")]
    EnvelopeTooLarge {
        /// Its length.
        len: usize,
        /// [`MAX_ENVELOPE`].
        max: usize,
    },
    /// The caller's buffer is shorter than the envelope, which is consumed.
    #[error("envelope buffer too small: needed {needed} bytes, provided {provided}")]
    BufferTooSmall {
        /// The envelope's length.
        needed: usize,
        /// The buffer's.
        provided: usize,
    },
    /// A datagram to the receiver's port that is not a Pulse frame.
    #[error("frame decode: {0}")]
    Decode(#[from] WireError),
}

/// Where the receiver's XDP program attaches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AttachMode {
    /// In the driver's hook if it has one, else the generic hook.
    #[default]
    Auto,
    /// In the driver's hook, or fail.
    Native,
    /// In the kernel's generic hook, after the driver has built its
    /// socket buffer: slower, and every interface has it.
    Generic,
}

/// Whether the sockets share their frames with the NIC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ZeroCopy {
    /// Zero-copy if the driver supports it, else copy.
    #[default]
    Auto,
    /// Zero-copy, or fail.
    Required,
    /// Copy every packet between the UMEM and the driver.
    Off,
}

/// The queue and the sizes of a sender's or a receiver's socket.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XdpConfig {
    /// The NIC queue to bind. Default 0.
    pub queue: u32,
    /// UMEM frames, 4 KiB each. A power of two; default 4096.
    pub frames: u32,
    /// Entries in each ring. A power of two; default 2048.
    pub ring: u32,
    /// Where the receiver's program attaches. Default auto.
    pub attach: AttachMode,
    /// Default auto: the receiver zero-copy when the driver supports it,
    /// the sender copy, since some drivers send zero-copy only with an XDP
    /// program attached to the interface, which a sender does not attach.
    pub zero_copy: ZeroCopy,
    /// How long a receive waits for a frame; `None` (the default) waits
    /// forever.
    pub read_timeout: Option<Duration>,
    /// How long a send waits for a free frame. Default 1 s.
    pub send_timeout: Duration,
    /// The receiver busy-polls its queue, for this long at a time, rather
    /// than waiting for its interrupt: a receive spins on the queue's NAPI
    /// loop, which costs a core and can cut the time to the first packet
    /// (`SO_PREFER_BUSY_POLL`, Linux 5.11). Pair it with the interface's
    /// `napi_defer_hard_irqs` and `gro_flush_timeout`. Default `None`.
    pub busy_poll: Option<Duration>,
}

impl Default for XdpConfig {
    fn default() -> Self {
        Self {
            queue: 0,
            frames: 4096,
            ring: 2048,
            attach: AttachMode::Auto,
            zero_copy: ZeroCopy::Auto,
            read_timeout: None,
            send_timeout: Duration::from_secs(1),
            busy_poll: None,
        }
    }
}

impl XdpConfig {
    fn check(&self) -> Result<(), XdpError> {
        if !self.frames.is_power_of_two() || !self.ring.is_power_of_two() {
            return Err(XdpError::InvalidConfig(
                "frames and ring entries must be powers of two",
            ));
        }
        Ok(())
    }
}

/// What came with a received frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XdpMeta {
    /// The frame's sequence number.
    pub seq: u64,
    /// The sender's clock when it sent the frame.
    pub send_ns: u64,
    /// Where the datagram came from.
    pub peer: SocketAddrV4,
}

/// Never constructed: the inside of a sender or receiver off Linux.
#[cfg(not(xdp))]
enum Unsupported {}

/// AF_XDP-bound producer half of a `pulse-wire` connection.
pub struct XdpSender {
    #[cfg(xdp)]
    tx: xsk::TxPath,
    #[cfg(not(xdp))]
    _never: Unsupported,
    ends: frame::Endpoints,
    seq: u64,
    #[cfg(xdp)]
    ip_id: u16,
    /// The longest envelope the interface's MTU carries.
    max_envelope: usize,
    #[cfg(xdp)]
    send_timeout: Duration,
    #[cfg(xdp)]
    scratch: Vec<u8>,
}

impl XdpSender {
    /// Send from `local` on interface `iface` to `peer`, whose MAC is
    /// `peer_mac` or, if `None`, the kernel's neighbour table's entry for
    /// it on `iface`.
    ///
    /// # Errors
    /// [`XdpError::PlatformUnsupported`] off Linux; [`XdpError::Io`] for
    /// the step the kernel refused; [`XdpError::NoNeighbour`];
    /// [`XdpError::InvalidConfig`].
    pub fn bind(
        iface: &str,
        local: SocketAddrV4,
        peer: SocketAddrV4,
        peer_mac: Option<Mac>,
        cfg: &XdpConfig,
    ) -> Result<Self, XdpError> {
        cfg.check()?;
        #[cfg(xdp)]
        {
            if local.ip().is_unspecified() {
                return Err(XdpError::InvalidConfig("the local address must be given"));
            }
            let ifindex = sys::ifindex(iface).map_err(|source| XdpError::Io {
                op: "find the interface",
                source,
            })?;
            let src_mac = interface_mac(iface)?;
            // A frame is the UDP payload of one IPv4 packet the MTU carries.
            let max_envelope = interface_mtu(iface)?
                .checked_sub(
                    frame::IPV4_HEADER
                        + frame::UDP_HEADER
                        + pulse_wire_udp::wire::HEADER_BYTES
                        + pulse_wire_udp::wire::TRAILER_BYTES,
                )
                .ok_or(XdpError::InvalidConfig(
                    "the interface's MTU carries no frame",
                ))?
                .min(MAX_ENVELOPE);
            let dst_mac = match peer_mac {
                Some(mac) => mac,
                None => neighbour_mac(iface, *peer.ip())?,
            };
            let tx = xsk::TxPath::open(ifindex, cfg.queue, cfg.frames, cfg.ring, cfg.zero_copy)?;
            Ok(Self {
                tx,
                ends: frame::Endpoints {
                    src_mac,
                    dst_mac,
                    src: local,
                    dst: peer,
                },
                seq: 0,
                ip_id: 0,
                max_envelope,
                send_timeout: cfg.send_timeout,
                scratch: Vec::with_capacity(pulse_wire_udp::MTU_BYTES),
            })
        }
        #[cfg(not(xdp))]
        {
            let _ = (iface, local, peer, peer_mac, cfg);
            Err(XdpError::PlatformUnsupported)
        }
    }

    /// Send `pulse` and `envelope` as the next frame, stamped `send_ns`;
    /// returns its sequence number.
    ///
    /// # Errors
    /// [`XdpError::EnvelopeTooLarge`] over [`Self::max_envelope`], with
    /// nothing sent; [`XdpError::TxFull`]; [`XdpError::Io`] for a failed
    /// wakeup, an interface gone down included.
    pub fn send(&mut self, send_ns: u64, pulse: &Pulse, envelope: &[u8]) -> Result<u64, XdpError> {
        #[cfg(not(xdp))]
        {
            let _ = (send_ns, pulse, envelope);
            match self._never {}
        }
        #[cfg(xdp)]
        self.send_frame(send_ns, pulse, envelope)
    }

    #[cfg(xdp)]
    fn send_frame(
        &mut self,
        send_ns: u64,
        pulse: &Pulse,
        envelope: &[u8],
    ) -> Result<u64, XdpError> {
        if envelope.len() > self.max_envelope {
            return Err(XdpError::EnvelopeTooLarge {
                len: envelope.len(),
                max: self.max_envelope,
            });
        }
        let seq = self.seq;
        self.scratch.clear();
        pulse_wire_udp::wire::encode_into(seq, send_ns, pulse, envelope, &mut self.scratch)?;
        let (ends, ip_id, payload) = (self.ends, self.ip_id, &self.scratch);
        self.tx.send(self.send_timeout, |buf| {
            frame::write_headers(buf, &ends, ip_id, payload.len());
            buf[frame::HEADERS..frame::HEADERS + payload.len()].copy_from_slice(payload);
            frame::HEADERS + payload.len()
        })?;
        self.seq = self.seq.wrapping_add(1);
        self.ip_id = self.ip_id.wrapping_add(1);
        Ok(seq)
    }

    /// Send each `(pulse, envelope)` of `frames` as the next frame, in
    /// order, stamped `send_ns`, waking the kernel once per run of free
    /// frames rather than once per frame; returns the first one's sequence
    /// number.
    ///
    /// # Errors
    /// [`XdpError::EnvelopeTooLarge`] for any envelope over
    /// [`Self::max_envelope`], with nothing sent; [`XdpError::TxFull`],
    /// with the frames before handed over; [`XdpError::Io`] for a failed
    /// wakeup.
    pub fn send_batch(
        &mut self,
        send_ns: u64,
        frames: &[(&Pulse, &[u8])],
    ) -> Result<u64, XdpError> {
        #[cfg(not(xdp))]
        {
            let _ = (send_ns, frames);
            match self._never {}
        }
        #[cfg(xdp)]
        {
            if let Some((_, long)) = frames.iter().find(|(_, e)| e.len() > self.max_envelope) {
                return Err(XdpError::EnvelopeTooLarge {
                    len: long.len(),
                    max: self.max_envelope,
                });
            }
            let first = self.seq;
            let (ends, ip_id, scratch) = (self.ends, self.ip_id, &mut self.scratch);
            self.tx
                .send_batch(self.send_timeout, frames.len(), |i, buf| {
                    let (pulse, envelope) = frames[i];
                    scratch.clear();
                    let seq = first.wrapping_add(i as u64);
                    // Cannot fail: every envelope's length is checked above.
                    if pulse_wire_udp::wire::encode_into(seq, send_ns, pulse, envelope, scratch)
                        .is_err()
                    {
                        return 0;
                    }
                    frame::write_headers(buf, &ends, ip_id.wrapping_add(i as u16), scratch.len());
                    buf[frame::HEADERS..frame::HEADERS + scratch.len()].copy_from_slice(scratch);
                    frame::HEADERS + scratch.len()
                })?;
            self.seq = self.seq.wrapping_add(frames.len() as u64);
            self.ip_id = self.ip_id.wrapping_add(frames.len() as u16);
            Ok(first)
        }
    }

    /// Wake the kernel until it has sent every frame handed to it, or
    /// `timeout` passes. A send hands its frame over and returns; a frame
    /// the kernel was too busy to take then waits for the next send, or
    /// for this.
    ///
    /// # Errors
    /// [`XdpError::TxFull`] with frames still waiting at the timeout;
    /// [`XdpError::Io`] for a failed wakeup.
    pub fn flush(&mut self, timeout: Duration) -> Result<(), XdpError> {
        #[cfg(not(xdp))]
        {
            let _ = timeout;
            match self._never {}
        }
        #[cfg(xdp)]
        self.tx.flush(timeout)
    }

    /// The longest envelope a send takes: what the interface's MTU leaves
    /// of [`MAX_ENVELOPE`].
    pub fn max_envelope(&self) -> usize {
        self.max_envelope
    }

    /// The next frame's sequence number.
    pub fn next_seq(&self) -> u64 {
        self.seq
    }

    /// Where frames go from and to.
    pub fn endpoints(&self) -> frame::Endpoints {
        self.ends
    }

    /// True when the socket shares its frames with the NIC.
    pub fn zero_copy(&self) -> bool {
        #[cfg(xdp)]
        return self.tx.sock.zero_copy;
        #[cfg(not(xdp))]
        match self._never {}
    }
}

/// AF_XDP-bound consumer half of a `pulse-wire` connection.
pub struct XdpReceiver {
    #[cfg(xdp)]
    rx: xsk::RxPath,
    #[cfg(not(xdp))]
    _never: Unsupported,
    listen: SocketAddrV4,
    #[cfg(xdp)]
    read_timeout: Option<Duration>,
    refused: u64,
    decode_errors: u64,
}

impl XdpReceiver {
    /// Receive the UDP datagrams to `listen` (an unspecified address takes
    /// any) that arrive on interface `iface`'s queue `cfg.queue`.
    ///
    /// # Errors
    /// [`XdpError::PlatformUnsupported`] off Linux; [`XdpError::Io`] for
    /// the step the kernel refused; [`XdpError::Verifier`];
    /// [`XdpError::InvalidConfig`].
    pub fn bind(iface: &str, listen: SocketAddrV4, cfg: &XdpConfig) -> Result<Self, XdpError> {
        cfg.check()?;
        #[cfg(xdp)]
        {
            if listen.port() == 0 {
                return Err(XdpError::InvalidConfig("the port must be given"));
            }
            let ifindex = sys::ifindex(iface).map_err(|source| XdpError::Io {
                op: "find the interface",
                source,
            })?;
            let addr = (!listen.ip().is_unspecified()).then_some(*listen.ip());
            let rx = xsk::RxPath::open(
                ifindex,
                cfg.queue,
                listen.port(),
                addr,
                cfg.frames,
                cfg.ring,
                cfg.attach,
                cfg.zero_copy,
            )?;
            let mut rx = rx;
            if let Some(spin) = cfg.busy_poll {
                let usecs = u32::try_from(spin.as_micros()).unwrap_or(u32::MAX);
                rx.enable_busy_poll(usecs, 64)?;
            }
            Ok(Self {
                rx,
                listen,
                read_timeout: cfg.read_timeout,
                refused: 0,
                decode_errors: 0,
            })
        }
        #[cfg(not(xdp))]
        {
            let _ = (iface, listen, cfg);
            Err(XdpError::PlatformUnsupported)
        }
    }

    /// Wait for the next frame and copy its envelope into `envelope`,
    /// returning the Pulse, the envelope's length and the metadata. A packet
    /// the XDP program passed that is not a datagram for this receiver is
    /// counted ([`Self::refused_count`]) and skipped.
    ///
    /// # Errors
    /// [`XdpError::Io`] with `TimedOut` after the read timeout, which
    /// packets refused on the way do not extend, or with the socket's
    /// error, an interface removed under it included;
    /// [`XdpError::Decode`] for a datagram to this port that is not a Pulse
    /// frame; [`XdpError::BufferTooSmall`], the frame consumed.
    pub fn recv_into(&mut self, envelope: &mut [u8]) -> Result<(Pulse, usize, XdpMeta), XdpError> {
        #[cfg(xdp)]
        let deadline = self.read_timeout.map(|t| std::time::Instant::now() + t);
        #[cfg(xdp)]
        loop {
            let listen = self.listen;
            let got = self.rx.next(deadline, |packet| {
                let datagram = match frame::parse(packet, listen) {
                    Ok(d) => d,
                    Err(_) => return None,
                };
                Some(take_frame(datagram, envelope))
            })?;
            match got {
                None => self.refused += 1,
                Some(result) => return result,
            }
        }
        #[cfg(not(xdp))]
        {
            let _ = envelope;
            match self._never {}
        }
    }

    /// Datagrams to this receiver that were not Pulse frames, skipped by
    /// [`Self::recv_batch_with`]. Diagnostic.
    pub fn decode_error_count(&self) -> u64 {
        self.decode_errors
    }

    /// Hand each frame of the next run the kernel delivered, up to `max`,
    /// to `f`: its Pulse, its envelope as it lies in the received packet,
    /// and its metadata. No envelope is copied, and the frames go back to
    /// the kernel together after the last. Waits up to the read timeout
    /// for at least one frame; returns how many `f` saw. Packets that are
    /// not datagrams for this receiver are counted
    /// ([`Self::refused_count`]), and datagrams that are not Pulse frames
    /// counted too ([`Self::decode_error_count`]), and skipped.
    ///
    /// # Errors
    /// [`XdpError::Io`] with `TimedOut` after the read timeout, or with the
    /// socket's error, an interface removed under it included.
    pub fn recv_batch_with(
        &mut self,
        max: usize,
        mut f: impl FnMut(&Pulse, &[u8], XdpMeta),
    ) -> Result<usize, XdpError> {
        #[cfg(not(xdp))]
        {
            let _ = (max, &mut f);
            match self._never {}
        }
        #[cfg(xdp)]
        {
            let deadline = self.read_timeout.map(|t| std::time::Instant::now() + t);
            let max = u32::try_from(max).unwrap_or(u32::MAX);
            loop {
                let (listen, mut seen, mut refused, mut bad) = (self.listen, 0usize, 0u64, 0u64);
                self.rx.next_batch(deadline, max, |packet| {
                    let Ok(datagram) = frame::parse(packet, listen) else {
                        refused += 1;
                        return;
                    };
                    let Ok(fr) = pulse_wire_udp::wire::decode(datagram.payload) else {
                        bad += 1;
                        return;
                    };
                    let Some(pulse) = Pulse::from_bytes(&fr.pulse) else {
                        bad += 1;
                        return;
                    };
                    let meta = XdpMeta {
                        seq: fr.seq,
                        send_ns: fr.send_ns,
                        peer: datagram.src,
                    };
                    f(&pulse, fr.envelope, meta);
                    seen += 1;
                })?;
                self.refused += refused;
                self.decode_errors += bad;
                if seen > 0 {
                    return Ok(seen);
                }
            }
        }
    }

    /// Packets the program passed to this socket that were not datagrams
    /// for it. Diagnostic.
    pub fn refused_count(&self) -> u64 {
        self.refused
    }

    /// The address and port it receives on.
    pub fn listen_addr(&self) -> SocketAddrV4 {
        self.listen
    }

    /// True when the XDP program runs in the driver's own hook.
    pub fn native(&self) -> bool {
        #[cfg(xdp)]
        return self.rx.native;
        #[cfg(not(xdp))]
        match self._never {}
    }

    /// True when the socket shares its frames with the NIC.
    pub fn zero_copy(&self) -> bool {
        #[cfg(xdp)]
        return self.rx.sock.zero_copy;
        #[cfg(not(xdp))]
        match self._never {}
    }

    /// Packets the kernel dropped because the RX ring was full, or because
    /// no frame was lent for them. Diagnostic.
    pub fn kernel_dropped_count(&self) -> u64 {
        #[cfg(xdp)]
        return self.rx.sock.statistics().map_or(0, |s| {
            s.rx_dropped + s.rx_ring_full + s.rx_fill_ring_empty_descs
        });
        #[cfg(not(xdp))]
        match self._never {}
    }
}

/// Decode a datagram's frame and copy its envelope into `envelope`.
#[cfg(xdp)]
fn take_frame(
    datagram: frame::Datagram<'_>,
    envelope: &mut [u8],
) -> Result<(Pulse, usize, XdpMeta), XdpError> {
    let f = pulse_wire_udp::wire::decode(datagram.payload)?;
    let pulse = Pulse::from_bytes(&f.pulse).ok_or(XdpError::Decode(WireError::BufferTooSmall {
        got: f.pulse.len(),
        min: pulse_canon::PULSE_WIRE_SIZE,
    }))?;
    let n = f.envelope.len();
    if n > envelope.len() {
        return Err(XdpError::BufferTooSmall {
            needed: n,
            provided: envelope.len(),
        });
    }
    envelope[..n].copy_from_slice(f.envelope);
    Ok((
        pulse,
        n,
        XdpMeta {
            seq: f.seq,
            send_ns: f.send_ns,
            peer: datagram.src,
        },
    ))
}

/// The MAC of interface `iface`, from sysfs.
#[cfg(xdp)]
fn interface_mac(iface: &str) -> Result<Mac, XdpError> {
    let path = format!("/sys/class/net/{iface}/address");
    let text = std::fs::read_to_string(&path).map_err(|source| XdpError::Io {
        op: "read the interface's MAC",
        source,
    })?;
    frame::parse_mac(&text).ok_or(XdpError::InvalidConfig("the interface has no Ethernet MAC"))
}

/// The MTU of interface `iface`, from sysfs.
#[cfg(xdp)]
fn interface_mtu(iface: &str) -> Result<usize, XdpError> {
    let path = format!("/sys/class/net/{iface}/mtu");
    let text = std::fs::read_to_string(&path).map_err(|source| XdpError::Io {
        op: "read the interface's MTU",
        source,
    })?;
    text.trim()
        .parse()
        .map_err(|_| XdpError::InvalidConfig("the interface's MTU does not read"))
}

/// The MAC the kernel's neighbour table holds for `ip` on `iface`.
#[cfg(xdp)]
fn neighbour_mac(iface: &str, ip: Ipv4Addr) -> Result<Mac, XdpError> {
    let table = std::fs::read_to_string("/proc/net/arp").map_err(|source| XdpError::Io {
        op: "read the neighbour table",
        source,
    })?;
    neighbour_in(&table, iface, ip).ok_or_else(|| XdpError::NoNeighbour {
        ip,
        iface: iface.to_owned(),
    })
}

/// The complete entry for `ip` on `iface` in `table`, `/proc/net/arp`'s
/// text: address, hardware type, flags, MAC, mask, device.
pub fn neighbour_in(table: &str, iface: &str, ip: Ipv4Addr) -> Option<Mac> {
    const COMPLETE: u32 = 0x2;
    table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (addr, flags, mac, dev) = (f.first()?, f.get(2)?, f.get(3)?, f.get(5)?);
        let flags = u32::from_str_radix(flags.trim_start_matches("0x"), 16).ok()?;
        (addr.parse::<Ipv4Addr>().ok()? == ip && *dev == iface && flags & COMPLETE != 0)
            .then(|| frame::parse_mac(mac))
            .flatten()
    })
}

impl From<XdpError> for TransportError {
    fn from(e: XdpError) -> Self {
        match e {
            XdpError::Io { source, .. } => TransportError::Io(source),
            XdpError::EnvelopeTooLarge { len, max } => {
                TransportError::EnvelopeTooLarge { len, max }
            }
            XdpError::BufferTooSmall { needed, provided } => {
                TransportError::BufferTooSmall { needed, provided }
            }
            XdpError::Decode(d) => TransportError::Decode(d.to_string()),
            XdpError::TxFull => TransportError::Send(e.to_string()),
            other => TransportError::Recv(other.to_string()),
        }
    }
}

impl PulseSender for XdpSender {
    fn send(&mut self, pulse: &Pulse, envelope: &[u8]) -> Result<(), TransportError> {
        XdpSender::send(self, 0, pulse, envelope)?;
        Ok(())
    }

    fn max_envelope(&self) -> usize {
        self.max_envelope
    }

    fn local_addr(&self) -> Option<SocketAddr> {
        Some(SocketAddr::V4(self.ends.src))
    }
}

impl PulseReceiver for XdpReceiver {
    fn recv_into(&mut self, envelope: &mut [u8]) -> Result<(Pulse, usize), TransportError> {
        let (pulse, n, _) = XdpReceiver::recv_into(self, envelope)?;
        Ok((pulse, n))
    }

    fn max_envelope(&self) -> usize {
        MAX_ENVELOPE
    }

    fn local_addr(&self) -> Option<SocketAddr> {
        Some(SocketAddr::V4(self.listen))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_neighbour_table_gives_complete_entries_on_the_interface() {
        let table = "IP address       HW type     Flags       HW address            Mask     Device\n\
                     10.77.0.2        0x1         0x2         aa:bb:cc:dd:ee:01     *        v0\n\
                     10.77.0.3        0x1         0x0         00:00:00:00:00:00     *        v0\n\
                     10.77.0.2        0x1         0x2         aa:bb:cc:dd:ee:02     *        eth0\n";
        let ip = |s: &str| s.parse().unwrap();
        assert_eq!(
            neighbour_in(table, "v0", ip("10.77.0.2")),
            Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01])
        );
        assert_eq!(
            neighbour_in(table, "eth0", ip("10.77.0.2")),
            Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x02])
        );
        assert_eq!(
            neighbour_in(table, "v0", ip("10.77.0.3")),
            None,
            "incomplete"
        );
        assert_eq!(neighbour_in(table, "v1", ip("10.77.0.2")), None);
    }

    #[test]
    fn a_config_needs_powers_of_two() {
        let mut cfg = XdpConfig::default();
        assert!(cfg.check().is_ok());
        cfg.frames = 3000;
        assert!(matches!(cfg.check(), Err(XdpError::InvalidConfig(_))));
    }

    #[cfg(not(xdp))]
    #[test]
    fn off_linux_nothing_binds() {
        let cfg = XdpConfig::default();
        let any: SocketAddrV4 = "10.0.0.1:9000".parse().unwrap();
        assert!(matches!(
            XdpSender::bind("lo", any, any, None, &cfg),
            Err(XdpError::PlatformUnsupported)
        ));
        assert!(matches!(
            XdpReceiver::bind("lo", any, &cfg),
            Err(XdpError::PlatformUnsupported)
        ));
    }
}
