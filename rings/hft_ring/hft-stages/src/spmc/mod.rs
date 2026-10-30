// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! `hft_stages::spmc`: a single-producer, multi-consumer ring in process
//! memory, with a cursor per reader (module M04).
//!
//! A publish is written once and read by every reader, where a ring per
//! consumer would copy it once per consumer. The writer is the ring's one
//! [`SpmcWriter`], made with it by [`SpmcRing::new`]; each reader is an
//! [`SpmcReader`] from [`SpmcRing::register_reader`], the one handle on its
//! cursor. The writer never waits for readers: a reader it laps gets
//! [`SpmcError::Lagged`].
//!
//! Ring layout (header followed by slot array):
//!
//! ```text
//! Header (256 bytes):
//!   bytes  0..  8  write_cursor (AtomicU64) — writer-only field
//!   bytes  8.. 16  total_writes (AtomicU64) — writer increments per publish
//!   bytes 16.. 24  capacity (u64) — fixed at create
//!   bytes 24.. 32  max_readers (u64) — fixed at create
//!   bytes 32.. 40  registered_readers (AtomicU64) — readers atomic-fetch-add at register
//!   bytes 40..168  16 reader cursors @ 8-byte stride (128 bytes)
//!   bytes 168..256 reserved
//!
//! Slot array (each slot is PULSE_SIZE = 128 bytes):
//!   bytes 0..8     SeqLock counter (AtomicU64) — same protocol as PulseRing
//!   bytes 8..128   Pulse data area (120 byte payload)
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use thiserror::Error;

use crate::overrun;

const HEADER_SIZE: usize = 256;
const SLOT_SIZE: usize = 128;
const SLOT_DATA_OFFSET: usize = 8;
const SLOT_DATA_SIZE: usize = SLOT_SIZE - SLOT_DATA_OFFSET;
const MAX_READERS: usize = 16;

/// One cache line: a `Vec<Line>` is a 64-byte-aligned backing that frees
/// itself.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Line([u8; 64]);

/// Errors returned by [`SpmcRing`] operations.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SpmcError {
    /// Underlying I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Every reader cursor of the ring is taken.
    #[error("too many readers registered: {got} >= {max}")]
    TooManyReaders {
        /// Readers registered when the limit was hit.
        got: u64,
        /// The most readers a ring takes, 16.
        max: u64,
    },
    /// The slot for `cursor` held no publish although the writer had passed
    /// it; the cursor moved past it rather than wedge there.
    #[error("slot for cursor {cursor} was empty; the reader cursor skipped it")]
    SlotSkipped {
        /// The reader cursor that was skipped.
        cursor: u64,
    },
    /// The writer overwrote `skipped` publishes before this reader read
    /// them: the reader was more than a capacity behind, or the writer took
    /// the slot's next lap while the reader copied it. The cursor has moved
    /// past them to the oldest publish the ring holds, where the next
    /// [`SpmcReader::recv`] reads, and they are added to
    /// [`SpmcReader::lost`]. Returned once per jump.
    #[error("reader lagged: {skipped} publishes were overwritten before it read them")]
    Lagged {
        /// Publishes the reader lost in this jump.
        skipped: u64,
    },
}

/// The panic [`SpmcRing::new`] documents for a ring size that overflows
/// `usize`, out of line so the constructor stays small.
#[cold]
#[allow(clippy::panic)] // a documented precondition, like the zero-capacity assert
fn capacity_overflow(capacity: u64) -> ! {
    panic!(
        "spmc capacity {capacity} is too large: {HEADER_SIZE} + {capacity} x {SLOT_SIZE} bytes \
         overflows usize"
    )
}

/// The part of a single-producer, multi-consumer ring its readers share:
/// every reader sees every publish, through a cursor of its own.
///
/// [`SpmcRing::new`] returns it in an `Arc`, with its one [`SpmcWriter`];
/// each reader reads through the [`SpmcReader`] that
/// [`SpmcRing::register_reader`] gives it.
pub struct SpmcRing {
    ptr: *mut u8,
    capacity: u64,
    /// The memory `ptr` points into, kept alive here; `Line` makes it
    /// 64-byte aligned, which every `AtomicU64` cast relies on.
    #[allow(dead_code)]
    backing: Vec<Line>,
    /// Publishes each reader has lost to overrun, indexed by cursor.
    lost: [AtomicU64; MAX_READERS],
}

