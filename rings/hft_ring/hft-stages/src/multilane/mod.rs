// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! `hft_stages::multilane`: N [`PulseRing`] lanes, one writer each, read by
//! one reader through a round-robin multiplexer.
//!
//! Writers that share a ring contend for its cache lines; lanes on disjoint
//! cache lines, and through [`MultiLaneRing::create_with_numa`] on chosen
//! NUMA nodes, keep them apart.
//!
//! ## Rules
//!
//! 1. **One writer per lane.** [`MultiLaneRing::create`] returns one
//!    [`LaneWriter`] per lane, which publishes through `&mut self`; the
//!    multiplexer reads the lanes through handles of its own.
//! 2. **One reader.** Two threads polling one `MultiLaneRing` each take
//!    pulses the other then does not see; poll from one thread.
//! 3. **No hidden threads, no auto-buffering, no hot-path allocation.**
//!    The multiplexer is a tight-poll loop driven by the caller's thread.
//!
//! A reader that a lane's writer laps loses pulses, counted per lane in
//! [`MultiLaneRing::lost`], and never receives one under another's position
//! ([`MultiLaneRing::poll_round_robin`]). A lane whose writer has left is
//! reported, once drained, through [`MultiLaneRing::lane_superseded`], as
//! `PulseHub::poll` reports a superseded ring.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::numa::NumaError;
use crate::overrun;
use core_ring::{PulseRing, RingError};
use pulse_canon::Pulse;

/// Errors raised when allocating or operating a [`MultiLaneRing`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MultiLaneError {
    /// `lane_count` was zero.
    #[error("invalid lane_count {got}: must be >= 1")]
    InvalidLaneCount {
        /// The bad value the caller passed.
        got: usize,
    },
    /// A lane's [`PulseRing`] failed to allocate.
    #[error("lane {lane_index} ring allocation failed: {source}")]
    LaneAlloc {
        /// 0-indexed lane that failed.
        lane_index: usize,
        /// The underlying ring error.
        #[source]
        source: RingError,
    },
    /// `last_cursors` slice length did not match `lane_count` in a
    /// call to [`MultiLaneRing::poll_round_robin`].
    #[error("last_cursors len {got} != lane_count {expected}")]
    CursorSliceMismatch {
        /// What the caller passed.
        got: usize,
        /// What was expected.
        expected: usize,
    },
    /// Binding a lane to its NUMA node failed (Linux only: elsewhere
    /// [`MultiLaneRing::create_with_numa`] binds nothing).
    #[error("lane {lane_index} NUMA bind to node {node} failed: {source}")]
    NumaBind {
        /// 0-indexed lane that failed the mbind call.
        lane_index: usize,
        /// The NUMA node the caller requested.
        node: u32,
        /// The underlying error from the numa submodule.
        #[source]
        source: NumaError,
    },
}

/// The one writer of a lane, returned by [`MultiLaneRing::create`] and
/// [`MultiLaneRing::create_with_numa`]. It holds the handle that made or
/// attached the lane and publishes through `&mut self`, so a lane has one
/// writer by construction; dropping the handle that made the lane marks it
/// superseded, which the reader reports once drained
/// ([`MultiLaneRing::lane_superseded`]).
pub struct LaneWriter {
    index: usize,
    ring: PulseRing,
}

impl std::fmt::Debug for LaneWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaneWriter")
            .field("index", &self.index)
            .field("ring", &self.ring.name())
            .finish()
    }
}

impl LaneWriter {
    /// The lane this writer publishes on.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    /// Publish `pulse` on the lane; its write number ([`PulseRing::publish`]).
    pub fn publish(&mut self, pulse: &Pulse) -> u64 {
        self.ring.publish(pulse)
    }

    /// The lane's writing handle, for the rest of [`PulseRing`]'s writes.
    pub fn ring_mut(&mut self) -> &mut PulseRing {
        &mut self.ring
    }

    /// The lane's writing handle, to inspect.
    #[must_use]
    pub fn ring(&self) -> &PulseRing {
        &self.ring
    }
}

/// N [`PulseRing`] lanes, one writer each, read by one reader in
/// round-robin order.
///
/// Each lane is a version-1 ring (128-byte slots, no ready line) made by
/// [`PulseRing::create_or_open`]. Its writer is the [`LaneWriter`] that
/// [`Self::create`] returned for it; the reader calls
/// [`Self::poll_round_robin`] on handles of its own.
pub struct MultiLaneRing {
    lanes: Vec<PulseRing>,
    /// Pulses lost to overrun per lane, indexed like `lanes`.
    lost: Box<[AtomicU64]>,
    /// Per lane, whether a poll found it superseded and drained.
    superseded: Box<[AtomicBool]>,
    /// The lane the next poll looks at first: one past the lane the last
    /// pulse came from.
    next_lane: AtomicUsize,
}

impl std::fmt::Debug for MultiLaneRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiLaneRing")
            .field("lane_count", &self.lanes.len())
            .finish()
    }
}

impl MultiLaneRing {
    fn from_lanes(lanes: Vec<PulseRing>) -> Self {
        let lost = lanes.iter().map(|_| AtomicU64::new(0)).collect();
        let superseded = lanes.iter().map(|_| AtomicBool::new(false)).collect();
        Self {
            lanes,
            lost,
            superseded,
            next_lane: AtomicUsize::new(0),
        }
    }

