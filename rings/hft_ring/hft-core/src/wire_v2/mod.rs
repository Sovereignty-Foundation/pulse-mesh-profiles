// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! `hft_core::wire_v2`: the version-2 slot layout as constants, for tooling
//! (module M11).
//!
//! A v2 slot adds a data-ready atomic on a cache line of its own, so UMWAIT
//! wakes once per publish rather than twice. `core_ring::PulseRing`
//! implements it (`from_fd_v2`, `ready_line_atomic`); the constants
//! core-ring also defines are taken from it, so the two cannot drift.

#![warn(missing_docs)]

use core::mem::size_of;
use core::sync::atomic::AtomicU64;

use pulse_canon::Pulse;

/// v2 slot size in bytes: 192, against v1's 128, for the data-ready line.
pub const V2_SLOT_BYTES: usize = core_ring::PULSE_SIZE_V2;

/// Byte offset of the SeqLock counter within a v2 slot: the Pulse's
/// [`Pulse::SEQ_LOCK_OFFSET`], unchanged from v1.
pub const V2_SEQ_OFFSET: usize = Pulse::SEQ_LOCK_OFFSET;

/// Byte offset of the Pulse data area within a v2 slot: after the SeqLock
/// word, an `AtomicU64`. Unchanged from v1.
pub const V2_DATA_OFFSET: usize = V2_SEQ_OFFSET + size_of::<AtomicU64>();

/// Byte length of the Pulse data area. Unchanged from v1.
pub const V2_DATA_LEN: usize = core_ring::PULSE_SIZE - V2_DATA_OFFSET;

/// Byte offset of the data-ready atomic within a v2 slot, on a cache line of
/// its own so UMWAIT monitors it apart from the SeqLock counter.
pub const V2_DATA_READY_OFFSET: usize = core_ring::READY_LINE_OFFSET;

/// Byte length of the data-ready atomic, the `AtomicU64` that
/// `core_ring::PulseRing::ready_line_atomic` hands out.
pub const V2_DATA_READY_LEN: usize = size_of::<AtomicU64>();

/// The layout version a version-2 ring's header records:
/// `core_ring::LAYOUT_VERSION`, as for version 1. A ring is version 2 by
/// the slot size its header records, [`V2_SLOT_BYTES`]; a slot carries no
/// version of its own.
pub const V2_PROTOCOL_VERSION: u16 = core_ring::LAYOUT_VERSION as u16;

// The layout invariants hold at compile time, whatever core-ring says.
const _: () = {
    assert!(V2_DATA_OFFSET + V2_DATA_LEN == V2_DATA_READY_OFFSET);
    assert!(V2_DATA_READY_OFFSET + V2_DATA_READY_LEN <= V2_SLOT_BYTES);
    assert!(core_ring::LAYOUT_VERSION <= u16::MAX as u32);
};

#[cfg(test)]
// `assert!` in a test names the invariant that broke.
#[allow(clippy::assertions_on_constants)]
mod tests {
    use super::*;

    #[test]
    fn v2_layout_constants_consistent() {
        assert_eq!(V2_SEQ_OFFSET + 8, V2_DATA_OFFSET);
        assert_eq!(V2_DATA_OFFSET + V2_DATA_LEN, V2_DATA_READY_OFFSET);
        assert_eq!(V2_DATA_READY_OFFSET + V2_DATA_READY_LEN, 136);
        assert!(
            V2_DATA_READY_OFFSET.is_multiple_of(64),
            "data-ready must start on its own cache line"
        );
        assert!(
            V2_SLOT_BYTES > 136,
            "slot must include padding to next 64-byte boundary"
        );
    }
}
