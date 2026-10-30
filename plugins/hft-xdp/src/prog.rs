// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! The XDP program a receiver attaches: the packets it is for go to its
//! AF_XDP socket, and every other packet goes on to the kernel.
//!
//! It is written here as eBPF instructions rather than compiled from C or
//! Rust, so that building the crate needs no eBPF toolchain; it is short
//! enough to read, and the kernel's verifier checks it before it runs. A
//! packet is redirected when it is long enough for every header, IPv4 with
//! no options, UDP, not a fragment, and to the receiver's port (and its
//! address, when it listens on one): then it
//! goes to the socket the XSKMAP holds for the queue it arrived on, or, if
//! there is none, on to the kernel (`bpf_redirect_map`'s flags give the
//! action for a missing entry). The receiver checks the rest
//! ([`crate::frame::parse`]).

use std::net::Ipv4Addr;

use crate::frame::{ETHERTYPE_IPV4, ETH_HEADER, HEADERS, IPPROTO_UDP};

/// XDP's verdict to pass a packet on to the kernel.
pub const XDP_PASS: i32 = 2;
/// The helper `bpf_redirect_map`.
const HELPER_REDIRECT_MAP: i32 = 51;
/// `ld_imm64`'s source register for a map file descriptor.
const PSEUDO_MAP_FD: u8 = 1;

/// Offsets in `struct xdp_md`.
const MD_DATA: i16 = 0;
const MD_DATA_END: i16 = 4;
const MD_RX_QUEUE: i16 = 16;

// Opcodes: class | size or op | source.
const LDX_W: u8 = 0x61;
const LDX_H: u8 = 0x69;
const LDX_B: u8 = 0x71;
const MOV64_X: u8 = 0xbf;
const MOV64_K: u8 = 0xb7;
const ADD64_K: u8 = 0x07;
const AND64_K: u8 = 0x57;
const JGT_X: u8 = 0x2d;
const JNE_K: u8 = 0x55;
/// A 32-bit compare: the immediate is not sign-extended to 64 bits, so an
/// address with its top bit set compares as written.
const JMP32_JNE_K: u8 = 0x56;
const LD_DW_IMM: u8 = 0x18;
const CALL: u8 = 0x85;
const EXIT: u8 = 0x95;

/// One eBPF instruction, `struct bpf_insn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    code: u8,
    dst: u8,
    src: u8,
    off: i16,
    imm: i32,
}

impl Insn {
    const fn new(code: u8, dst: u8, src: u8, off: i16, imm: i32) -> Self {
        Self {
            code,
            dst,
            src,
            off,
            imm,
        }
    }

    /// The instruction as the kernel reads it: opcode, the registers
    /// (destination in the low nibble), offset and immediate, little-endian
    /// on the little-endian hosts AF_XDP runs on.
    pub fn to_bytes(self) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0] = self.code;
        b[1] = (self.src << 4) | (self.dst & 0x0f);
        b[2..4].copy_from_slice(&self.off.to_le_bytes());
        b[4..8].copy_from_slice(&self.imm.to_le_bytes());
        b
    }
}