    /// Create, or attach to, `lane_count` version-1 lanes of `capacity`
    /// slots, named `<name_prefix>-l<index>`: the reader, and one
    /// [`LaneWriter`] per lane, in lane order.
    ///
    /// On macOS a name must fit `PSEMNAMLEN` (30 bytes with the leading
    /// `/`), so keep the prefix to 24.
    ///
    /// # Errors
    /// [`MultiLaneError::InvalidLaneCount`] if `lane_count == 0`;
    /// [`MultiLaneError::LaneAlloc`] if any lane's `PulseRing` allocation
    /// fails.
    pub fn create(
        name_prefix: &str,
        capacity: u64,
        lane_count: usize,
    ) -> Result<(Self, Vec<LaneWriter>), MultiLaneError> {
        if lane_count == 0 {
            return Err(MultiLaneError::InvalidLaneCount { got: lane_count });
        }
        let mut writers = Vec::with_capacity(lane_count);
        for lane_index in 0..lane_count {
            writers.push(Self::make_lane(name_prefix, capacity, lane_index)?);
        }
        Self::attach(name_prefix, capacity, writers)
    }

    /// Make, or attach to, lane `lane_index`, and return its writer.
    fn make_lane(
        name_prefix: &str,
        capacity: u64,
        lane_index: usize,
    ) -> Result<LaneWriter, MultiLaneError> {
        let ring = PulseRing::create_or_open(&Self::lane_name(name_prefix, lane_index), capacity)
            .map_err(|source| MultiLaneError::LaneAlloc { lane_index, source })?;
        Ok(LaneWriter {
            index: lane_index,
            ring,
        })
    }

    /// The reader over the lanes `writers` write, through handles of its
    /// own, and the writers.
    fn attach(
        name_prefix: &str,
        capacity: u64,
        writers: Vec<LaneWriter>,
    ) -> Result<(Self, Vec<LaneWriter>), MultiLaneError> {
        let mut lanes = Vec::with_capacity(writers.len());
        for writer in &writers {
            let lane_index = writer.index;
            let ring = PulseRing::open(&Self::lane_name(name_prefix, lane_index), capacity)
                .map_err(|source| MultiLaneError::LaneAlloc { lane_index, source })?;
            lanes.push(ring);
        }
        Ok((Self::from_lanes(lanes), writers))
    }

    fn lane_name(name_prefix: &str, lane_index: usize) -> String {
        format!("{name_prefix}-l{lane_index}")
    }

    /// [`Self::create`] with `lane_to_node.len()` lanes, lane `i` bound to
    /// NUMA node `lane_to_node[i]` with `mbind(2)`.
    ///
    /// Compiled on Linux, not run: binding lanes needs NUMA nodes 0 and 1.
    ///
    /// ```no_run
    /// use hft_stages::multilane::MultiLaneRing;
    ///
    /// // 8-lane ring; lanes 0-3 on node 0, 4-7 on node 1.
    /// let lane_to_node: Vec<u32> = (0..8).map(|i| (i / 4) as u32).collect();
    /// let (ring, writers) = MultiLaneRing::create_with_numa("/pmml", 4096, &lane_to_node)?;
    /// # let _ = (ring, writers);
    /// # Ok::<(), hft_stages::multilane::MultiLaneError>(())
    /// ```
    ///
    /// # Errors
    /// [`MultiLaneError::InvalidLaneCount`] if `lane_to_node` is empty;
    /// [`MultiLaneError::LaneAlloc`] on allocation failure;
    /// [`MultiLaneError::NumaBind`] (Linux) if `mbind(2)` rejects the node.
    #[cfg(target_os = "linux")]
    pub fn create_with_numa(
        name_prefix: &str,
        capacity: u64,
        lane_to_node: &[u32],
    ) -> Result<(Self, Vec<LaneWriter>), MultiLaneError> {
        let lane_count = lane_to_node.len();
        if lane_count == 0 {
            return Err(MultiLaneError::InvalidLaneCount { got: 0 });
        }

        let mut writers = Vec::with_capacity(lane_count);
        for lane_index in 0..lane_count {
            writers.push(Self::make_lane(name_prefix, capacity, lane_index)?);
        }

        for (lane_index, (&node, writer)) in lane_to_node.iter().zip(writers.iter()).enumerate() {
            let addr = writer.ring.mmap_addr();
            let len = writer.ring.mmap_total_size();
            crate::numa::numa_bind_memory(addr, len, node).map_err(|source| {
                MultiLaneError::NumaBind {
                    lane_index,
                    node,
                    source,
                }
            })?;
            tracing::debug!(
                lane = lane_index,
                node,
                bytes = len,
                "hft_stages::multilane: bound lane to NUMA node"
            );
        }

        Self::attach(name_prefix, capacity, writers)
    }

