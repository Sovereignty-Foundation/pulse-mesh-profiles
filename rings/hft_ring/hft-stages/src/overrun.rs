// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! The overrun rule the SPMC ring and the MultiLane lanes share.
//!
//! Neither writer waits for its readers, so a writer more than a capacity
//! ahead of a reader has overwritten slots the reader has not read. Both
//! readers apply one rule around their SeqLock read:
//!
//! 1. Before the read, a cursor more than a capacity behind the writer's
//!    moves to the oldest position the ring still holds ([`resume_at`]), and
//!    the positions it passes are counted lost.
//! 2. After the copy, a copy that may hold the slot's next lap ([`lapped`])
//!    is discarded and counted lost, and the cursor moves past it.
//!
//! A position is therefore delivered once, as itself, or counted lost once.
//!
//! Both writers fill a slot under its SeqLock before they advance their
//! cursor, so a writer cursor more than a capacity past the reader's proves
//! the slot's next lap is written. One exactly a capacity past does not (the
//! writer may not have started the lap, or not yet advanced), and the slot's
//! counter settles it: it starts at zero and every completed write adds two,
//! so a slot holding lap `k` has taken at least `k + 1` writes. A counter
//! ahead for another reason (a `publish_at_slot` into a lane, or a lane
//! writer that died between its slot write and its cursor advance) makes
//! the rule discard a good copy, and only for a reader exactly a capacity
//! behind: the rule errs toward loss, never toward a newer lap under an
//! older position.

/// Where a reader at `cursor` reads next, given the writer's cursor `head`:
/// `cursor` itself while the ring still holds it, otherwise the oldest
/// position the ring holds, `head - capacity`. The positions in between are
/// lost.
pub(crate) fn resume_at(cursor: u64, head: u64, capacity: u64) -> u64 {
    cursor.max(head.saturating_sub(capacity))
}

/// Whether a stable SeqLock copy of the slot for `cursor` may hold a newer
/// lap than `cursor`'s.
///
/// `head` is the writer's cursor loaded after the copy. `seq` yields the
/// slot's counter as the copy saw it, or as loaded after the copy (an odd
/// value counts a write in progress, which `seq / 2` leaves out); it is
/// called only when the reader is exactly a capacity behind.
pub(crate) fn lapped(cursor: u64, head: u64, capacity: u64, seq: impl FnOnce() -> u64) -> bool {
    let ahead = head.saturating_sub(cursor);
    // The slot has taken `seq / 2` completed writes; more than `cursor`'s lap
    // plus one means a newer lap. `ahead == capacity >= 1` puts `cursor`
    // below `u64::MAX`, so the sum cannot overflow.
    ahead > capacity || (ahead == capacity && seq() / 2 > cursor / capacity + 1)
}

#[cfg(test)]
// Test idiom: `panic!` in a closure marks a counter the rule must not read.
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn resume_at_keeps_a_cursor_the_ring_still_holds() {
        assert_eq!(resume_at(0, 3, 8), 0);
        assert_eq!(resume_at(5, 9, 8), 5);
        // Exactly a capacity behind: the oldest slot still holds the cursor.
        assert_eq!(resume_at(0, 8, 8), 0);
    }

    #[test]
    fn resume_at_moves_a_lapped_cursor_to_the_oldest_position_held() {
        assert_eq!(resume_at(0, 20, 8), 12);
        assert_eq!(resume_at(3, 12, 8), 4);
        assert_eq!(resume_at(0, 2, 1), 1);
    }

    #[test]
    fn a_reader_less_than_a_capacity_behind_keeps_its_copy() {
        assert!(!lapped(4, 11, 8, || panic!(
            "counter read below the boundary"
        )));
        assert!(!lapped(4, 5, 8, || panic!(
            "counter read below the boundary"
        )));
    }

    #[test]
    fn a_writer_more_than_a_capacity_ahead_voids_the_copy() {
        assert!(lapped(0, 9, 8, || panic!("counter read past the boundary")));
        assert!(lapped(12, 30, 8, || panic!(
            "counter read past the boundary"
        )));
    }

    #[test]
    fn at_exactly_a_capacity_the_slot_counter_decides() {
        // Position 12 is lap 1 of a capacity-8 ring: its slot took writes 4
        // and 12, so its counter is 4.
        assert!(!lapped(12, 20, 8, || 4));
        // Position 20 mid-write: the copy predates it.
        assert!(!lapped(12, 20, 8, || 5));
        // Position 20 written, the writer not yet advanced to 21.
        assert!(lapped(12, 20, 8, || 6));
        // Lap 0 of a full ring, first write into each slot.
        assert!(!lapped(0, 8, 8, || 2));
        assert!(lapped(0, 8, 8, || 4));
    }
}
