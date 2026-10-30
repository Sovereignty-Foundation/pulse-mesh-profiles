// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! The Ethernet, IPv4 and UDP headers around a Pulse frame: written in
//! front of the frame for the TX ring, and read off a packet from the RX
//! ring. What travels between the headers is the UDP transport's datagram,
//! byte for byte ([`pulse_wire_udp::wire`]), so either end can be the
//! kernel's UDP stack.
//!
//! Only plain IPv4 is built and taken: a 20-byte header with no options,
//! not fragmented. The XDP program sends anything else on to the kernel.

use std::net::{Ipv4Addr, SocketAddrV4};

/// Ethernet header: destination and source MAC, EtherType.
pub const ETH_HEADER: usize = 14;
/// IPv4 header without options.
pub const IPV4_HEADER: usize = 20;
/// UDP header.
pub const UDP_HEADER: usize = 8;
/// Every header before the UDP payload.
pub const HEADERS: usize = ETH_HEADER + IPV4_HEADER + UDP_HEADER;
/// The EtherType of IPv4.
pub const ETHERTYPE_IPV4: u16 = 0x0800;
/// The IPv4 protocol number of UDP.
pub const IPPROTO_UDP: u8 = 17;
/// The hop limit set on packets sent.
const TTL: u8 = 64;

/// A MAC address.
pub type Mac = [u8; 6];

/// Who a packet goes from and to, at the link and at UDP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// The sending interface's MAC.
    pub src_mac: Mac,
    /// The next hop's MAC: the peer's, on one link.
    pub dst_mac: Mac,
    /// The sending address and port.
    pub src: SocketAddrV4,
    /// The destination address and port.
    pub dst: SocketAddrV4,
}

/// Write the headers for a UDP payload of `payload_len` bytes into
/// `buf[..HEADERS]`, the IPv4 identification `ip_id`, the Don't Fragment
/// bit set and the UDP checksum left zero, which IPv4 allows: the Pulse
/// frame carries a CRC32C of its own.
///
/// # Panics
/// If `buf` is shorter than [`HEADERS`] or the payload too long for an IPv4
/// packet; callers size both from the MTU.
pub fn write_headers(buf: &mut [u8], ends: &Endpoints, ip_id: u16, payload_len: usize) {
    let udp_len = UDP_HEADER + payload_len;
    let ip_len = IPV4_HEADER + udp_len;
    assert!(ip_len <= usize::from(u16::MAX), "an IPv4 packet that long");
    let (eth, rest) = buf[..HEADERS].split_at_mut(ETH_HEADER);
    let (ip, udp) = rest.split_at_mut(IPV4_HEADER);

    eth[0..6].copy_from_slice(&ends.dst_mac);
    eth[6..12].copy_from_slice(&ends.src_mac);
    eth[12..14].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());

    ip[0] = 0x45;
    ip[1] = 0;
    ip[2..4].copy_from_slice(&(ip_len as u16).to_be_bytes());
    ip[4..6].copy_from_slice(&ip_id.to_be_bytes());
    ip[6..8].copy_from_slice(&0x4000u16.to_be_bytes());
    ip[8] = TTL;
    ip[9] = IPPROTO_UDP;
    ip[10..12].fill(0);
    ip[12..16].copy_from_slice(&ends.src.ip().octets());
    ip[16..20].copy_from_slice(&ends.dst.ip().octets());
    let checksum = ipv4_checksum(ip);
    ip[10..12].copy_from_slice(&checksum.to_be_bytes());

    udp[0..2].copy_from_slice(&ends.src.port().to_be_bytes());
    udp[2..4].copy_from_slice(&ends.dst.port().to_be_bytes());
    udp[4..6].copy_from_slice(&(udp_len as u16).to_be_bytes());
    udp[6..8].fill(0);
}

/// The IPv4 header checksum of `header`: the ones' complement of the ones'
/// complement sum of its 16-bit words. Over a header whose checksum field
/// is filled in, it is zero exactly when that field is right.
pub fn ipv4_checksum(header: &[u8]) -> u16 {
    let mut sum: u32 = header
        .chunks(2)
        .map(|w| u32::from(u16::from_be_bytes([w[0], *w.get(1).unwrap_or(&0)])))
        .sum();
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// A UDP datagram read off a packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Datagram<'a> {
    /// Where it came from.
    pub src: SocketAddrV4,
    /// Where it went.
    pub dst: SocketAddrV4,
    /// Its payload.
    pub payload: &'a [u8],
}

/// Why a packet is not a datagram for the receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Shorter than the headers, or than its headers say it is.
    Truncated,
    /// Not IPv4, or IPv4 with options.
    NotPlainIpv4,
    /// The IPv4 header checksum does not match.
    BadChecksum,
    /// Not UDP.
    NotUdp,
    /// A fragment.
    Fragment,
    /// To another port or address than the receiver's.
    NotOurs,
}