    /// Off Linux, binds nothing: [`Self::create`] with `lane_to_node.len()`
    /// lanes, and a debug log.
    ///
    /// # Errors
    ///
    /// [`MultiLaneError::InvalidLaneCount`] if `lane_to_node` is empty;
    /// [`MultiLaneError::LaneAlloc`] if a lane's allocation fails.
    #[cfg(not(target_os = "linux"))]
    pub fn create_with_numa(
        name_prefix: &str,
        capacity: u64,
        lane_to_node: &[u32],
    ) -> Result<(Self, Vec<LaneWriter>), MultiLaneError> {
        let lane_count = lane_to_node.len();
        tracing::debug!(
            lane_count,
            "hft_stages::multilane: create_with_numa — NUMA not available on this OS, \
             falling back to create()"
        );
        Self::create(name_prefix, capacity, lane_count)
    }

    /// Number of lanes.
    pub fn lane_count(&self) -> usize {
        self.lanes.len()
    }

    /// Lane `index` as the reader holds it, to inspect; its writer is the
    /// [`LaneWriter`] [`Self::create`] returned for it.
    ///
    /// # Panics
    /// Panics if `index >= self.lane_count()`.
    pub fn lane(&self, index: usize) -> &PulseRing {
        &self.lanes[index]
    }

    /// Pulses on lane `index` that its writer overwrote before
    /// [`Self::poll_round_robin`] read them, as this handle's polls counted.
    ///
    /// # Panics
    /// Panics if `index >= self.lane_count()`, as [`Self::lane`] does.
    pub fn lost(&self, index: usize) -> u64 {
        self.lost[index].load(Ordering::Relaxed)
    }

    /// Whether [`Self::poll_round_robin`] has found lane `index`
    /// superseded: its writer has left it (the creating handle dropped it,
    /// or something marked it with [`PulseRing::supersede`]) and every pulse
    /// it held has been returned or counted in [`Self::lost`]. It stays
    /// `true` for this handle; a new handle from [`Self::create`] attaches
    /// to whatever ring the lane's name holds now. A writer killed outright
    /// marks nothing, so its readers are told only if something marks the
    /// lanes for it.
    ///
    /// # Panics
    /// Panics if `index >= self.lane_count()`, as [`Self::lane`] does.
    #[must_use]
    pub fn lane_superseded(&self, index: usize) -> bool {
        self.superseded[index].load(Ordering::Relaxed)
    }

    /// The next unread pulse of the first lane that has one, with the lane's
    /// index. The search starts one past the lane the last pulse came from
    /// and wraps, so the lanes take turns and a lane that always has a pulse
    /// cannot starve the others.
    ///
    /// `last_cursors[i]` is the position of the next pulse to read on lane
    /// `i`, and the poll moves it past every pulse it returns or loses.
    /// Start from zeros to read what the lanes hold, any excess over a
    /// capacity counted lost, or from each lane's
    /// [`PulseRing::cursor_position`] to start from the present.
    ///
    /// A lane cursor the writer has lapped moves to the oldest pulse the lane
    /// still holds, and a copy the writer may have overwritten during the
    /// read is discarded; both count in [`Self::lost`], and no pulse is
    /// returned under another's position, or twice. A lane whose writer has
    /// left is read to its end, then reported once through
    /// [`Self::lane_superseded`], with a warning.
    ///
    /// `None` when no lane yields a pulse in this pass: every lane is caught
    /// up, or a read was torn or discarded and is retried on the next call.
    ///
    /// # Errors
    ///
    /// Returns [`MultiLaneError::CursorSliceMismatch`] if
    /// `last_cursors.len() != self.lane_count()`.
    pub fn poll_round_robin(
        &self,
        last_cursors: &mut [u64],
    ) -> Result<Option<(usize, Pulse)>, MultiLaneError> {
        self.poll_round_robin_with(last_cursors, |_, _| {})
    }

    /// [`Self::poll_round_robin`], running `before_read(lane, cursor)`
    /// between a lane's overrun check and its read: the tests' seam for a
    /// writer that moves in between.
    #[inline(always)]
    fn poll_round_robin_with(
        &self,
        last_cursors: &mut [u64],
        mut before_read: impl FnMut(usize, u64),
    ) -> Result<Option<(usize, Pulse)>, MultiLaneError> {
        if last_cursors.len() != self.lanes.len() {
            return Err(MultiLaneError::CursorSliceMismatch {
                got: last_cursors.len(),
                expected: self.lanes.len(),
            });
        }
        let lanes = self.lanes.len();
        let first = self.next_lane.load(Ordering::Relaxed) % lanes;
        for lane_index in (first..lanes).chain(0..first) {
            let (lane, lost) = (&self.lanes[lane_index], &self.lost[lane_index]);
            let capacity = lane.capacity();
            let writer_cursor = lane.cursor_position();
            let mut cursor = last_cursors[lane_index];
            if writer_cursor <= cursor {
                self.note_if_superseded(lane_index, lane, cursor);
                continue;
            }
            // Overrun before the read: nothing below `writer_cursor -
            // capacity` is held.
            let oldest = overrun::resume_at(cursor, writer_cursor, capacity);
            if oldest > cursor {
                lost.fetch_add(oldest - cursor, Ordering::Relaxed);
                cursor = oldest;
                last_cursors[lane_index] = cursor;
                lane.advance_read_cursor(cursor);
            }
            before_read(lane_index, cursor);
            let Some(pulse) = lane.read_slot(cursor) else {
                // Torn past the retry budget: retried on the next call.
                continue;
            };
            // Overrun during the read: the writer may have refilled the slot
            // since the check. `read_slot` acquired the counter of the write it
            // copied, which began only after the writer advanced past every
            // earlier position, so the cursor loaded now is at least that
            // write's, and the counter loaded after the copy at least the
            // copy's.
            let writer_cursor = lane.cursor_position();
            if overrun::lapped(cursor, writer_cursor, capacity, || {
                lane.slot_seq_atomic(cursor).load(Ordering::Acquire)
            }) {
                let next = overrun::resume_at(cursor + 1, writer_cursor, capacity);
                lost.fetch_add(next - cursor, Ordering::Relaxed);
                last_cursors[lane_index] = next;
                lane.advance_read_cursor(next);
                continue;
            }
            last_cursors[lane_index] = cursor + 1;
            // Lets a `publish_blocking` writer see the slot free.
            lane.advance_read_cursor(cursor + 1);
            self.next_lane
                .store((lane_index + 1) % lanes, Ordering::Relaxed);
            return Ok(Some((lane_index, pulse)));
        }
        Ok(None)
    }