/// The program for a receiver on UDP `port` and, if given, IPv4 address
/// `addr`, the XSKMAP at `map_fd`.
///
/// Registers: r1 the context, then the map; r2 the packet's start, then the
/// queue; r3 its end, then the flags; r4 and r5 scratch; r6 the context.
pub fn redirect_program(port: u16, addr: Option<Ipv4Addr>, map_fd: i32) -> Vec<Insn> {
    // A 16-bit field loads as the little-endian reading of its network
    // order bytes.
    let be16 = |v: u16| i32::from(u16::from_le_bytes(v.to_be_bytes()));
    let ip = ETH_HEADER as i16;
    let udp = ip + 20;
    let mut p = vec![
        Insn::new(MOV64_X, 6, 1, 0, 0),
        Insn::new(LDX_W, 2, 6, MD_DATA, 0),
        Insn::new(LDX_W, 3, 6, MD_DATA_END, 0),
        // Room for every header, or pass.
        Insn::new(MOV64_X, 4, 2, 0, 0),
        Insn::new(ADD64_K, 4, 0, 0, HEADERS as i32),
        Insn::new(JGT_X, 4, 3, PASS, 0),
        // IPv4,
        Insn::new(LDX_H, 5, 2, 12, 0),
        Insn::new(JNE_K, 5, 0, PASS, be16(ETHERTYPE_IPV4)),
        // with no options,
        Insn::new(LDX_B, 5, 2, ip, 0),
        Insn::new(JNE_K, 5, 0, PASS, 0x45),
        // UDP,
        Insn::new(LDX_B, 5, 2, ip + 9, 0),
        Insn::new(JNE_K, 5, 0, PASS, i32::from(IPPROTO_UDP)),
        // not a fragment (no offset, no more-fragments bit),
        Insn::new(LDX_H, 5, 2, ip + 6, 0),
        Insn::new(AND64_K, 5, 0, 0, be16(0x3fff)),
        Insn::new(JNE_K, 5, 0, PASS, 0),
        // to our port,
        Insn::new(LDX_H, 5, 2, udp + 2, 0),
        Insn::new(JNE_K, 5, 0, PASS, be16(port)),
    ];
    // and our address, when we listen on one: the word as it lies in the
    // packet, read little-endian.
    if let Some(addr) = addr {
        p.push(Insn::new(LDX_W, 5, 2, ip + 16, 0));
        p.push(Insn::new(
            JMP32_JNE_K,
            5,
            0,
            PASS,
            i32::from_le_bytes(addr.octets()),
        ));
    }
    p.extend([
        // bpf_redirect_map(map, rx_queue_index, XDP_PASS)
        Insn::new(LDX_W, 2, 6, MD_RX_QUEUE, 0),
        Insn::new(LD_DW_IMM, 1, PSEUDO_MAP_FD, 0, map_fd),
        Insn::new(0, 0, 0, 0, 0),
        Insn::new(MOV64_K, 3, 0, 0, XDP_PASS),
        Insn::new(CALL, 0, 0, 0, HELPER_REDIRECT_MAP),
        Insn::new(EXIT, 0, 0, 0, 0),
    ]);
    let pass = p.len() as i16;
    // Each jump's offset counts from the instruction after it.
    for (i, insn) in p.iter_mut().enumerate() {
        if insn.off == PASS && matches!(insn.code, JGT_X | JNE_K | JMP32_JNE_K) {
            insn.off = pass - (i as i16) - 1;
        }
    }
    p.push(Insn::new(MOV64_K, 0, 0, 0, XDP_PASS));
    p.push(Insn::new(EXIT, 0, 0, 0, 0));
    p
}

/// A placeholder offset, patched to the pass block's.
const PASS: i16 = i16::MIN;

/// The program's instructions as one byte string, for `BPF_PROG_LOAD`.
pub fn to_bytes(program: &[Insn]) -> Vec<u8> {
    program.iter().flat_map(|i| i.to_bytes()).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn every_jump_lands_on_the_pass_block() {
        for (addr, n) in [(None, 6), (Some(Ipv4Addr::new(10, 1, 2, 200)), 7)] {
            let p = redirect_program(9000, addr, 3);
            let pass = p.len() - 2;
            assert_eq!(p[pass], Insn::new(MOV64_K, 0, 0, 0, XDP_PASS));
            let jumps: Vec<usize> = (0..p.len())
                .filter(|&i| matches!(p[i].code, JGT_X | JNE_K | JMP32_JNE_K))
                .collect();
            assert_eq!(jumps.len(), n);
            for i in jumps {
                assert_eq!(
                    i as i64 + 1 + i64::from(p[i].off),
                    pass as i64,
                    "jump at {i}"
                );
            }
        }
    }

    #[test]
    fn the_port_and_the_map_are_where_the_kernel_reads_them() {
        let p = redirect_program(0x1234, Some(Ipv4Addr::new(10, 1, 2, 200)), 42);
        let port = p.iter().rev().find(|i| i.code == JNE_K).unwrap();
        // 0x1234 in network order is the bytes 12 34, read as 0x3412.
        assert_eq!(port.imm, 0x3412);
        // 10.1.2.200 is the bytes 0a 01 02 c8, read as 0xc802010a, which
        // compares in 32 bits as written.
        let addr = p.iter().find(|i| i.code == JMP32_JNE_K).unwrap();
        assert_eq!(addr.imm as u32, 0xc802_010a);
        let ld = p.iter().position(|i| i.code == LD_DW_IMM).unwrap();
        assert_eq!(p[ld].to_bytes(), [0x18, 0x11, 0, 0, 42, 0, 0, 0]);
        assert_eq!(p[ld + 1].to_bytes(), [0; 8]);
        assert_eq!(to_bytes(&p).len(), 8 * p.len());
    }
}