/// Read the UDP datagram from `packet`, an Ethernet frame, if it is plain
/// IPv4 to `listen` (an unspecified address takes any): every length the
/// headers give must fit, the IPv4 checksum must hold, and bytes after the
/// IPv4 packet (Ethernet padding) are not the datagram's.
///
/// # Errors
/// The first check that fails, as [`Refused`].
pub fn parse(packet: &[u8], listen: SocketAddrV4) -> Result<Datagram<'_>, Refused> {
    if packet.len() < HEADERS {
        return Err(Refused::Truncated);
    }
    if u16::from_be_bytes([packet[12], packet[13]]) != ETHERTYPE_IPV4 {
        return Err(Refused::NotPlainIpv4);
    }
    let ip = &packet[ETH_HEADER..];
    if ip[0] != 0x45 {
        return Err(Refused::NotPlainIpv4);
    }
    let ip_len = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
    if ip_len < IPV4_HEADER + UDP_HEADER || ip_len > ip.len() {
        return Err(Refused::Truncated);
    }
    if ipv4_checksum(&ip[..IPV4_HEADER]) != 0 {
        return Err(Refused::BadChecksum);
    }
    if ip[9] != IPPROTO_UDP {
        return Err(Refused::NotUdp);
    }
    if u16::from_be_bytes([ip[6], ip[7]]) & 0x3fff != 0 {
        return Err(Refused::Fragment);
    }
    let src_ip = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
    let dst_ip = Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]);
    let udp = &ip[IPV4_HEADER..ip_len];
    let src_port = u16::from_be_bytes([udp[0], udp[1]]);
    let dst_port = u16::from_be_bytes([udp[2], udp[3]]);
    let udp_len = usize::from(u16::from_be_bytes([udp[4], udp[5]]));
    if udp_len < UDP_HEADER || udp_len > udp.len() {
        return Err(Refused::Truncated);
    }
    if dst_port != listen.port() || !(listen.ip().is_unspecified() || *listen.ip() == dst_ip) {
        return Err(Refused::NotOurs);
    }
    Ok(Datagram {
        src: SocketAddrV4::new(src_ip, src_port),
        dst: SocketAddrV4::new(dst_ip, dst_port),
        payload: &udp[UDP_HEADER..udp_len],
    })
}

