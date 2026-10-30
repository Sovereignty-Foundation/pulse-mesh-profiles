// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! A memfd from `allocate_hugepage_memfd` carries a ring made with
//! `core_ring::PulseRing::from_fd`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

// memfds are Linux-only, and so are these tests and their imports.
#[cfg(target_os = "linux")]
use core_ring::PulseRing;
#[cfg(target_os = "linux")]
use hft_stages::numa::{allocate_hugepage_memfd, PageSize};
#[cfg(target_os = "linux")]
use pulse_canon::Pulse;

/// A pulse published into a ring over a regular-page memfd reads back.
#[cfg(target_os = "linux")]
#[test]
fn memfd_regular_pages_round_trip() {
    let capacity: u64 = 1024;
    let total = core_ring::HEADER_SIZE + (capacity as usize) * core_ring::PULSE_SIZE;
    let fd = allocate_hugepage_memfd("pms-shm-numa-test-regular", total, PageSize::Regular)
        .expect("memfd allocation");
    let mut ring = PulseRing::from_fd("pms-shm-numa-test-regular", fd, capacity, true, true)
        .expect("from_fd mount");

    let mut bytes = [0u8; 128];
    // From offset 8: bytes 0..8 of a Pulse are the SeqLock counter, which
    // the ring overwrites.
    bytes[8..16].copy_from_slice(&0xCAFE_BABE_DEAD_BEEFu64.to_le_bytes());
    let pulse = Pulse::from_bytes(&bytes).expect("from_bytes");
    let slot = ring.publish(&pulse);
    assert!(slot >= 1);

    let recovered = ring.read_slot(0).expect("read slot 0 after publish");
    assert_eq!(
        &recovered.as_bytes()[8..16],
        &0xCAFE_BABE_DEAD_BEEFu64.to_le_bytes()
    );
}

/// The same on 2 MB hugepages; skipped where the pool is empty
/// (`/sys/kernel/mm/hugepages/hugepages-2048kB/nr_hugepages`).
#[cfg(target_os = "linux")]
#[test]
fn memfd_hugepage_round_trip_or_skip() {
    let capacity: u64 = 8192;
    let total = core_ring::HEADER_SIZE + (capacity as usize) * core_ring::PULSE_SIZE;
    let fd = match allocate_hugepage_memfd("pms-shm-numa-test-huge", total, PageSize::Huge2M) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!(
                "SKIP: hugepage memfd unavailable on this host (likely empty pool): {}",
                e
            );
            return;
        }
    };
    // Mapping can still fail, with ENOMEM, where the pool is empty.
    let mut ring = match PulseRing::from_fd("pms-shm-numa-test-huge", fd, capacity, true, true) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("SKIP: from_fd mmap failed (empty hugepage pool or no permission): {e}");
            return;
        }
    };

    let mut bytes = [0u8; 128];
    // Pattern at offset 8+ — same reason as memfd_regular_pages_round_trip.
    bytes[8..16].copy_from_slice(&0xFEED_FACE_C0DE_F00Du64.to_le_bytes());
    let pulse = Pulse::from_bytes(&bytes).expect("from_bytes");
    let slot = ring.publish(&pulse);
    assert!(slot >= 1);

    let recovered = ring.read_slot(0).expect("read slot 0 after publish");
    assert_eq!(
        &recovered.as_bytes()[8..16],
        &0xFEED_FACE_C0DE_F00Du64.to_le_bytes()
    );
}