// SAFETY: `ptr` points into `backing`, which the ring owns and never
// touches through the `Vec` again, so the pointer stays valid wherever the
// ring moves.
unsafe impl Send for SpmcRing {}
// SAFETY: a shared ring's backing is touched only by atomic loads and
// stores: the cursors, the counts, each slot's SeqLock word, and the slot
// data, which readers and the publish copy as atomic words
// (`core_region::shm::load_words`, `store_words`) under the slot's SeqLock. Those are made only by the ring's one `SpmcWriter`, through
// `&mut self`, so no two of them overlap; a reader's copy that overlaps
// one is discarded by the SeqLock check, the protocol every ring here uses.
unsafe impl Sync for SpmcRing {}

/// The one writer of an [`SpmcRing`], made with it by [`SpmcRing::new`].
///
/// Two publishes into one slot at once would leave a mix of both payloads
/// that the slot's SeqLock passes as stable, so a ring has one writer: it
/// cannot be cloned, and [`SpmcWriter::publish`] takes `&mut self`. It can
/// move to another thread; publishers on several threads share it behind a
/// `Mutex`.
///
/// ```compile_fail,E0599
/// let (writer, _ring) = hft_stages::spmc::SpmcRing::new(8);
/// let _second = writer.clone(); // there is no second writer
/// ```
///
/// ```compile_fail,E0499
/// let (mut writer, _ring) = hft_stages::spmc::SpmcRing::new(8);
/// std::thread::scope(|s| {
///     s.spawn(|| writer.publish(b"one"));
///     s.spawn(|| writer.publish(b"two")); // nor two publishes at once
/// });
/// ```
pub struct SpmcWriter {
    ring: Arc<SpmcRing>,
}

/// One reader of an [`SpmcRing`], made by [`SpmcRing::register_reader`]: the
/// one handle on the reader cursor it claimed.
///
/// Two reads on one cursor at once would both deliver the same publish, so
/// a cursor has one reader: it cannot be cloned or made from a number, and
/// [`SpmcReader::recv`] takes `&mut self`. It can move to another thread;
/// consumers on several threads that share one cursor share its reader
/// behind a `Mutex`, and between them receive each publish once.
///
/// A registration lasts as long as the ring: dropping a reader does not
/// give its place back, and its cursor stays where the reader left it, in
/// [`SpmcRing::slowest_reader_cursor`].
///
/// ```compile_fail,E0599
/// let (_writer, ring) = hft_stages::spmc::SpmcRing::new(8);
/// let reader = ring.register_reader().expect("a reader");
/// let _second = reader.clone(); // there is no second handle on a cursor
/// ```
///
/// ```compile_fail,E0499
/// let (_writer, ring) = hft_stages::spmc::SpmcRing::new(8);
/// let mut reader = ring.register_reader().expect("a reader");
/// std::thread::scope(|s| {
///     s.spawn(|| reader.recv(&mut [0u8; 120]));
///     s.spawn(|| reader.recv(&mut [0u8; 120])); // nor two reads on it at once
/// });
/// ```
///
/// ```compile_fail,E0599
/// let (_writer, ring) = hft_stages::spmc::SpmcRing::new(8);
/// let _ = ring.recv(5, &mut [0u8; 120]); // nor a read by a number
/// ```
pub struct SpmcReader {
    ring: Arc<SpmcRing>,
    /// Which of the ring's reader cursors this reader owns: below
    /// `MAX_READERS`, fixed at registration.
    index: usize,
}

impl SpmcRing {
    /// A ring of `capacity` slots in process memory, with its one writer;
    /// readers share the `Arc`.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero, since cursor arithmetic divides by it,
    /// or if the ring's size, a 256-byte header plus `capacity` 128-byte
    /// slots, does not fit in `usize`.
    pub fn new(capacity: u64) -> (SpmcWriter, Arc<SpmcRing>) {
        assert!(capacity >= 1, "spmc capacity must be at least 1");
        let Some(total_size) = usize::try_from(capacity)
            .ok()
            .and_then(|slots| slots.checked_mul(SLOT_SIZE))
            .and_then(|bytes| bytes.checked_add(HEADER_SIZE))
        else {
            capacity_overflow(capacity)
        };
        let lines = total_size.div_ceil(64);
        let mut backing = vec![Line([0u8; 64]); lines];
        let ptr = backing.as_mut_ptr().cast::<u8>();
        debug_assert_eq!(ptr as usize % 64, 0, "backing must be cache-line aligned");

        // SAFETY: ptr is valid for lines * 64 >= total_size bytes and is
        // 64-byte aligned (`Line` is `align(64)`); the Vec is never touched
        // again, so the address is stable. Both writes lie in the 256-byte
        // header, at offsets that are multiples of 8.
        unsafe {
            (ptr.add(16) as *mut u64).write(capacity);
            (ptr.add(24) as *mut u64).write(MAX_READERS as u64);
        }

        let ring = Arc::new(Self {
            ptr,
            capacity,
            backing,
            lost: [const { AtomicU64::new(0) }; MAX_READERS],
        });
        let writer = SpmcWriter {
            ring: Arc::clone(&ring),
        };
        (writer, ring)
    }