    /// Record lane `lane_index` as superseded, once, if its writer has left
    /// and the reader at `cursor` has read all it holds. The mark is loaded
    /// before the write cursor is loaded again: a writer is marked only
    /// after its last publish, so the cursor loaded after the mark is final.
    fn note_if_superseded(&self, lane_index: usize, lane: &PulseRing, cursor: u64) {
        if !lane.is_superseded() || lane.cursor_position() > cursor {
            return;
        }
        if !self.superseded[lane_index].swap(true, Ordering::Relaxed) {
            tracing::warn!(
                lane = lane_index,
                ring = %lane.name(),
                "a lane's writer has left it; attach anew to follow the lane now under its name"
            );
        }
    }

    /// [`Self::poll_round_robin`] into `out` until it is full or a poll
    /// returns `None`; returns the number of pulses written.
    ///
    /// # Errors
    /// [`MultiLaneError::CursorSliceMismatch`] if
    /// `last_cursors.len() != self.lane_count()`.
    pub fn drain_into(
        &self,
        last_cursors: &mut [u64],
        out: &mut [(usize, Pulse)],
    ) -> Result<usize, MultiLaneError> {
        let mut written = 0;
        while written < out.len() {
            match self.poll_round_robin(last_cursors)? {
                Some(pair) => {
                    out[written] = pair;
                    written += 1;
                }
                None => break,
            }
        }
        Ok(written)
    }
}

/// Pin the calling writer thread to `numa_node`, the node its lane is bound
/// to, before its first publish; `lane_index` is not used. A no-op off
/// Linux.
///
/// # Errors
/// On Linux, the [`NumaError`] of [`crate::numa::pin_thread_to_numa_node`];
/// never off Linux.
pub fn pin_lane_writer(lane_index: usize, numa_node: u32) -> Result<(), NumaError> {
    let _ = lane_index;
    #[cfg(target_os = "linux")]
    {
        crate::numa::pin_thread_to_numa_node(numa_node, None)?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = numa_node;
    }
    Ok(())
}