/// A MAC address from its text, six hex pairs joined by colons.
pub fn parse_mac(text: &str) -> Option<Mac> {
    let mut mac = [0u8; 6];
    let mut parts = text.trim().split(':');
    for byte in &mut mac {
        *byte = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    parts.next().is_none().then_some(mac)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn ends() -> Endpoints {
        Endpoints {
            src_mac: [2, 0, 0, 0, 0, 1],
            dst_mac: [2, 0, 0, 0, 0, 2],
            src: "10.77.0.1:40000".parse().unwrap(),
            dst: "10.77.0.2:9000".parse().unwrap(),
        }
    }

    fn packet(payload: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; HEADERS + payload.len()];
        write_headers(&mut buf, &ends(), 7, payload.len());
        buf[HEADERS..].copy_from_slice(payload);
        buf
    }

    #[test]
    fn a_packet_written_reads_back() {
        let p = packet(b"pulse");
        let d = parse(&p, "0.0.0.0:9000".parse().unwrap()).unwrap();
        assert_eq!(
            (d.src, d.dst, d.payload),
            (ends().src, ends().dst, &b"pulse"[..])
        );
        assert_eq!(ipv4_checksum(&p[ETH_HEADER..ETH_HEADER + IPV4_HEADER]), 0);
        // Ethernet padding after the IPv4 packet is not the payload's.
        let mut padded = p.clone();
        padded.resize(60, 0);
        assert_eq!(parse(&padded, ends().dst).unwrap().payload, b"pulse");
    }

    #[test]
    fn a_known_header_has_the_known_checksum() {
        // RFC 1071's worked example, as a 20-byte header.
        let h = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        assert_eq!(ipv4_checksum(&h), 0xb861);
    }

    #[test]
    fn what_is_not_a_datagram_for_the_receiver_is_refused() {
        let listen: SocketAddrV4 = "10.77.0.2:9000".parse().unwrap();
        let good = packet(b"x");
        let bent = |at: usize, v: u8| {
            let mut p = good.clone();
            p[at] = v;
            p
        };
        assert_eq!(parse(&good[..HEADERS - 1], listen), Err(Refused::Truncated));
        assert_eq!(parse(&bent(12, 0x86), listen), Err(Refused::NotPlainIpv4));
        assert_eq!(parse(&bent(14, 0x46), listen), Err(Refused::NotPlainIpv4));
        assert_eq!(parse(&bent(24, 0xff), listen), Err(Refused::BadChecksum));
        assert_eq!(
            parse(&good, "10.77.0.2:9001".parse().unwrap()),
            Err(Refused::NotOurs)
        );
        assert_eq!(
            parse(&good, "10.77.0.3:9000".parse().unwrap()),
            Err(Refused::NotOurs)
        );
        // A fragment, rewritten with its checksum fixed.
        let mut frag = good.clone();
        frag[ETH_HEADER + 6] = 0x20;
        frag[ETH_HEADER + 10..ETH_HEADER + 12].fill(0);
        let sum = ipv4_checksum(&frag[ETH_HEADER..ETH_HEADER + IPV4_HEADER]);
        frag[ETH_HEADER + 10..ETH_HEADER + 12].copy_from_slice(&sum.to_be_bytes());
        assert_eq!(parse(&frag, listen), Err(Refused::Fragment));
    }

    /// A small deterministic generator for the property test below.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            // xorshift64*
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn addr(&mut self) -> SocketAddrV4 {
            SocketAddrV4::new(Ipv4Addr::from(self.next() as u32), self.next() as u16)
        }
    }

    /// What `parse` returns holds together: the datagram is to the
    /// receiver, its header's checksum holds, and its payload starts after
    /// the headers and ends inside the IPv4 packet, as long as UDP says.
    fn holds(packet: &[u8], listen: SocketAddrV4) {
        let Ok(d) = parse(packet, listen) else {
            return;
        };
        assert_eq!(d.dst.port(), listen.port());
        assert!(listen.ip().is_unspecified() || d.dst.ip() == listen.ip());
        assert_eq!(
            ipv4_checksum(&packet[ETH_HEADER..ETH_HEADER + IPV4_HEADER]),
            0
        );
        let start = d.payload.as_ptr() as usize - packet.as_ptr() as usize;
        assert_eq!(start, HEADERS);
        let ip_len = usize::from(u16::from_be_bytes([packet[16], packet[17]]));
        assert!(start + d.payload.len() <= ETH_HEADER + ip_len);
        let udp_len = usize::from(u16::from_be_bytes([packet[38], packet[39]]));
        assert_eq!(d.payload.len() + UDP_HEADER, udp_len);
    }

    /// Packets built from random endpoints and payloads, padded, then left
    /// whole, bent at one byte or cut short; and random bytes. A whole one
    /// reads back; whatever is returned holds together; nothing panics.
    #[test]
    fn random_packets_read_back_whole_and_hold_together_bent() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for _ in 0..20_000 {
            let ends = Endpoints {
                src_mac: [rng.next() as u8; 6],
                dst_mac: [rng.next() as u8; 6],
                src: rng.addr(),
                dst: rng.addr(),
            };
            let payload: Vec<u8> = (0..rng.below(1500)).map(|_| rng.next() as u8).collect();
            let mut p = vec![0u8; HEADERS + payload.len()];
            write_headers(&mut p, &ends, rng.next() as u16, payload.len());
            p[HEADERS..].copy_from_slice(&payload);
            p.extend((0..rng.below(64)).map(|_| rng.next() as u8));
            for listen in [
                ends.dst,
                SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, ends.dst.port()),
            ] {
                let d = parse(&p, listen).unwrap();
                assert_eq!(
                    (d.src, d.dst, d.payload),
                    (ends.src, ends.dst, payload.as_slice())
                );
            }
            let elsewhere = SocketAddrV4::new(*ends.dst.ip(), ends.dst.port().wrapping_add(1));
            assert_eq!(parse(&p, elsewhere), Err(Refused::NotOurs));
            if rng.below(2) == 0 {
                let at = rng.below(p.len());
                p[at] = rng.next() as u8;
            } else {
                p.truncate(rng.below(p.len() + 1));
            }
            holds(&p, ends.dst);
            let noise: Vec<u8> = (0..rng.below(200)).map(|_| rng.next() as u8).collect();
            holds(&noise, rng.addr());
        }
    }

    #[test]
    fn macs_parse_from_sysfs_text() {
        assert_eq!(
            parse_mac("92:7f:aa:cc:ca:ed\n"),
            Some([0x92, 0x7f, 0xaa, 0xcc, 0xca, 0xed])
        );
        assert_eq!(parse_mac("92:7f:aa:cc:ca"), None);
        assert_eq!(parse_mac("92:7f:aa:cc:ca:ed:00"), None);
        assert_eq!(parse_mac("zz:7f:aa:cc:ca:ed"), None);
    }
}
