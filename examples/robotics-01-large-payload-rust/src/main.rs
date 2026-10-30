// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! Example — the robotics ring's large-payload pool, reached through its hub
//! (Rust).
//!
//! The robotics ring is imported through `robotics_hub` alone, which
//! re-exports the ring's layers; nothing here names `robotics_stages`. A
//! payload too large for a 128-byte Pulse goes into a segment of a
//! shared-memory pool, and the Pulse carries a 16-byte `SegmentRef` (segment,
//! length, CRC-32C) in its data area. This program writes a 4 MiB scan in one
//! process and reads it in another: it creates the pool, writes the scan,
//! packs the reference into a Pulse buffer, and runs itself again as the
//! reader, which maps the pool by name, reads and checks the scan, and frees
//! the segment; the writer then sees the segment free for the next scan.
#![forbid(unsafe_code)]
// A demonstration: the expected flow is "everything works, prints, exits 0",
// so fail-fast idioms are deliberate here.
#![allow(clippy::print_stdout, clippy::expect_used)]

use robotics_hub::stages::large_payload::{
    pack_segment_ref, unpack_segment_ref, LargePayloadError, LargePayloadPool, PoolConfig,
    SegmentRef,
};

/// A synthetic 4 MiB LIDAR scan: 262,144 points of four 32-bit floats.
fn scan(seed: u32) -> Vec<u8> {
    (0..(4u32 << 20) / 4)
        .flat_map(|i| (i.wrapping_mul(2_654_435_761) ^ seed).to_le_bytes())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> [u8; 128] {
    let mut out = [0u8; 128];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).expect("hex");
    }
    out
}

/// The reading process: map the pool `name`, take the reference out of the
/// Pulse buffer, read and check the scan, free its segment.
fn reader(name: &str, pulse_hex: &str) {
    let pool = LargePayloadPool::open(name).expect("map the writer's pool by name");
    let seg = unpack_segment_ref(&from_hex(pulse_hex));
    let mut payload = Vec::new();
    pool.read(&seg, &mut payload)
        .expect("read the scan; its CRC-32C checks");
    assert!(payload == scan(7), "the scan changed between processes");
    println!(
        "reader (pid {}): read {} bytes from segment {:#x}, CRC-32C {:#010x} checks",
        std::process::id(),
        payload.len(),
        seg.id,
        seg.crc32
    );
    pool.free(seg.id).expect("free the segment");
    println!("reader: freed the segment");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let [_, role, name, pulse] = args.as_slice() {
        if role == "read" {
            reader(name, pulse);
            return;
        }
    }

    // The hub's manifest names the stage this example reaches.
    println!("robotics ring manifest: {:?}", robotics_hub::RING_MANIFEST);

    // `PoolConfig` is `non_exhaustive`: it is built from its `Default` and
    // set field by field. One 4 MiB segment, so the next scan reuses it.
    let mut config = PoolConfig::default();
    config.name = format!("/pms_lp_example_{}", std::process::id());
    config.segment_count = 1;
    config.segment_bytes = 4 << 20;
    let pool = LargePayloadPool::create(config).expect("create the pool");
    let name = pool.config().name.clone();

    // The writer: the scan goes into a segment, and its reference into the
    // data area of a Pulse, which is all a ring would carry.
    let seg: SegmentRef = pool.write(&scan(7)).expect("write the scan");
    let mut pulse_buf = [0u8; 128];
    pack_segment_ref(&mut pulse_buf, &seg);
    println!(
        "writer (pid {}): wrote {} bytes to segment {:#x}; the Pulse carries {} bytes of reference",
        std::process::id(),
        seg.len,
        seg.id,
        SegmentRef::SIZE
    );

    // The reader: this program again, in its own process.
    let status = std::process::Command::new(std::env::current_exe().expect("this program"))
        .args(["read", &name, &hex(&pulse_buf)])
        .status()
        .expect("run the reader");
    assert!(status.success(), "the reader failed: {status}");

    // The reader's free reached this process: the segment is free again,
    // and the reference to the old scan is refused.
    println!("writer: {} segments in use after the reader", pool.in_use());
    let mut payload = Vec::new();
    match pool.read(&seg, &mut payload) {
        Err(LargePayloadError::InvalidSegment(id)) => {
            println!("writer: the freed reference {id:#x} is refused");
        }
        other => println!("unexpected: {other:?}"),
    }
    let next = pool.write(&scan(8)).expect("write the next scan");
    println!(
        "writer: the next scan reused segment {} at generation {} (id {:#x})",
        next.id & 0xFFFF_FFFF,
        next.id >> 32,
        next.id
    );
    pool.free(next.id).expect("free the segment");
}