#[cfg(test)]
// Tests fail fast: a setup error is a failed test.
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::useless_vec,
    clippy::print_stderr
)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_NAME_COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// A name prefix unique to this process and test, at most 24 bytes, so
    /// the lane suffix keeps names within macOS's 30. It carries the whole
    /// process id, since other checkouts run these tests at once, and one
    /// too long fails rather than be cut to a name another test may hold.
    fn unique_prefix(tag: &str) -> String {
        let id = TEST_NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
        let prefix = format!("/pmml-{tag}-{}-{id}", std::process::id());
        assert!(prefix.len() <= 24, "shm prefix {prefix} is over 24 bytes");
        prefix
    }

    #[test]
    fn single_lane_round_trip() {
        let prefix = unique_prefix("sl");
        let (ring, mut writers) = MultiLaneRing::create(&prefix, 64, 1).expect("create");
        assert_eq!(ring.lane_count(), 1);

        let pulse = Pulse::zeroed();
        let _ = writers[0].publish(&pulse);

        let mut cursors = vec![0u64; 1];
        let result = ring.poll_round_robin(&mut cursors).expect("poll");
        match result {
            Some((idx, _p)) => {
                assert_eq!(idx, 0);
                assert_eq!(cursors[0], 1);
            }
            None => panic!("expected pulse on lane 0"),
        }

        assert!(ring
            .poll_round_robin(&mut cursors)
            .expect("poll2")
            .is_none());
    }

    /// Two lanes that both hold pulses are read in turn, not the first
    /// drained before the second is looked at.
    #[test]
    fn polls_take_the_lanes_in_turn() {
        let prefix = unique_prefix("rr");
        let (ring, mut writers) = MultiLaneRing::create(&prefix, 64, 3).expect("create");
        let pulse = Pulse::zeroed();
        for writer in &mut writers[..2] {
            for _ in 0..3 {
                let _ = writer.publish(&pulse);
            }
        }
        let mut cursors = vec![0u64; 3];
        let order: Vec<usize> = std::iter::from_fn(|| {
            ring.poll_round_robin(&mut cursors)
                .expect("poll")
                .map(|(lane, _)| lane)
        })
        .collect();
        assert_eq!(order, [0, 1, 0, 1, 0, 1]);
    }

    #[test]
    fn four_lanes_each_writer_independent() {
        const LANE_COUNT: usize = 4;
        const PULSES_PER_LANE: u64 = 1000;

        let prefix = unique_prefix("4w");
        let (ring, writers) = MultiLaneRing::create(&prefix, 4096, LANE_COUNT).expect("create");

        let mut threads = Vec::new();
        for mut writer in writers {
            threads.push(std::thread::spawn(move || {
                let pulse = Pulse::zeroed();
                for _ in 0..PULSES_PER_LANE {
                    let _ = writer.publish(&pulse);
                }
            }));
        }

        let mut cursors = vec![0u64; LANE_COUNT];
        let mut per_lane_count = [0u64; LANE_COUNT];
        let target_total = LANE_COUNT as u64 * PULSES_PER_LANE;
        let mut total = 0u64;
        let start = std::time::Instant::now();
        while total < target_total {
            if let Some((lane_idx, _pulse)) = ring.poll_round_robin(&mut cursors).expect("poll") {
                per_lane_count[lane_idx] += 1;
                total += 1;
            }
            if start.elapsed() > std::time::Duration::from_secs(10) {
                panic!(
                    "timeout after {} pulses (expected {}); per-lane: {:?}",
                    total, target_total, per_lane_count
                );
            }
        }

        for thread in threads {
            thread.join().expect("writer panicked");
        }

        for (i, count) in per_lane_count.iter().enumerate() {
            assert_eq!(
                *count, PULSES_PER_LANE,
                "lane {i} received {count} pulses (expected {PULSES_PER_LANE})"
            );
        }
    }

    #[test]
    fn lane_count_boundary_two_to_sixteen() {
        for lane_count in [2usize, 4, 8, 16] {
            let prefix = unique_prefix(&format!("b{lane_count}"));
            let (ring, mut writers) =
                MultiLaneRing::create(&prefix, 64, lane_count).expect("create");
            assert_eq!(ring.lane_count(), lane_count);

            let pulse = Pulse::zeroed();
            for writer in &mut writers {
                let _ = writer.publish(&pulse);
            }

            let mut cursors = vec![0u64; lane_count];
            let mut seen = vec![false; lane_count];
            for _ in 0..lane_count {
                let (lane_idx, _) = ring
                    .poll_round_robin(&mut cursors)
                    .expect("poll")
                    .expect("expected a pulse");
                seen[lane_idx] = true;
            }
            for (i, was_seen) in seen.iter().enumerate() {
                assert!(*was_seen, "lane {i} not seen at lane_count={lane_count}");
            }
        }
    }

    #[test]
    fn multiplexer_stress() {
        const LANE_COUNT: usize = 16;
        const PULSES_PER_LANE: u64 = 10_000;

        let prefix = unique_prefix("st");
        // More slots than pulses per lane, so none is overwritten.
        let (ring, writers) = MultiLaneRing::create(&prefix, 16_384, LANE_COUNT).expect("create");

        let mut threads = Vec::new();
        for mut writer in writers {
            threads.push(std::thread::spawn(move || {
                let pulse = Pulse::zeroed();
                for _ in 0..PULSES_PER_LANE {
                    let _ = writer.publish(&pulse);
                }
            }));
        }

        let mut cursors = vec![0u64; LANE_COUNT];
        let mut per_lane_count = [0u64; LANE_COUNT];
        let target_total = LANE_COUNT as u64 * PULSES_PER_LANE;
        let mut total = 0u64;
        let start = std::time::Instant::now();
        while total < target_total {
            if let Some((lane_idx, _)) = ring.poll_round_robin(&mut cursors).expect("poll") {
                per_lane_count[lane_idx] += 1;
                total += 1;
            }
            if start.elapsed() > std::time::Duration::from_secs(30) {
                panic!("stress timeout; total={total}, per-lane={per_lane_count:?}");
            }
        }

        for thread in threads {
            thread.join().expect("writer panicked");
        }

        assert_eq!(total, target_total);
        for (i, count) in per_lane_count.iter().enumerate() {
            assert_eq!(
                *count, PULSES_PER_LANE,
                "lane {i} count mismatch ({count} != {PULSES_PER_LANE})"
            );
        }
    }

    #[test]
    fn drain_into_returns_count() {
        const LANE_COUNT: usize = 4;
        let prefix = unique_prefix("dr");
        let (ring, mut writers) = MultiLaneRing::create(&prefix, 32, LANE_COUNT).expect("create");

        let pulse = Pulse::zeroed();
        for writer in &mut writers {
            for _ in 0..3 {
                let _ = writer.publish(&pulse);
            }
        }

        let mut cursors = vec![0u64; LANE_COUNT];
        let mut out: Vec<(usize, Pulse)> = (0..20).map(|_| (0usize, Pulse::zeroed())).collect(); // bigger than total
        let n = ring.drain_into(&mut cursors, &mut out).expect("drain");
        assert_eq!(n, 12);
    }

    #[test]
    fn invalid_lane_count_rejected() {
        let prefix = unique_prefix("ilc");
        let err = MultiLaneRing::create(&prefix, 64, 0).unwrap_err();
        match err {
            MultiLaneError::InvalidLaneCount { got: 0 } => {}
            other => panic!("expected InvalidLaneCount, got {other:?}"),
        }
    }

    #[test]
    fn cursor_slice_mismatch_rejected() {
        let prefix = unique_prefix("csm");
        let (ring, _writers) = MultiLaneRing::create(&prefix, 64, 4).expect("create");
        let mut wrong_size = vec![0u64; 3]; // ring has 4 lanes
        let err = ring.poll_round_robin(&mut wrong_size).unwrap_err();
        match err {
            MultiLaneError::CursorSliceMismatch {
                got: 3,
                expected: 4,
            } => {}
            other => panic!("expected CursorSliceMismatch, got {other:?}"),
        }
    }

    /// `create_with_numa` gives working lanes on every OS. Where `mbind` is
    /// refused (a seccomp filter, an unprivileged container) the test skips.
    #[test]
    fn create_with_numa_round_trips() {
        const LANE_COUNT: usize = 4;
        let prefix = unique_prefix("numa");
        let lane_to_node: Vec<u32> = vec![0; LANE_COUNT];

        let (ring, mut writers) = match MultiLaneRing::create_with_numa(&prefix, 64, &lane_to_node)
        {
            Ok(pair) => pair,
            Err(MultiLaneError::NumaBind { ref source, .. }) => {
                eprintln!("SKIP create_with_numa_round_trips: mbind unavailable ({source})");
                return;
            }
            Err(e) => panic!("create_with_numa unexpected error: {e}"),
        };
        assert_eq!(ring.lane_count(), LANE_COUNT);

        let pulse = Pulse::zeroed();
        for writer in &mut writers {
            let _ = writer.publish(&pulse);
        }
        let mut cursors = vec![0u64; LANE_COUNT];
        let mut seen = vec![false; LANE_COUNT];
        for _ in 0..LANE_COUNT {
            let (lane_idx, _) = ring
                .poll_round_robin(&mut cursors)
                .expect("poll")
                .expect("expected a pulse");
            seen[lane_idx] = true;
        }
        for (i, was_seen) in seen.iter().enumerate() {
            assert!(*was_seen, "lane {i} not seen after create_with_numa");
        }
    }

    /// Zero-lane `create_with_numa` must return `InvalidLaneCount` on all
    /// platforms (both cfg paths validate the empty-slice case).
    #[test]
    fn create_with_numa_zero_lanes_rejected() {
        let prefix = unique_prefix("n0");
        let err = MultiLaneRing::create_with_numa(&prefix, 64, &[]).unwrap_err();
        match err {
            MultiLaneError::InvalidLaneCount { got: 0 } => {}
            other => panic!("expected InvalidLaneCount, got {other:?}"),
        }
    }

    /// `pin_lane_writer` must not panic on any platform (Linux: actual pin;
    /// non-Linux: no-op returning Ok).
    #[test]
    fn pin_lane_writer_node_zero_is_ok() {
        super::pin_lane_writer(0, 0).expect("pin_lane_writer");
    }

    /// A pulse carrying `seq` in two fields, so a copy mixing two publishes
    /// is caught.
    fn tagged(seq: u64) -> Pulse {
        let mut pulse = Pulse::zeroed();
        pulse.monotonic_nanos = seq;
        pulse.chain_step = !seq;
        pulse
    }

    /// The tag of a delivered pulse, checked against its complement.
    fn tag_of(pulse: &Pulse) -> u64 {
        assert_eq!(pulse.chain_step, !pulse.monotonic_nanos, "torn pulse");
        pulse.monotonic_nanos
    }

    /// The tags of every pulse lane `lane` yields now.
    fn drain_lane(ring: &MultiLaneRing, cursors: &mut [u64], lane: usize) -> Vec<u64> {
        let mut got = Vec::new();
        while let Some((index, pulse)) = ring.poll_round_robin(cursors).expect("poll") {
            assert_eq!(index, lane);
            got.push(tag_of(&pulse));
        }
        got
    }

    #[test]
    fn overrun_is_counted_per_lane_and_never_redelivered() {
        // Lane 1 takes twenty publishes into eight slots before the reader
        // looks; lane 0 takes three and is the control.
        let prefix = unique_prefix("ov");
        let (ring, mut writers) = MultiLaneRing::create(&prefix, 8, 2).expect("create");
        for seq in 0..3 {
            let _ = writers[0].publish(&tagged(seq));
        }
        for seq in 0..20 {
            let _ = writers[1].publish(&tagged(seq));
        }
        let mut cursors = vec![0u64; 2];
        let mut got: [Vec<u64>; 2] = Default::default();
        while let Some((lane, pulse)) = ring.poll_round_robin(&mut cursors).expect("poll") {
            got[lane].push(tag_of(&pulse));
        }
        assert_eq!(got[0], [0, 1, 2]);
        assert_eq!(got[1], (12..20).collect::<Vec<_>>());
        assert_eq!((ring.lost(0), ring.lost(1)), (0, 12));
        assert_eq!(cursors, [3, 20]);
    }

    #[test]
    fn a_lane_that_keeps_up_loses_nothing() {
        let prefix = unique_prefix("ku");
        let (ring, mut writers) = MultiLaneRing::create(&prefix, 8, 1).expect("create");
        let mut cursors = vec![0u64; 1];
        let mut next = 0u64;
        let mut all = Vec::new();
        // Bursts of every size up to a full lane, each drained before the next.
        for burst in (1..=8).chain([8, 3, 8]) {
            for _ in 0..burst {
                let _ = writers[0].publish(&tagged(next));
                next += 1;
            }
            all.extend(drain_lane(&ring, &mut cursors, 0));
        }
        assert_eq!(all, (0..next).collect::<Vec<_>>());
        assert_eq!(ring.lost(0), 0);
    }

    #[test]
    fn a_writer_that_laps_the_reader_mid_read_is_detected() {
        // The reader passes its overrun check at cursor 0 with four pulses
        // written; before its read the writer publishes nine more, so slot 0
        // holds pulse 8. The SeqLock cannot see a write that finished before
        // the read began, so only the check after the read keeps pulse 8 from
        // being delivered as cursor 0, and again at cursor 8.
        let prefix = unique_prefix("mr");
        let (ring, mut writers) = MultiLaneRing::create(&prefix, 8, 1).expect("create");
        for seq in 0..4 {
            let _ = writers[0].publish(&tagged(seq));
        }
        let mut cursors = vec![0u64; 1];
        let mut laps = 0;
        let got = ring
            .poll_round_robin_with(&mut cursors, |lane, cursor| {
                assert_eq!((lane, cursor), (0, 0));
                for seq in 4..13 {
                    let _ = writers[0].publish(&tagged(seq));
                }
                laps += 1;
            })
            .expect("poll");
        assert_eq!(laps, 1);
        assert!(got.is_none(), "the lapped copy was delivered");
        // Cursor 0 was read and discarded; cursors 1 to 4 were overwritten.
        assert_eq!((ring.lost(0), cursors[0]), (5, 5));
        assert_eq!(
            drain_lane(&ring, &mut cursors, 0),
            (5..13).collect::<Vec<_>>()
        );
        assert_eq!(ring.lost(0), 5);
    }

    #[test]
    fn a_next_lap_written_but_not_yet_published_is_not_delivered_early() {
        // A lane writer fills a slot and advances its cursor after. Stand
        // for one paused between the two with `publish_at_slot`, which writes
        // a slot under its SeqLock as `publish` does and leaves the cursor
        // alone. The reader, a full lane behind at cursor 0, then reads
        // pulse 8 from slot 0 while the write cursor still reads 8, exactly
        // a capacity ahead; only the slot's counter shows the newer lap.
        let prefix = unique_prefix("np");
        let (ring, mut writers) = MultiLaneRing::create(&prefix, 8, 1).expect("create");
        for seq in 0..8 {
            let _ = writers[0].publish(&tagged(seq));
        }
        let mut cursors = vec![0u64; 1];
        let got = ring
            .poll_round_robin_with(&mut cursors, |_, _| {
                let _ = writers[0].ring_mut().publish_at_slot(8, &tagged(8));
            })
            .expect("poll");
        assert!(got.is_none(), "the next lap was delivered as cursor 0");
        assert_eq!((ring.lost(0), cursors[0]), (1, 1));
        // The writer resumes: publishing pulse 8 rewrites its slot and
        // advances, and pulse 8 is read once, at its own position.
        let _ = writers[0].publish(&tagged(8));
        assert_eq!(
            drain_lane(&ring, &mut cursors, 0),
            (1..9).collect::<Vec<_>>()
        );
        assert_eq!(ring.lost(0), 1);
    }

    /// A reader is told, lane by lane once each is drained, that the writer
    /// left. The writer's handle creates the lanes and marks them superseded
    /// as it drops them; the reader's, made the same way, attaches to them.
    #[test]
    fn a_lane_whose_writer_left_is_reported_once_it_is_drained() {
        let prefix = unique_prefix("sup");
        let (writer, mut writers) = MultiLaneRing::create(&prefix, 8, 2).expect("create");
        let (reader, attached) = MultiLaneRing::create(&prefix, 8, 2).expect("attach");
        assert!(writers[0].ring().is_creator() && !attached[0].ring().is_creator());
        assert!(!writer.lane(0).is_creator() && !reader.lane(0).is_creator());
        drop(attached);
        for seq in 0..3 {
            let _ = writers[1].publish(&tagged(seq));
        }
        let mut cursors = vec![0u64; 2];
        let (lane, pulse) = ring_pulse(&reader, &mut cursors);
        assert_eq!((lane, tag_of(&pulse)), (1, 0));
        assert!(!reader.lane_superseded(0) && !reader.lane_superseded(1));

        // The writer leaves with two pulses unread on lane 1 and none on
        // lane 0. Lane 0 is reported on the next poll; lane 1 is read to
        // its end first.
        drop(writers);
        drop(writer);
        assert!(reader.lane(0).is_superseded() && reader.lane(1).is_superseded());
        let (lane, pulse) = ring_pulse(&reader, &mut cursors);
        assert_eq!((lane, tag_of(&pulse)), (1, 1));
        assert!(
            reader.lane_superseded(0),
            "an empty lane is reported at once"
        );
        assert!(!reader.lane_superseded(1), "lane 1 still holds a pulse");
        let (lane, pulse) = ring_pulse(&reader, &mut cursors);
        assert_eq!((lane, tag_of(&pulse)), (1, 2));
        assert!(!reader.lane_superseded(1), "reported only once drained");
        assert!(reader
            .poll_round_robin(&mut cursors)
            .expect("poll")
            .is_none());
        assert!(reader.lane_superseded(1), "reported once drained");

        // Nothing more comes, and the report stays.
        assert!(reader
            .poll_round_robin(&mut cursors)
            .expect("poll")
            .is_none());
        assert!(reader.lane_superseded(0) && reader.lane_superseded(1));
        assert_eq!(cursors, [0, 3]);
        assert_eq!((reader.lost(0), reader.lost(1)), (0, 0));
    }

    #[test]
    fn a_lane_whose_writer_stays_is_never_reported() {
        let prefix = unique_prefix("stay");
        let (ring, mut writers) = MultiLaneRing::create(&prefix, 8, 1).expect("create");
        let mut cursors = vec![0u64; 1];
        for seq in 0..3 {
            let _ = writers[0].publish(&tagged(seq));
        }
        assert_eq!(drain_lane(&ring, &mut cursors, 0), [0, 1, 2]);
        assert!(ring.poll_round_robin(&mut cursors).expect("poll").is_none());
        assert!(
            !ring.lane_superseded(0),
            "a caught-up lane is not superseded"
        );
    }

    /// The next pulse `ring` yields, which must be there.
    fn ring_pulse(ring: &MultiLaneRing, cursors: &mut [u64]) -> (usize, Pulse) {
        ring.poll_round_robin(cursors)
            .expect("poll")
            .expect("a pulse")
    }

    /// Statistical: a lane writer publishing flat out laps a reader that
    /// pauses between polls, many times over. Whatever the interleaving,
    /// every pulse reaches the reader once, at its own position, or is
    /// counted lost once. Without the overrun checks the first lap delivers
    /// a newer pulse under an older position.
    #[test]
    fn concurrent_overrun_is_counted_never_redelivered() {
        const PUBLISHES: u64 = 100_000;
        let prefix = unique_prefix("co");
        let (ring, writers) = MultiLaneRing::create(&prefix, 16, 1).expect("create");
        let writer = {
            let mut lane = writers.into_iter().next().expect("one lane");
            std::thread::spawn(move || {
                for seq in 0..PUBLISHES {
                    let _ = lane.publish(&tagged(seq));
                }
            })
        };
        let mut cursors = vec![0u64; 1];
        let mut delivered = 0u64;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while cursors[0] < PUBLISHES {
            assert!(
                std::time::Instant::now() < deadline,
                "reader stuck at {}",
                cursors[0]
            );
            if let Some((_, pulse)) = ring.poll_round_robin(&mut cursors).expect("poll") {
                assert_eq!(
                    tag_of(&pulse),
                    cursors[0] - 1,
                    "delivered under another position"
                );
                delivered += 1;
            }
            for _ in 0..64 {
                std::hint::spin_loop();
            }
        }
        writer.join().expect("writer");
        assert_eq!(delivered + ring.lost(0), PUBLISHES);
        assert!(ring.lost(0) > 0, "the writer never lapped the reader");
    }

    /// The same with batches of seven into an eight-slot lane: a batch that
    /// moved the lane's cursor only at its end would let the reader take its
    /// pulses for the positions a lap earlier.
    #[test]
    fn concurrent_batches_are_counted_never_redelivered() {
        const BATCH: u64 = 7;
        const PUBLISHES: u64 = 20_000 * BATCH;
        let prefix = unique_prefix("cb");
        let (ring, writers) = MultiLaneRing::create(&prefix, 8, 1).expect("create");
        let writer = {
            let mut lane = writers.into_iter().next().expect("one lane");
            std::thread::spawn(move || {
                for first in (0..PUBLISHES).step_by(BATCH as usize) {
                    let batch: Vec<Pulse> = (first..first + BATCH).map(tagged).collect();
                    lane.ring_mut()
                        .publish_batch(&batch)
                        .expect("a batch of seven fits an eight-slot lane");
                }
            })
        };
        let mut cursors = vec![0u64; 1];
        let mut delivered = 0u64;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while cursors[0] < PUBLISHES {
            assert!(
                std::time::Instant::now() < deadline,
                "reader stuck at {}",
                cursors[0]
            );
            if let Some((_, pulse)) = ring.poll_round_robin(&mut cursors).expect("poll") {
                assert_eq!(
                    tag_of(&pulse),
                    cursors[0] - 1,
                    "delivered under another position"
                );
                delivered += 1;
            }
            for _ in 0..64 {
                std::hint::spin_loop();
            }
        }
        writer.join().expect("writer");
        assert_eq!(delivered + ring.lost(0), PUBLISHES);
        assert!(ring.lost(0) > 0, "the writer never lapped the reader");
    }
}