    /// Register a reader: it claims one of the ring's 16 reader cursors, set
    /// to the write cursor, so it reads from the next publish on.
    ///
    /// # Errors
    ///
    /// [`SpmcError::TooManyReaders`] if all 16 are taken; concurrent
    /// registrations never take more.
    pub fn register_reader(self: &Arc<Self>) -> Result<SpmcReader, SpmcError> {
        let registered = self.registered_readers_atomic();
        let id = registered.fetch_add(1, Ordering::AcqRel);
        let Some(index) = usize::try_from(id).ok().filter(|&i| i < MAX_READERS) else {
            registered.fetch_sub(1, Ordering::AcqRel);
            return Err(SpmcError::TooManyReaders {
                got: id,
                max: MAX_READERS as u64,
            });
        };
        let write_cursor = self.write_cursor();
        self.reader_cursors()[index].store(write_cursor, Ordering::Release);
        Ok(SpmcReader {
            ring: Arc::clone(self),
            index,
        })
    }
}

impl SpmcWriter {
    /// The ring this writer publishes to, shared by its readers.
    pub fn ring(&self) -> &Arc<SpmcRing> {
        &self.ring
    }

    /// Publish `payload` into the next slot, zero-filling the rest of its
    /// 120 bytes, and return the new write cursor (1-indexed).
    ///
    /// # Panics
    ///
    /// If `payload` is longer than 120 bytes: a precondition, so the hot
    /// path carries no `Result`.
    pub fn publish(&mut self, payload: &[u8]) -> u64 {
        let cursor = self.ring.write_cursor_atomic().load(Ordering::Acquire);
        self.write_slot(cursor, payload);
        self.advance_write_cursor(cursor)
    }

    /// The first half of a publish, the SeqLock write into `cursor`'s slot,
    /// which readers read only once [`Self::advance_write_cursor`] runs; the
    /// tests call the halves apart. Panics as [`Self::publish`] does.
    #[inline(always)]
    fn write_slot(&mut self, cursor: u64, payload: &[u8]) {
        assert!(
            payload.len() <= SLOT_DATA_SIZE,
            "payload exceeds {} bytes",
            SLOT_DATA_SIZE
        );
        let slot = cursor % self.ring.capacity;
        let slot_ptr = self.ring.slot_ptr(slot);

        // SAFETY: slot_ptr is the start of slot `cursor % capacity` inside
        // the backing (HEADER_SIZE + capacity * SLOT_SIZE bytes), 8-byte
        // aligned via the slot's 64-byte alignment; the assert above keeps
        // the copy and the zero-fill inside the slot's 120-byte data area.
        // `&mut self` on the ring's only writer means no other copy into the
        // slot runs meanwhile; readers' copies are checked by the SeqLock.
        unsafe {
            let seq = &*(slot_ptr as *const AtomicU64);
            // `& !1` re-bases a counter left odd by an interrupted write.
            let prev = seq.load(Ordering::Relaxed) & !1;
            seq.store(prev | 1, Ordering::Release);
            // The odd mark must be visible before any payload byte is.
            std::sync::atomic::fence(Ordering::Release);

            // The payload, zero-filled so no byte of an older one remains,
            // stored as atomic words: SLOT_DATA_SIZE is a multiple of eight
            // and the data area starts 8-byte aligned.
            let mut data = [0u8; SLOT_DATA_SIZE];
            data[..payload.len()].copy_from_slice(payload);
            core_region::shm::store_words(slot_ptr.add(SLOT_DATA_OFFSET), &data);

            seq.store(prev.wrapping_add(2), Ordering::Release);
        }
    }

    /// The second half of a publish: advance the write cursor past `cursor`,
    /// which makes it readable. Returns the new write cursor.
    #[inline(always)]
    fn advance_write_cursor(&mut self, cursor: u64) -> u64 {
        let new_cursor = cursor + 1;
        self.ring
            .write_cursor_atomic()
            .store(new_cursor, Ordering::Release);
        self.ring
            .total_writes_atomic()
            .fetch_add(1, Ordering::Release);
        new_cursor
    }
}

impl SpmcReader {
    /// The ring this reader reads.
    pub fn ring(&self) -> &Arc<SpmcRing> {
        &self.ring
    }

    /// Read this reader's next publish into `out`: `Some(120)`, the bytes
    /// written, or `None` when the reader is caught up or the slot's SeqLock
    /// did not settle within the retry budget. A reader never receives a
    /// publish under another's position, or twice.
    ///
    /// # Errors
    ///
    /// - [`SpmcError::Lagged`] when the writer overwrote publishes this
    ///   reader had not read, found before the read (more than a capacity
    ///   behind) or after the copy (the writer took the slot's next lap).
    /// - [`SpmcError::SlotSkipped`] when the slot for the cursor holds no
    ///   publish although the writer has passed it.
    ///
    /// Either way the cursor has moved on, and the next call reads on.
    ///
    /// # Panics
    ///
    /// If `out` is shorter than 120 bytes, as [`SpmcWriter::publish`] panics.
    pub fn recv(&mut self, out: &mut [u8]) -> Result<Option<usize>, SpmcError> {
        self.recv_with(out, || {})
    }

    /// [`Self::recv`], running `before_copy` between the overrun check and
    /// the copy: the tests' seam for a writer that moves in between.
    #[inline(always)]
    fn recv_with(
        &mut self,
        out: &mut [u8],
        before_copy: impl FnOnce(),
    ) -> Result<Option<usize>, SpmcError> {
        assert!(
            out.len() >= SLOT_DATA_SIZE,
            "out buffer must be ≥ {} bytes",
            SLOT_DATA_SIZE
        );
        let ring = &*self.ring;
        let reader_cursor = self.cursor();
        let my_cursor = reader_cursor.load(Ordering::Acquire);
        let write_cursor = ring.write_cursor();

        if my_cursor >= write_cursor {
            return Ok(None);
        }
        // Overrun before the read: everything below `write_cursor -
        // capacity` has been overwritten.
        let oldest = overrun::resume_at(my_cursor, write_cursor, ring.capacity);
        if oldest > my_cursor {
            return Err(self.skip(my_cursor, oldest));
        }
        before_copy();

        let slot = my_cursor % ring.capacity;
        let slot_ptr = ring.slot_ptr(slot);

        for _ in 0..100 {
            // SAFETY: `slot_ptr(slot)`, with `slot < capacity`, is inside the
            // backing, and a slot's first 8 bytes are its SeqLock `AtomicU64`,
            // aligned since slots are 64-byte aligned.
            let v1 = unsafe { (*(slot_ptr as *const AtomicU64)).load(Ordering::Acquire) };
            if v1 & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            // SAFETY: `slot_ptr.add(SLOT_DATA_OFFSET)` is the slot's
            // SLOT_DATA_SIZE-byte data area, 8-byte aligned and only ever
            // accessed as atomic words; `out` holds at least that many bytes
            // (asserted above) and, a `&mut [u8]` of the caller's, cannot
            // overlap the ring's private backing.
            unsafe {
                core_region::shm::load_words(
                    slot_ptr.add(SLOT_DATA_OFFSET),
                    &mut out[..SLOT_DATA_SIZE],
                );
            }
            // The copy's loads must complete before the counter is re-read.
            std::sync::atomic::fence(Ordering::Acquire);
            // SAFETY: same pointer-validity invariant as the v1 load above.
            let v2 = unsafe { (*(slot_ptr as *const AtomicU64)).load(Ordering::Acquire) };
            if v1 != v2 {
                std::hint::spin_loop();
                continue;
            }
            if v1 == 0 {
                // Written, yet zeroed since: skip it rather than wedge here.
                reader_cursor.store(my_cursor + 1, Ordering::Release);
                return Err(SpmcError::SlotSkipped { cursor: my_cursor });
            }
            // Overrun during the read: the writer may have refilled the slot
            // since the check. `v1` was acquired from the write the copy
            // holds, which began only after the writer advanced past every
            // earlier position, so `head` is at least that write's.
            let head = ring.write_cursor();
            if overrun::lapped(my_cursor, head, ring.capacity, || v1) {
                let next = overrun::resume_at(my_cursor + 1, head, ring.capacity);
                return Err(self.skip(my_cursor, next));
            }
            reader_cursor.store(my_cursor + 1, Ordering::Release);
            return Ok(Some(SLOT_DATA_SIZE));
        }
        Ok(None) // torn-read budget exhausted
    }

    /// Move this reader from `from` to `to` past publishes it will never
    /// see, count them, and return the report.
    fn skip(&self, from: u64, to: u64) -> SpmcError {
        let skipped = to - from;
        self.cursor().store(to, Ordering::Release);
        self.ring.lost[self.index].fetch_add(skipped, Ordering::Relaxed);
        SpmcError::Lagged { skipped }
    }

    /// Publishes this reader has lost to overrun: the sum of the `skipped`
    /// of every [`SpmcError::Lagged`] it has returned. An empty slot
    /// ([`SpmcError::SlotSkipped`]) is not counted.
    pub fn lost(&self) -> u64 {
        self.ring.lost[self.index].load(Ordering::Relaxed)
    }

    /// This reader's cursor in the ring's header.
    fn cursor(&self) -> &AtomicU64 {
        &self.ring.reader_cursors()[self.index]
    }
}

impl SpmcRing {
    /// The lowest cursor among the registered readers, or 0 with none.
    pub fn slowest_reader_cursor(&self) -> u64 {
        let registered = self.registered_readers_atomic().load(Ordering::Acquire);
        let registered = usize::try_from(registered).map_or(MAX_READERS, |n| n.min(MAX_READERS));
        let mut min_cursor = u64::MAX;
        for rc in &self.reader_cursors()[..registered] {
            let c = rc.load(Ordering::Acquire);
            if c < min_cursor {
                min_cursor = c;
            }
        }
        if min_cursor == u64::MAX {
            0
        } else {
            min_cursor
        }
    }

    /// The write cursor: one past the last publish, loaded with `Acquire`,
    /// so every publish below it is visible.
    pub fn write_cursor(&self) -> u64 {
        self.write_cursor_atomic().load(Ordering::Acquire)
    }
    /// Publishes since the ring was made.
    pub fn total_writes(&self) -> u64 {
        self.total_writes_atomic().load(Ordering::Acquire)
    }
    /// Slot capacity of this ring, fixed at create time.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
    /// Number of readers registered against this ring. A registration is
    /// never given back, so a dropped reader still counts.
    pub fn registered_readers(&self) -> u64 {
        self.registered_readers_atomic().load(Ordering::Acquire)
    }

    // The accessors below borrow header fields at their offsets in the
    // module docs' layout. Shared SAFETY argument: `self.ptr` is the start
    // of `backing`, `HEADER_SIZE + capacity * SLOT_SIZE` bytes or more,
    // which lives as long as `self`, so no borrow outlives it; the backing
    // is 64-byte aligned and every offset a multiple of 8.

    fn write_cursor_atomic(&self) -> &AtomicU64 {
        // SAFETY: header offset 0 is the write-cursor AtomicU64.
        unsafe { &*(self.ptr as *const AtomicU64) }
    }
    fn total_writes_atomic(&self) -> &AtomicU64 {
        // SAFETY: header offset 8 is the total-writes AtomicU64.
        unsafe { &*(self.ptr.add(8) as *const AtomicU64) }
    }
    fn registered_readers_atomic(&self) -> &AtomicU64 {
        // SAFETY: header offset 32 is the registered-readers AtomicU64.
        unsafe { &*(self.ptr.add(32) as *const AtomicU64) }
    }
    fn reader_cursors(&self) -> &[AtomicU64; MAX_READERS] {
        // SAFETY: header offset 40 begins the reader-cursor array,
        // MAX_READERS AtomicU64s (bytes 40..168), which the 256-byte header
        // is sized for; indexing the array keeps every access inside it.
        unsafe { &*(self.ptr.add(40) as *const [AtomicU64; MAX_READERS]) }
    }
    fn slot_ptr(&self, slot: u64) -> *mut u8 {
        // SAFETY: every caller passes `slot = cursor % capacity`, so
        // HEADER_SIZE + slot * SLOT_SIZE is below HEADER_SIZE + capacity *
        // SLOT_SIZE, the backing's size: the pointer stays inside it.
        unsafe { self.ptr.add(HEADER_SIZE + (slot as usize) * SLOT_SIZE) }
    }
}

#[cfg(test)]
// Tests fail fast: a setup error is a failed test.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_constants() {
        assert_eq!(HEADER_SIZE, 256);
        assert_eq!(SLOT_SIZE, 128);
        assert_eq!(SLOT_DATA_SIZE, 120);
    }

    #[test]
    fn backing_is_cache_line_aligned() {
        let (_writer, ring) = SpmcRing::new(3);
        assert_eq!(ring.ptr as usize % 64, 0);
        assert_eq!(ring.slot_ptr(1) as usize % 64, 0);
    }

    #[test]
    #[should_panic(expected = "capacity")]
    fn zero_capacity_panics() {
        let _ = SpmcRing::new(0);
    }

    #[test]
    fn reader_survives_wrap_around() {
        let (mut writer, ring) = SpmcRing::new(4);
        let mut reader = ring.register_reader().expect("register");
        let mut buf = vec![0u8; SLOT_DATA_SIZE];
        for round in 0..2u64 {
            for seq in round * 4..round * 4 + 4 {
                writer.publish(&seq.to_le_bytes());
            }
            for seq in round * 4..round * 4 + 4 {
                let got = reader.recv(&mut buf).expect("recv");
                assert_eq!(got, Some(SLOT_DATA_SIZE), "seq {seq}");
                assert_eq!(u64::from_le_bytes(buf[..8].try_into().unwrap()), seq);
            }
        }
        assert_eq!(ring.slowest_reader_cursor(), 8);
    }

    #[test]
    fn zeroed_slot_below_write_cursor_is_skipped_not_wedging() {
        let (mut writer, ring) = SpmcRing::new(4);
        let mut reader = ring.register_reader().expect("register"); // cursor 0
        writer.publish(&1u64.to_le_bytes());
        writer.publish(&2u64.to_le_bytes());
        // SAFETY: slot 0's first 8 bytes are its aligned SeqLock counter,
        // inside the ring's backing; zeroing it stands for a slot that lost
        // its frame under the reader.
        unsafe { (*(ring.slot_ptr(0) as *const AtomicU64)).store(0, Ordering::Release) };
        let mut buf = vec![0u8; SLOT_DATA_SIZE];
        match reader.recv(&mut buf) {
            Err(SpmcError::SlotSkipped { cursor }) => assert_eq!(cursor, 0),
            other => panic!("expected SlotSkipped, got {other:?}"),
        }
        let got = reader.recv(&mut buf).expect("recv after the skip");
        assert_eq!(got, Some(SLOT_DATA_SIZE));
        assert_eq!(u64::from_le_bytes(buf[..8].try_into().unwrap()), 2);
    }

    #[test]
    fn single_writer_single_reader_round_trip() {
        let (mut writer, ring) = SpmcRing::new(64);
        let mut reader = ring.register_reader().expect("register");

        let payload = b"hello SPMC ring";
        let cursor = writer.publish(payload);
        assert_eq!(cursor, 1);

        let mut out = vec![0u8; SLOT_DATA_SIZE];
        let n = reader.recv(&mut out).expect("recv");
        assert_eq!(n, Some(SLOT_DATA_SIZE));
        assert_eq!(&out[..payload.len()], payload);
    }

    #[test]
    fn one_writer_n_readers_all_see_every_publish() {
        let (mut writer, ring) = SpmcRing::new(1024);
        const N_READERS: u64 = 4;
        const N_PUBLISH: u64 = 1000;

        let mut readers: Vec<SpmcReader> = Vec::with_capacity(N_READERS as usize);
        for _ in 0..N_READERS {
            readers.push(ring.register_reader().expect("register"));
        }
        assert_eq!(ring.registered_readers(), N_READERS);

        let mut handles = Vec::new();
        for mut reader in readers {
            handles.push(std::thread::spawn(move || {
                let mut received: Vec<u64> = Vec::with_capacity(N_PUBLISH as usize);
                let mut buf = vec![0u8; SLOT_DATA_SIZE];
                while (received.len() as u64) < N_PUBLISH {
                    if let Ok(Some(_)) = reader.recv(&mut buf) {
                        let seq = u64::from_le_bytes(buf[0..8].try_into().unwrap());
                        received.push(seq);
                    } else {
                        std::thread::sleep(std::time::Duration::from_micros(10));
                    }
                }
                received
            }));
        }

        for seq in 0..N_PUBLISH {
            let mut payload = vec![0u8; 16];
            payload[0..8].copy_from_slice(&seq.to_le_bytes());
            writer.publish(&payload);
        }

        let results: Vec<Vec<u64>> = handles
            .into_iter()
            .map(|h| h.join().expect("join"))
            .collect();

        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.len() as u64, N_PUBLISH, "reader {} short", i);
            for (j, &seq) in r.iter().enumerate() {
                assert_eq!(
                    seq, j as u64,
                    "reader {} got seq {} at position {}",
                    i, seq, j
                );
            }
        }
    }

    #[test]
    fn slowest_reader_cursor_tracks_min() {
        let (mut writer, ring) = SpmcRing::new(64);
        let mut r0 = ring.register_reader().unwrap();
        let mut r1 = ring.register_reader().unwrap();
        let mut r2 = ring.register_reader().unwrap();

        for _ in 0..10 {
            writer.publish(b"x");
        }

        let mut buf = vec![0u8; SLOT_DATA_SIZE];
        for _ in 0..5 {
            r0.recv(&mut buf).unwrap();
        }
        for _ in 0..3 {
            r1.recv(&mut buf).unwrap();
        }
        for _ in 0..7 {
            r2.recv(&mut buf).unwrap();
        }

        // Slowest = reader 1 with cursor 3, as the writer sees it too.
        assert_eq!(ring.slowest_reader_cursor(), 3);
        assert_eq!(writer.ring().slowest_reader_cursor(), 3);
    }

    #[test]
    fn too_many_readers_rejected() {
        let (_writer, ring) = SpmcRing::new(64);
        // Each reader is dropped at once: a registration is never given
        // back, so the dropped ones still count.
        for _ in 0..MAX_READERS {
            ring.register_reader().expect("should succeed");
        }
        let result = ring.register_reader();
        assert!(matches!(result, Err(SpmcError::TooManyReaders { .. })));
        assert_eq!(ring.registered_readers(), MAX_READERS as u64);
    }

    #[test]
    #[should_panic(expected = "overflows usize")]
    fn a_capacity_whose_size_overflows_panics() {
        let _ = SpmcRing::new(u64::MAX);
    }

    /// A payload carrying `seq` at bytes 0..8 and its complement at bytes
    /// 112..120, so a copy mixing two publishes is caught.
    fn tagged(seq: u64) -> [u8; SLOT_DATA_SIZE] {
        let mut payload = [0u8; SLOT_DATA_SIZE];
        payload[..8].copy_from_slice(&seq.to_le_bytes());
        payload[SLOT_DATA_SIZE - 8..].copy_from_slice(&(!seq).to_le_bytes());
        payload
    }

    /// The tag of a received payload, checked against its complement.
    fn tag_of(buf: &[u8]) -> u64 {
        let seq = u64::from_le_bytes(buf[..8].try_into().unwrap());
        let check = u64::from_le_bytes(buf[SLOT_DATA_SIZE - 8..SLOT_DATA_SIZE].try_into().unwrap());
        assert_eq!(check, !seq, "torn payload for tag {seq}");
        seq
    }

    /// Everything `reader` can read now, and the `skipped` of each `Lagged`
    /// returned on the way.
    fn drain(reader: &mut SpmcReader) -> (Vec<u64>, Vec<u64>) {
        let mut buf = vec![0u8; SLOT_DATA_SIZE];
        let (mut got, mut lagged) = (Vec::new(), Vec::new());
        loop {
            match reader.recv(&mut buf) {
                Ok(Some(_)) => got.push(tag_of(&buf)),
                Ok(None) => return (got, lagged),
                Err(SpmcError::Lagged { skipped }) => lagged.push(skipped),
                Err(other) => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn overrun_jumps_to_the_oldest_publish_and_reports_lagged_once() {
        let (mut writer, ring) = SpmcRing::new(8);
        let mut reader = ring.register_reader().expect("register");
        for seq in 0..20 {
            writer.publish(&tagged(seq));
        }
        let (got, lagged) = drain(&mut reader);
        assert_eq!(got, (12..20).collect::<Vec<_>>());
        assert_eq!(lagged, [12]);
        assert_eq!(reader.lost(), 12);
    }

    #[test]
    fn a_reader_that_keeps_up_loses_nothing() {
        let (mut writer, ring) = SpmcRing::new(8);
        let mut reader = ring.register_reader().expect("register");
        let mut next = 0u64;
        let mut all = Vec::new();
        // Bursts of every size up to a full ring, each drained before the next.
        for burst in (1..=8).chain([8, 3, 8]) {
            for _ in 0..burst {
                writer.publish(&tagged(next));
                next += 1;
            }
            let (got, lagged) = drain(&mut reader);
            assert!(
                lagged.is_empty(),
                "lagged {lagged:?} after a burst of {burst}"
            );
            all.extend(got);
        }
        assert_eq!(all, (0..next).collect::<Vec<_>>());
        assert_eq!(reader.lost(), 0);
    }

    #[test]
    fn a_writer_that_laps_the_reader_mid_read_is_detected() {
        // The reader passes its overrun check at cursor 0 with four publishes
        // written; before its copy the writer publishes nine more, so slot 0
        // holds publish 8. The SeqLock cannot see a write that finished before
        // the copy began, so only the check after the copy keeps publish 8
        // from being delivered as cursor 0, and again at cursor 8.
        let (mut writer, ring) = SpmcRing::new(8);
        let mut reader = ring.register_reader().expect("register");
        for seq in 0..4 {
            writer.publish(&tagged(seq));
        }
        let mut buf = vec![0u8; SLOT_DATA_SIZE];
        let got = reader.recv_with(&mut buf, || {
            for seq in 4..13 {
                writer.publish(&tagged(seq));
            }
        });
        // Cursor 0 was copied and discarded; cursors 1 to 4 were overwritten.
        match got {
            Err(SpmcError::Lagged { skipped }) => assert_eq!(skipped, 5),
            other => panic!("expected Lagged, got {other:?}"),
        }
        let (rest, lagged) = drain(&mut reader);
        assert_eq!(rest, (5..13).collect::<Vec<_>>());
        assert!(lagged.is_empty(), "lagged {lagged:?}");
        assert_eq!(reader.lost(), 5);
    }

    #[test]
    fn a_next_lap_written_but_not_yet_published_is_not_delivered_early() {
        // The writer fills slot 0 with publish 8 and pauses before advancing
        // its cursor, while the reader, a full ring behind at cursor 0, copies
        // slot 0. The write cursor still reads 8, exactly a capacity ahead;
        // only the slot's counter shows the copy is publish 8.
        let (mut writer, ring) = SpmcRing::new(8);
        let mut reader = ring.register_reader().expect("register");
        for seq in 0..8 {
            writer.publish(&tagged(seq));
        }
        let mut buf = vec![0u8; SLOT_DATA_SIZE];
        let got = reader.recv_with(&mut buf, || writer.write_slot(8, &tagged(8)));
        match got {
            Err(SpmcError::Lagged { skipped }) => assert_eq!(skipped, 1),
            other => panic!("expected Lagged, got {other:?}"),
        }
        // The writer resumes, and publish 8 is read at its own position.
        writer.advance_write_cursor(8);
        let (rest, lagged) = drain(&mut reader);
        assert_eq!(rest, (1..9).collect::<Vec<_>>());
        assert!(lagged.is_empty(), "lagged {lagged:?}");
        assert_eq!(reader.lost(), 1);
    }

    /// Statistical: a writer publishing flat out laps a reader that pauses
    /// between reads, many times over. Whatever the interleaving, every
    /// publish reaches the reader once, at its own position, or is counted
    /// lost once. Without the overrun checks the first lap delivers a newer
    /// publish under an older position.
    #[test]
    fn concurrent_overrun_is_counted_never_redelivered() {
        const PUBLISHES: u64 = 100_000;
        let (mut writer, ring) = SpmcRing::new(16);
        let mut reader = ring.register_reader().expect("register");
        let writer = std::thread::spawn(move || {
            for seq in 0..PUBLISHES {
                writer.publish(&tagged(seq));
            }
        });
        let mut buf = vec![0u8; SLOT_DATA_SIZE];
        let (mut position, mut delivered, mut skipped_total) = (0u64, 0u64, 0u64);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while position < PUBLISHES {
            assert!(
                std::time::Instant::now() < deadline,
                "reader stuck at {position}"
            );
            match reader.recv(&mut buf) {
                Ok(Some(_)) => {
                    assert_eq!(tag_of(&buf), position, "delivered under another position");
                    position += 1;
                    delivered += 1;
                }
                Ok(None) => {}
                Err(SpmcError::Lagged { skipped }) => {
                    position += skipped;
                    skipped_total += skipped;
                }
                Err(other) => panic!("unexpected {other:?}"),
            }
            for _ in 0..64 {
                std::hint::spin_loop();
            }
        }
        writer.join().expect("writer");
        assert_eq!(delivered + skipped_total, PUBLISHES);
        assert_eq!(reader.lost(), skipped_total);
        assert!(skipped_total > 0, "the writer never lapped the reader");
    }

    /// Two threads that share one reader behind a `Mutex` take turns, and
    /// between them receive each publish once or count it lost once. Shared
    /// any other way the reader does not compile (the doctests on
    /// `SpmcReader`).
    #[test]
    fn a_reader_shared_by_two_threads_delivers_each_publish_once() {
        const PUBLISHES: u64 = 50_000;
        let (mut writer, ring) = SpmcRing::new(1 << 10);
        let reader = std::sync::Mutex::new(ring.register_reader().expect("register"));
        let done = std::sync::atomic::AtomicBool::new(false);
        let (mut seen, skipped) = std::thread::scope(|s| {
            let consumers: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        let mut buf = vec![0u8; SLOT_DATA_SIZE];
                        let (mut seen, mut skipped) = (Vec::new(), 0u64);
                        loop {
                            // Loaded before the read: once it shows the
                            // writer done, a read that finds nothing new
                            // has had everything.
                            let finished = done.load(Ordering::Acquire);
                            let got = reader.lock().expect("the reader").recv(&mut buf);
                            match got {
                                Ok(Some(_)) => seen.push(tag_of(&buf)),
                                Ok(None) if finished => return (seen, skipped),
                                Ok(None) => std::hint::spin_loop(),
                                Err(SpmcError::Lagged { skipped: n }) => skipped += n,
                                Err(other) => panic!("unexpected {other:?}"),
                            }
                        }
                    })
                })
                .collect();
            for seq in 0..PUBLISHES {
                writer.publish(&tagged(seq));
            }
            done.store(true, Ordering::Release);
            consumers
                .into_iter()
                .map(|c| c.join().expect("a consumer"))
                .fold((Vec::new(), 0), |(mut seen, skipped), (more, n)| {
                    seen.extend(more);
                    (seen, skipped + n)
                })
        });
        let delivered = seen.len() as u64;
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len() as u64,
            delivered,
            "a publish was delivered twice"
        );
        assert_eq!(delivered + skipped, PUBLISHES);
        let reader = reader.into_inner().expect("the reader");
        assert_eq!(reader.lost(), skipped);
    }
}
