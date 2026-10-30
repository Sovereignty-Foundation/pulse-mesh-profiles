// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! An out-of-band shared-memory segment pool for payloads too large for a
//! 128-byte pulse slot, such as LIDAR scans and video frames: the payload goes
//! in a pool segment, and the pulse carries a 16-byte [`SegmentRef`] to it, so
//! only the reference crosses the ring.
//!
//! One process creates the pool ([`LargePayloadPool::create`]) and others map
//! it by name ([`LargePayloadPool::open`]). Any handle writes a payload into a
//! free segment and gets its reference back ([`LargePayloadPool::write`], or
//! [`LargePayloadPool::alloc`] and [`SegmentMut::finish`] to write in place).
//! The reader checks the reference and reads the payload
//! ([`LargePayloadPool::read`], or [`LargePayloadPool::payload`] in place),
//! then frees the segment ([`LargePayloadPool::free`]) for the next writer.
//!
//! ## Layout (version 1)
//!
//! ```text
//! header:  0..8   allocation hint (AtomicU64): where the next search starts
//!          8..12  magic "PMLP" (AtomicU32, stored last on create)
//!         12..16  layout version (u32 LE) = 1
//!         16..20  segment count (u32 LE)
//!         20..24  reserved, zero
//!         24..32  segment bytes (u64 LE)
//!         32..40  unlink token (AtomicU64): zero until the creator drops
//!                 its handle, as in the envelope store
//!         40..64  reserved, zero
//! states:  64..   one AtomicU64 per segment: its generation in the high
//!                 32 bits, and bit 0 set while the segment is held
//! data:           from the first 4096-byte boundary after the states, one
//!                 segment every `segment bytes` rounded up to 64
//! ```
//!
//! An allocation claims a free segment by compare-and-swap of its state word
//! from `generation << 32` to `generation << 32 | 1`, searching from the
//! header's hint; a free swaps it back to free with the next generation. A
//! [`SegmentRef`]'s id carries the generation beside the index, so a
//! reference to a segment freed since, or freed and allocated again, is
//! refused as [`LargePayloadError::InvalidSegment`] rather than read. A
//! process that dies holding a segment leaves it held until the pool is
//! created again. The POSIX calls are `core_region::shm`'s (ADR-0013), and an
//! open checks the object's size and header before it maps anything, as the
//! envelope store's does.

use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

use core_region::shm::{self, Mapping, Name, Opened};
use thiserror::Error;

/// One cache line, so the allocation hint shares no line with a state word.
const HEADER_SIZE: usize = 64;
/// Layout version written into the header and checked by `open`.
pub const LAYOUT_VERSION: u32 = 1;
const MAGIC: u32 = u32::from_le_bytes(*b"PMLP");

const H_HINT: usize = 0;
const H_MAGIC: usize = 8;
const H_VERSION: usize = 12;
const H_COUNT: usize = 16;
const H_SEGMENT_BYTES: usize = 24;
/// The header word of the unlink token.
const H_UNLINK_TOKEN: usize = 32;

/// The data area starts on a page boundary.
const DATA_ALIGN: usize = 4096;
/// Each segment starts on a cache line.
const SEGMENT_ALIGN: usize = 64;
/// A state word's held bit.
const HELD: u64 = 1;

/// 16-byte segment reference, packable into a Pulse's data region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct SegmentRef {
    /// The segment: its index in the pool in the low 32 bits, and in the
    /// high 32 the generation it had when the payload was written.
    pub id: u64,
    /// Logical payload length in bytes (≤ segment_bytes).
    pub len: u32,
    /// CRC-32C (Castagnoli) of the payload's `len` bytes, which
    /// [`LargePayloadPool::read`] and [`LargePayloadPool::payload`] check.
    pub crc32: u32,
}

impl SegmentRef {
    /// Wire size of a serialized [`SegmentRef`] in bytes (id + len + crc32).
    pub const SIZE: usize = 16;

    /// The 16-byte little-endian encoding: `id`, `len`, then `crc32`.
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..8].copy_from_slice(&self.id.to_le_bytes());
        out[8..12].copy_from_slice(&self.len.to_le_bytes());
        out[12..16].copy_from_slice(&self.crc32.to_le_bytes());
        out
    }

    /// Decode [`Self::to_bytes`]'s encoding; any 16 bytes decode.
    pub fn from_bytes(b: &[u8; 16]) -> Self {
        // A destructure, not `try_into().unwrap()`: the compiler checks every
        // length.
        let [id_0, id_1, id_2, id_3, id_4, id_5, id_6, id_7, len_0, len_1, len_2, len_3, crc_0, crc_1, crc_2, crc_3] =
            *b;
        Self {
            id: u64::from_le_bytes([id_0, id_1, id_2, id_3, id_4, id_5, id_6, id_7]),
            len: u32::from_le_bytes([len_0, len_1, len_2, len_3]),
            crc32: u32::from_le_bytes([crc_0, crc_1, crc_2, crc_3]),
        }
    }
}

/// Errors raised by [`LargePayloadPool`] operations.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum LargePayloadError {
    /// `shm_open(3)`, `fstat(2)`, `ftruncate(2)`, a header read or `mmap(2)`
    /// failed: `AlreadyExists` from a create whose name holds an object,
    /// `NotFound` from an open of a name that holds none.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The pool has no free segment.
    #[error("pool exhausted: {used}/{total} segments in flight")]
    Exhausted {
        /// Segments held when the search ended (allocated and not yet freed).
        used: u32,
        /// Total segment slots in the pool.
        total: u32,
    },
    /// A segment id not held in the pool: its index is out of range, or the
    /// segment was never allocated under this generation, or was freed since.
    #[error("invalid segment id {0:#x}")]
    InvalidSegment(u64),
    /// A payload, or a reference's length, exceeds the pool's segment size.
    #[error("payload too large: {got} bytes > segment size {limit}")]
    PayloadTooLarge {
        /// The payload or reference length.
        got: usize,
        /// Pool's per-segment byte limit.
        limit: usize,
    },
    /// The payload's CRC-32C is not the one its reference carries: the bytes
    /// or the reference changed after the write.
    #[error("segment {id:#x}: CRC-32C {actual:#010x}, reference says {expected:#010x}")]
    CrcMismatch {
        /// The reference's segment id.
        id: u64,
        /// The CRC the reference carries.
        expected: u32,
        /// The CRC of the bytes in the segment.
        actual: u32,
    },
    /// A configuration with no segments, empty segments, segments larger
    /// than a [`SegmentRef`]'s 32-bit length records, or more bytes than fit
    /// a `usize` and a file offset. Nothing was created.
    #[error("invalid pool configuration: {segment_count} segments of {segment_bytes} bytes")]
    InvalidConfig {
        /// Segments asked for.
        segment_count: u32,
        /// Bytes per segment asked for.
        segment_bytes: usize,
    },
    /// Linux could not reserve the new pool's pages: `posix_fallocate(3)`
    /// failed, with `ENOSPC` on a full `/dev/shm`. The object was unlinked
    /// and nothing was mapped. Never returned off Linux.
    #[error("could not reserve {need} bytes of shared memory: posix_fallocate failed with errno {errno}")]
    NoSpace {
        /// Bytes the pool needed.
        need: u64,
        /// The error number `posix_fallocate` returned.
        errno: i32,
    },
    /// The name contains a NUL byte.
    #[error("shared-memory name contains a NUL byte")]
    InvalidName,
    /// The object is smaller than a header, or than the pool its header
    /// describes.
    #[error("shared-memory object is {have} bytes but {need} are needed")]
    RegionTooSmall {
        /// Size of the object, from `fstat(2)`.
        have: u64,
        /// Bytes the pool needs.
        need: u64,
    },
    /// The header does not describe a pool this build reads.
    #[error("pool header mismatch: {field} is {have}, this build needs {need}")]
    LayoutMismatch {
        /// `magic`, `version`, `segment_count` or `segment_bytes`.
        field: &'static str,
        /// Value found in the header.
        have: u64,
        /// Value this build needs (the largest it accepts, for `segment_bytes`).
        need: u64,
    },
}

fn shm_error(e: shm::Error) -> LargePayloadError {
    match e {
        shm::Error::Open(e) | shm::Error::Truncate(e) | shm::Error::Map(e) => {
            LargePayloadError::Io(std::io::Error::from_raw_os_error(e))
        }
        shm::Error::NoSpace { need, errno } => LargePayloadError::NoSpace { need, errno },
        shm::Error::InvalidName => LargePayloadError::InvalidName,
        shm::Error::TooSmall { have, need } => LargePayloadError::RegionTooSmall { have, need },
    }
}

/// Configuration for a [`LargePayloadPool`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PoolConfig {
    /// SHM identifier (POSIX shm_open name; max 31 chars on macOS).
    pub name: String,
    /// Number of segments in the pool: the most payloads in flight at once.
    pub segment_count: u32,
    /// Per-segment byte size, at most `u32::MAX`. Common: 4 MiB for LIDAR,
    /// 8 MiB for 4K frames.
    pub segment_bytes: usize,
}

impl Default for PoolConfig {
    /// Eight 4 MiB segments, 32 MiB in all, which fits Docker's default
    /// 64 MB `/dev/shm`; on Linux a pool's pages are reserved when it is
    /// created.
    fn default() -> Self {
        Self {
            name: format!("/pms_lp_{}", std::process::id()),
            segment_count: 8,
            segment_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Where a pool's parts lie in its object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    count: usize,
    segment_bytes: usize,
    stride: usize,
    data_offset: usize,
    total: usize,
}

impl Layout {
    /// The layout of `segment_count` segments of `segment_bytes`, or `None`
    /// when either is zero, a segment is longer than a 32-bit length, or the
    /// pool does not fit a `usize` and a file offset.
    fn new(segment_count: u32, segment_bytes: usize) -> Option<Self> {
        if segment_count == 0 || segment_bytes == 0 || u32::try_from(segment_bytes).is_err() {
            return None;
        }
        let count = segment_count as usize;
        let stride = segment_bytes.checked_next_multiple_of(SEGMENT_ALIGN)?;
        let states_end = HEADER_SIZE.checked_add(count.checked_mul(8)?)?;
        let data_offset = states_end.checked_next_multiple_of(DATA_ALIGN)?;
        let total = data_offset.checked_add(count.checked_mul(stride)?)?;
        libc::off_t::try_from(total).ok()?;
        Some(Self {
            count,
            segment_bytes,
            stride,
            data_offset,
            total,
        })
    }
}

/// The segment pool, in a shared-memory object any process can map by name.
/// The handle that created it unlinks the name on drop, while the name still
/// names this pool; other handles only unmap.
pub struct LargePayloadPool {
    /// Its descriptor is closed once mapped: the mapping keeps the object
    /// alive.
    map: Mapping,
    cfg: PoolConfig,
    layout: Layout,
    owner: bool,
}

// SAFETY: the handle owns a mapping that stays valid until it drops and
// holds nothing tied to the thread that made it. Through `&self` the header
// hint and every state word change only as atomics. A segment's bytes are
// written only through the `SegmentMut` whose compare-and-swap set the
// segment's held bit, which no other claim can set until a free clears it.
// A reader checks the state word before its copy and `read` again after
// it; a copy that overlaps a write is possible only for a reference whose
// segment was freed and allocated again, which that check refuses, as a
// SeqLock reader refuses a torn copy.
unsafe impl Send for LargePayloadPool {}
// SAFETY: see the `Send` impl above; the same protocol covers `Sync`.
unsafe impl Sync for LargePayloadPool {}

impl LargePayloadPool {
    /// Create a pool for `cfg` under a name that must be free; the handle is
    /// its creator. On Linux the pages are reserved before they are mapped,
    /// so a full `/dev/shm` is [`LargePayloadError::NoSpace`] here rather
    /// than SIGBUS at the first touch of a page, and transparent huge pages
    /// are asked for. A failure after the object exists unlinks it.
    ///
    /// # Errors
    ///
    /// [`LargePayloadError::InvalidConfig`] before anything is created;
    /// [`LargePayloadError::Io`] with `AlreadyExists` when the name holds an
    /// object; on Linux [`LargePayloadError::NoSpace`]; and the other libc
    /// and mapping failures.
    pub fn create(cfg: PoolConfig) -> Result<Self, LargePayloadError> {
        let layout = Layout::new(cfg.segment_count, cfg.segment_bytes).ok_or(
            LargePayloadError::InvalidConfig {
                segment_count: cfg.segment_count,
                segment_bytes: cfg.segment_bytes,
            },
        )?;
        let name = Name::new(&cfg.name).map_err(shm_error)?;
        let mut map = Mapping::create_exclusive(name, layout.total).map_err(shm_error)?;
        map.close_fd();
        map.advise_hugepages();
        let pool = Self {
            map,
            cfg,
            layout,
            owner: true,
        };
        // Not zero-filled: the kernel's zero pages are already a pool with
        // every segment free at generation zero.
        pool.write_header(H_VERSION, &LAYOUT_VERSION.to_le_bytes());
        pool.write_header(H_COUNT, &pool.cfg.segment_count.to_le_bytes());
        pool.write_header(
            H_SEGMENT_BYTES,
            &(layout.segment_bytes as u64).to_le_bytes(),
        );
        // The magic last, with release ordering: an opener that sees it and
        // fences with acquire sees the fields above.
        pool.header_u32(H_MAGIC).store(MAGIC, Ordering::Release);
        tracing::info!(
            name = %pool.cfg.name,
            segment_count = pool.cfg.segment_count,
            segment_bytes = layout.segment_bytes,
            total_bytes = layout.total,
            "LargePayloadPool created"
        );
        Ok(pool)
    }

    /// Map the pool another handle created under `name`, taking its segment
    /// count and size from its header. The size and then the header are
    /// checked before anything is mapped. An object at size zero, or with a
    /// zero magic, may be a creator that has not finished, so it is retried
    /// briefly before it is refused.
    ///
    /// # Errors
    ///
    /// [`LargePayloadError::Io`] with `NotFound` when no object exists under
    /// the name; [`LargePayloadError::RegionTooSmall`] when the object is
    /// smaller than a header or than the pool its header describes;
    /// [`LargePayloadError::LayoutMismatch`] when the header is not this
    /// build's layout; [`LargePayloadError::InvalidName`] for a name with a
    /// NUL byte; and the other libc and mapping failures.
    pub fn open(name: &str) -> Result<Self, LargePayloadError> {
        let name = Name::new(name).map_err(shm_error)?;
        shm::wait_out_creator(
            || Self::open_once(&name),
            |opened| {
                matches!(
                    opened,
                    Err(LargePayloadError::RegionTooSmall { have: 0, .. })
                        | Err(LargePayloadError::LayoutMismatch {
                            field: "magic",
                            have: 0,
                            ..
                        })
                )
            },
        )
    }

    /// One attempt of [`Self::open`].
    fn open_once(name: &Name) -> Result<Self, LargePayloadError> {
        let opened = Opened::open(name.clone()).map_err(shm_error)?;
        opened.at_least(HEADER_SIZE).map_err(shm_error)?;
        let layout = PoolHeader::read(&opened)?.check(opened.len())?;
        opened.at_least(layout.total).map_err(shm_error)?;
        // A published magic means the pages were reserved (Linux), so
        // touching them cannot fault for want of room.
        let mut map = opened.map(layout.total).map_err(shm_error)?;
        map.close_fd();
        // Checked again through the mapping: its load of the magic, fenced
        // with acquire, orders this handle's reads after the formatting.
        // SAFETY: the mapping's start is page aligned, spans the header and
        // lives until `map` drops, after this call.
        let again = unsafe { PoolHeader::from_mapping(map.ptr()) }.check(map.len() as u64)?;
        if again != layout {
            return Err(LargePayloadError::LayoutMismatch {
                field: "segment_count",
                have: again.count as u64,
                need: layout.count as u64,
            });
        }
        Ok(Self {
            map,
            cfg: PoolConfig {
                name: name.as_str().to_owned(),
                segment_count: layout.count as u32,
                segment_bytes: layout.segment_bytes,
            },
            layout,
            owner: false,
        })
    }

    /// Claim a free segment for writing. The segment is the caller's until
    /// [`SegmentMut::finish`] hands its reference on, or until the
    /// [`SegmentMut`] drops unfinished, which frees it.
    ///
    /// # Errors
    ///
    /// [`LargePayloadError::Exhausted`] when every segment is held.
    pub fn alloc(&self) -> Result<SegmentMut<'_>, LargePayloadError> {
        let count = self.layout.count;
        let start =
            (self.header_u64(H_HINT).fetch_add(1, Ordering::Relaxed) % count as u64) as usize;
        for i in 0..count {
            let index = (start + i) % count;
            let state = self.state(index);
            let word = state.load(Ordering::Relaxed);
            // Acquire: the free that cleared the bit released the previous
            // holder's reads, which so end before this holder's writes.
            if word & HELD == 0
                && state
                    .compare_exchange(word, word | HELD, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                return Ok(SegmentMut {
                    pool: self,
                    index,
                    generation: (word >> 32) as u32,
                    finished: false,
                });
            }
        }
        Err(LargePayloadError::Exhausted {
            used: self.in_use(),
            total: self.cfg.segment_count,
        })
    }

    /// Write `payload` into a free segment and return its reference.
    ///
    /// # Errors
    ///
    /// [`LargePayloadError::PayloadTooLarge`] when `payload` is longer than a
    /// segment, before any segment is claimed;
    /// [`LargePayloadError::Exhausted`] when every segment is held.
    pub fn write(&self, payload: &[u8]) -> Result<SegmentRef, LargePayloadError> {
        let limit = self.layout.segment_bytes;
        if payload.len() > limit {
            return Err(LargePayloadError::PayloadTooLarge {
                got: payload.len(),
                limit,
            });
        }
        let mut segment = self.alloc()?;
        segment.as_mut_slice()[..payload.len()].copy_from_slice(payload);
        segment.finish(payload.len())
    }

    /// Copy the payload `seg` refers to into `out`, replacing what `out`
    /// held, once its segment is held under the reference's generation and
    /// its CRC matches. The segment stays held; the reader frees it when it
    /// is done.
    ///
    /// # Errors
    ///
    /// [`LargePayloadError::InvalidSegment`] when the segment is not held
    /// under the reference's generation, before or after the copy;
    /// [`LargePayloadError::PayloadTooLarge`] when the reference's length
    /// exceeds a segment; [`LargePayloadError::CrcMismatch`] when the bytes
    /// do not match the reference's CRC.
    pub fn read(&self, seg: &SegmentRef, out: &mut Vec<u8>) -> Result<(), LargePayloadError> {
        let (index, held) = self.check_ref(seg)?;
        let len = seg.len as usize;
        out.clear();
        out.resize(len, 0);
        // SAFETY: the segment starts 8-byte aligned (page-aligned mapping,
        // page-aligned data area, 64-byte stride) and `len` rounded up to 8
        // is within its 64-byte-rounded stride, inside the mapping, which
        // lives as long as `self`. Bytes of a held segment are written only
        // before its reference is handed on; see the `Send` impl.
        unsafe { shm::load_words(self.segment_ptr(index), out) };
        fence(Ordering::Acquire);
        if self.state(index).load(Ordering::Relaxed) != held {
            return Err(LargePayloadError::InvalidSegment(seg.id));
        }
        check_crc(seg, out)
    }

    /// The payload `seg` refers to, in place, once its segment is held under
    /// the reference's generation and its CRC matches: no copy, which is
    /// what the pool is for with multi-megabyte payloads.
    ///
    /// # Safety
    ///
    /// No handle, in this process or another, may free the segment while
    /// the returned slice is alive: a free lets a writer claim the segment
    /// and write over the bytes the slice borrows. The reader that frees the
    /// segment does so after it is done with the slice.
    ///
    /// # Errors
    ///
    /// Those of [`Self::read`].
    pub unsafe fn payload(&self, seg: &SegmentRef) -> Result<&[u8], LargePayloadError> {
        let (index, _) = self.check_ref(seg)?;
        // SAFETY: the segment lies inside the mapping, which lives as long
        // as `self` and so as the returned borrow; `check_ref` bounded the
        // length by the segment size. The caller keeps the segment held, so
        // no writer claims it while the slice lives.
        let bytes =
            unsafe { std::slice::from_raw_parts(self.segment_ptr(index), seg.len as usize) };
        check_crc(seg, bytes)?;
        Ok(bytes)
    }

    /// Return the segment `id` names to the pool, for the next allocation.
    ///
    /// # Errors
    ///
    /// [`LargePayloadError::InvalidSegment`] when `id` names no segment held
    /// under its generation: out of range, never allocated, or freed already.
    pub fn free(&self, id: u64) -> Result<(), LargePayloadError> {
        let (index, generation) = self.locate(id)?;
        if self.release(index, generation) {
            Ok(())
        } else {
            Err(LargePayloadError::InvalidSegment(id))
        }
    }

    /// Segments held now, by every handle on the pool.
    pub fn in_use(&self) -> u32 {
        (0..self.layout.count)
            .filter(|&i| self.state(i).load(Ordering::Relaxed) & HELD != 0)
            .count() as u32
    }

    /// The configuration this pool was created with, or, for an opened pool,
    /// its name and the geometry its header records.
    pub fn config(&self) -> &PoolConfig {
        &self.cfg
    }

    /// Clear the held bit of segment `index`, held under `generation`, and
    /// move it to the next generation; false when it was not so held.
    fn release(&self, index: usize, generation: u32) -> bool {
        let held = u64::from(generation) << 32 | HELD;
        let free = u64::from(generation.wrapping_add(1)) << 32;
        // Release: this holder's reads end before the next holder's writes.
        self.state(index)
            .compare_exchange(held, free, Ordering::Release, Ordering::Relaxed)
            .is_ok()
    }

    /// The index and generation `id` names, once the index is in range.
    fn locate(&self, id: u64) -> Result<(usize, u32), LargePayloadError> {
        let index = (id & 0xFFFF_FFFF) as usize;
        if index >= self.layout.count {
            return Err(LargePayloadError::InvalidSegment(id));
        }
        Ok((index, (id >> 32) as u32))
    }

    /// The index of `seg`'s segment and the state word it holds, once that
    /// segment is held under `seg`'s generation and `seg`'s length fits it.
    fn check_ref(&self, seg: &SegmentRef) -> Result<(usize, u64), LargePayloadError> {
        let (index, generation) = self.locate(seg.id)?;
        let held = u64::from(generation) << 32 | HELD;
        if self.state(index).load(Ordering::Acquire) != held {
            return Err(LargePayloadError::InvalidSegment(seg.id));
        }
        let limit = self.layout.segment_bytes;
        if seg.len as usize > limit {
            return Err(LargePayloadError::PayloadTooLarge {
                got: seg.len as usize,
                limit,
            });
        }
        Ok((index, held))
    }

    fn header_u64(&self, offset: usize) -> &AtomicU64 {
        // SAFETY: `offset` is an 8-byte-aligned word of the header, inside
        // the page-aligned mapping, which lives as long as `self`; the word
        // is only ever accessed atomically.
        unsafe { &*(self.map.ptr().add(offset) as *const AtomicU64) }
    }

    fn header_u32(&self, offset: usize) -> &AtomicU32 {
        // SAFETY: as `header_u64`, for a 4-byte-aligned word.
        unsafe { &*(self.map.ptr().add(offset) as *const AtomicU32) }
    }

    /// Write a header field during creation, before the magic is published.
    fn write_header(&self, offset: usize, bytes: &[u8]) {
        debug_assert!(offset + bytes.len() <= HEADER_SIZE);
        // SAFETY: the bytes lie inside the header, inside the mapping; only
        // the creator writes them, before it publishes the magic, and no
        // opener reads them until it has seen the magic.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.map.ptr().add(offset), bytes.len())
        };
    }

    fn state(&self, index: usize) -> &AtomicU64 {
        debug_assert!(index < self.layout.count);
        // SAFETY: the state words follow the header, 8-byte aligned, one per
        // segment, inside the mapping, which lives as long as `self`; each is
        // only ever accessed atomically.
        unsafe { &*(self.map.ptr().add(HEADER_SIZE + 8 * index) as *const AtomicU64) }
    }

    fn segment_ptr(&self, index: usize) -> *mut u8 {
        debug_assert!(index < self.layout.count);
        // SAFETY: `data_offset + index * stride + stride <= total`, the
        // mapping's length, for every index below the count.
        unsafe {
            self.map
                .ptr()
                .add(self.layout.data_offset + index * self.layout.stride)
        }
    }
}

impl Drop for LargePayloadPool {
    fn drop(&mut self) {
        // Only while the name still names this pool
        // (`shm::Mapping::unlink_if_token`).
        if self.owner {
            self.map.unlink_if_token(H_UNLINK_TOKEN);
        }
    }
}

/// A segment claimed by [`LargePayloadPool::alloc`], writable in place until
/// [`Self::finish`] returns its reference. Dropped unfinished, it frees the
/// segment.
pub struct SegmentMut<'a> {
    pool: &'a LargePayloadPool,
    index: usize,
    generation: u32,
    finished: bool,
}

impl SegmentMut<'_> {
    /// The id the segment's reference will carry.
    pub fn id(&self) -> u64 {
        segment_id(self.generation, self.index)
    }

    /// The whole segment, `segment_bytes` long, to write the payload into.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the segment lies inside the pool's mapping, which outlives
        // this borrow of the pool. This handle's compare-and-swap set the
        // segment's held bit, so no other claim writes it, and no reader has
        // its reference before `finish` returns it.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.pool.segment_ptr(self.index),
                self.pool.layout.segment_bytes,
            )
        }
    }

    /// End the write: the payload is the segment's first `len` bytes. Returns
    /// the reference a reader needs, with the payload's CRC-32C; the segment
    /// stays held until a reader frees it.
    ///
    /// # Errors
    ///
    /// [`LargePayloadError::PayloadTooLarge`] when `len` exceeds the segment;
    /// the segment is then freed.
    pub fn finish(mut self, len: usize) -> Result<SegmentRef, LargePayloadError> {
        let limit = self.pool.layout.segment_bytes;
        if len > limit {
            return Err(LargePayloadError::PayloadTooLarge { got: len, limit });
        }
        let crc32 = crc32c::crc32c(&self.as_mut_slice()[..len]);
        self.finished = true;
        Ok(SegmentRef {
            id: self.id(),
            // `len` fits: a segment is at most `u32::MAX` bytes.
            len: len as u32,
            crc32,
        })
    }
}

impl Drop for SegmentMut<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.pool.release(self.index, self.generation);
        }
    }
}

/// A segment id: `generation` in the high 32 bits, `index` in the low.
fn segment_id(generation: u32, index: usize) -> u64 {
    u64::from(generation) << 32 | index as u64
}

fn check_crc(seg: &SegmentRef, bytes: &[u8]) -> Result<(), LargePayloadError> {
    let actual = crc32c::crc32c(bytes);
    if actual == seg.crc32 {
        Ok(())
    } else {
        Err(LargePayloadError::CrcMismatch {
            id: seg.id,
            expected: seg.crc32,
            actual,
        })
    }
}

/// The header fields an open checks, as one read of the header found them.
#[derive(Clone, Copy)]
struct PoolHeader {
    magic: u32,
    version: u32,
    count: u32,
    segment_bytes: u64,
}

impl PoolHeader {
    /// The header at the start of a pool mapping. The magic is loaded
    /// relaxed, then an acquire fence, as the envelope store's header is
    /// read: a relaxed load is the only atomic load std allows through the
    /// read-only mapping `read` uses off Linux. The other fields are written
    /// once, before the magic, so they are read only after a magic that
    /// checks out, and are left zero otherwise.
    ///
    /// # Safety
    ///
    /// `base` must be the page-aligned start of a live mapping of at least
    /// [`HEADER_SIZE`] bytes that stays mapped during the call.
    unsafe fn from_mapping(base: *const u8) -> Self {
        // SAFETY: bytes 8..12 lie inside the mapping the caller vouched
        // for, 4-byte aligned, and the magic is only ever accessed
        // atomically.
        let magic = unsafe { &*(base.add(H_MAGIC) as *const AtomicU32) }.load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        if magic != MAGIC {
            return Self {
                magic,
                version: 0,
                count: 0,
                segment_bytes: 0,
            };
        }
        let field = |offset: usize, len: usize| {
            let mut bytes = [0u8; 8];
            // SAFETY: `offset..offset + len` lies inside the header, inside
            // the mapping the caller vouched for. The creator wrote these
            // fields before it published the magic with release ordering and
            // never writes them again, and the load above saw the magic
            // before the acquire fence, so nothing writes them during this
            // copy.
            unsafe { std::ptr::copy_nonoverlapping(base.add(offset), bytes.as_mut_ptr(), len) };
            u64::from_le_bytes(bytes)
        };
        Self {
            magic,
            version: field(H_VERSION, 4) as u32,
            count: field(H_COUNT, 4) as u32,
            segment_bytes: field(H_SEGMENT_BYTES, 8),
        }
    }

    /// One read of the opened object's header, which is at least a header
    /// long, with `pread(2)`, which allocates no page. `pread` copies in no
    /// set order, so the header is read again after an acquire fence once a
    /// copy shows the magic; the check through the mapping decides.
    #[cfg(target_os = "linux")]
    fn read(opened: &Opened) -> Result<Self, LargePayloadError> {
        let mut bytes = [0u8; HEADER_SIZE];
        shm::pread_exact(opened.fd(), 0, &mut bytes).map_err(shm_error)?;
        if Self::from_bytes(&bytes).magic == 0 {
            return Ok(Self::from_bytes(&bytes));
        }
        fence(Ordering::Acquire);
        shm::pread_exact(opened.fd(), 0, &mut bytes).map_err(shm_error)?;
        Ok(Self::from_bytes(&bytes))
    }

    /// The fields of a header's little-endian bytes.
    #[cfg(target_os = "linux")]
    fn from_bytes(bytes: &[u8; HEADER_SIZE]) -> Self {
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let mut wide = [0u8; 8];
        wide.copy_from_slice(&bytes[H_SEGMENT_BYTES..H_SEGMENT_BYTES + 8]);
        Self {
            magic: word(H_MAGIC),
            version: word(H_VERSION),
            count: word(H_COUNT),
            segment_bytes: u64::from_le_bytes(wide),
        }
    }

    /// One read of the opened object's header through a read-only mapping
    /// of it ([`shm::with_prefix_mapped`] says why that is safe off Linux).
    #[cfg(not(target_os = "linux"))]
    fn read(opened: &Opened) -> Result<Self, LargePayloadError> {
        shm::with_prefix_mapped(opened.fd(), HEADER_SIZE, |ptr| {
            // SAFETY: `ptr` is the page-aligned start of a live mapping of
            // HEADER_SIZE bytes until the closure returns; the view only
            // loads, its atomic load relaxed.
            unsafe { Self::from_mapping(ptr) }
        })
        .map_err(shm_error)
    }

    /// Check the magic, the layout version and the geometry against an
    /// object of `have` bytes; returns the layout the header describes.
    fn check(&self, have: u64) -> Result<Layout, LargePayloadError> {
        let mismatch = |field, have: u64, need: u64| {
            if have == need {
                Ok(())
            } else {
                Err(LargePayloadError::LayoutMismatch { field, have, need })
            }
        };
        mismatch("magic", u64::from(self.magic), u64::from(MAGIC))?;
        mismatch(
            "version",
            u64::from(self.version),
            u64::from(LAYOUT_VERSION),
        )?;
        if self.count == 0 {
            mismatch("segment_count", 0, 1)?;
        }
        if self.segment_bytes == 0 || self.segment_bytes > u64::from(u32::MAX) {
            return Err(LargePayloadError::LayoutMismatch {
                field: "segment_bytes",
                have: self.segment_bytes,
                need: u64::from(u32::MAX),
            });
        }
        // A pool too large for a `usize` fits no object either.
        Layout::new(self.count, self.segment_bytes as usize).ok_or(
            LargePayloadError::RegionTooSmall {
                have,
                need: u64::MAX,
            },
        )
    }
}

/// Write `seg` into a pulse's bytes 8..24, after the 8-byte SeqLock word.
pub fn pack_segment_ref(pulse_buf: &mut [u8; 128], seg: &SegmentRef) {
    pulse_buf[8..24].copy_from_slice(&seg.to_bytes());
}

/// Read the [`SegmentRef`] that [`pack_segment_ref`] wrote.
pub fn unpack_segment_ref(pulse_buf: &[u8; 128]) -> SegmentRef {
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&pulse_buf[8..24]);
    SegmentRef::from_bytes(&buf)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A pool name unique to this process and test, short enough for macOS.
    fn unique(tag: &str) -> String {
        format!("lp-{tag}-{:x}", std::process::id())
    }

    fn pool(tag: &str, segment_count: u32, segment_bytes: usize) -> LargePayloadPool {
        LargePayloadPool::create(PoolConfig {
            name: unique(tag),
            segment_count,
            segment_bytes,
        })
        .unwrap()
    }

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn segment_ref_round_trip_pack() {
        let seg = SegmentRef {
            id: 0xDEAD_BEEF_CAFE,
            len: 4 * 1024 * 1024,
            crc32: 0xABCD_1234,
        };
        let bytes = seg.to_bytes();
        assert_eq!(bytes.len(), 16);
        let decoded = SegmentRef::from_bytes(&bytes);
        assert_eq!(decoded, seg);
    }

    #[test]
    fn segment_ref_round_trip_in_pulse_buf() {
        let mut pulse = [0u8; 128];
        let seg = SegmentRef {
            id: 42,
            len: 1_048_576,
            crc32: 0x1234_5678,
        };
        pack_segment_ref(&mut pulse, &seg);
        let decoded = unpack_segment_ref(&pulse);
        assert_eq!(decoded, seg);
        // Bytes 0..8 (SeqLock-reserved) and 24..128 (unused) untouched.
        assert!(pulse[0..8].iter().all(|&b| b == 0));
        assert!(pulse[24..].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_payload_written_is_read_back_intact_and_in_place() {
        let pool = pool("rw", 4, 64 * 1024);
        let payload = pattern(50_000, 7);
        let seg = pool.write(&payload).unwrap();
        assert_eq!(seg.len, 50_000);
        assert_eq!(seg.crc32, crc32c::crc32c(&payload));
        let mut out = Vec::new();
        pool.read(&seg, &mut out).unwrap();
        assert_eq!(out, payload);
        // SAFETY: nothing frees the segment while `bytes` lives.
        let bytes = unsafe { pool.payload(&seg) }.unwrap();
        assert_eq!(bytes, &payload[..]);
        pool.free(seg.id).unwrap();
        assert_eq!(pool.in_use(), 0);
    }

    #[test]
    fn a_freed_segment_is_reused_and_its_old_reference_refused() {
        let pool = pool("reuse", 1, 4096);
        let first = pool.write(b"first").unwrap();
        pool.free(first.id).unwrap();
        let second = pool.write(b"second payload").unwrap();
        assert_eq!(
            first.id & 0xFFFF_FFFF,
            second.id & 0xFFFF_FFFF,
            "one segment, reused"
        );
        assert_ne!(first.id, second.id, "a new generation");
        let mut out = Vec::new();
        assert!(matches!(
            pool.read(&first, &mut out),
            Err(LargePayloadError::InvalidSegment(id)) if id == first.id
        ));
        assert!(matches!(
            pool.free(first.id),
            Err(LargePayloadError::InvalidSegment(_))
        ));
        pool.read(&second, &mut out).unwrap();
        assert_eq!(out, b"second payload");
        pool.free(second.id).unwrap();
        assert!(matches!(
            pool.free(second.id),
            Err(LargePayloadError::InvalidSegment(_))
        ));
    }

    #[test]
    fn the_pool_exhausts_and_recovers_when_a_segment_is_freed() {
        let pool = pool("exhaust", 2, 1024);
        let a = pool.write(b"a").unwrap();
        let b = pool.write(b"b").unwrap();
        assert_ne!(a.id & 0xFFFF_FFFF, b.id & 0xFFFF_FFFF);
        assert!(matches!(
            pool.alloc(),
            Err(LargePayloadError::Exhausted { used: 2, total: 2 })
        ));
        pool.free(a.id).unwrap();
        let c = pool.write(b"c").unwrap();
        assert_eq!(c.id & 0xFFFF_FFFF, a.id & 0xFFFF_FFFF);
        assert_eq!(pool.in_use(), 2);
    }

    #[test]
    fn an_unfinished_segment_is_freed_when_dropped() {
        let pool = pool("drop", 1, 1024);
        {
            let mut seg = pool.alloc().unwrap();
            seg.as_mut_slice()[0] = 1;
            assert_eq!(pool.in_use(), 1);
        }
        assert_eq!(pool.in_use(), 0);
        assert!(matches!(
            pool.alloc().unwrap().finish(1025),
            Err(LargePayloadError::PayloadTooLarge {
                got: 1025,
                limit: 1024
            })
        ));
        assert_eq!(pool.in_use(), 0, "a failed finish frees the segment");
    }

    #[test]
    fn a_changed_payload_or_reference_fails_the_crc() {
        let pool = pool("crc", 1, 4096);
        let seg = pool.write(&pattern(4000, 3)).unwrap();
        let mut out = Vec::new();
        for tampered in [
            SegmentRef {
                crc32: seg.crc32 ^ 1,
                ..seg
            },
            SegmentRef {
                len: seg.len - 1,
                ..seg
            },
        ] {
            assert!(matches!(
                pool.read(&tampered, &mut out),
                Err(LargePayloadError::CrcMismatch { id, expected, .. })
                    if id == seg.id && expected == tampered.crc32
            ));
            // SAFETY: nothing frees the segment during the call.
            let in_place = unsafe { pool.payload(&tampered) };
            assert!(matches!(
                in_place,
                Err(LargePayloadError::CrcMismatch { .. })
            ));
        }
        pool.read(&seg, &mut out).unwrap();
    }

    #[test]
    fn oversized_payloads_and_references_and_foreign_ids_are_refused() {
        let pool = pool("limits", 2, 1000);
        assert!(matches!(
            pool.write(&[0u8; 1001]),
            Err(LargePayloadError::PayloadTooLarge {
                got: 1001,
                limit: 1000
            })
        ));
        assert_eq!(
            pool.in_use(),
            0,
            "nothing is claimed for a payload too large"
        );
        let seg = pool.write(&[9u8; 1000]).unwrap();
        let mut out = Vec::new();
        assert!(matches!(
            pool.read(&SegmentRef { len: 1001, ..seg }, &mut out),
            Err(LargePayloadError::PayloadTooLarge {
                got: 1001,
                limit: 1000
            })
        ));
        assert!(matches!(
            pool.read(&SegmentRef { id: 2, ..seg }, &mut out),
            Err(LargePayloadError::InvalidSegment(2))
        ));
        assert!(matches!(
            pool.free(7),
            Err(LargePayloadError::InvalidSegment(7))
        ));
    }

    #[test]
    fn invalid_configurations_create_nothing() {
        for (segment_count, segment_bytes) in [(0, 1024), (4, 0), (1, u32::MAX as usize + 1)] {
            let name = unique("badcfg");
            assert!(matches!(
                LargePayloadPool::create(PoolConfig {
                    name: name.clone(),
                    segment_count,
                    segment_bytes,
                }),
                Err(LargePayloadError::InvalidConfig { .. })
            ));
            assert!(matches!(
                LargePayloadPool::open(&name),
                Err(LargePayloadError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound
            ));
        }
    }

    #[test]
    fn a_second_handle_shares_the_segments_and_the_creator_unlinks_the_name() {
        let creator = pool("share", 2, 8192);
        let name = creator.config().name.clone();
        assert!(matches!(
            LargePayloadPool::create(PoolConfig {
                name: name.clone(),
                segment_count: 2,
                segment_bytes: 8192,
            }),
            Err(LargePayloadError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists
        ));
        let opener = LargePayloadPool::open(&name).unwrap();
        assert_eq!(opener.config().segment_count, 2);
        assert_eq!(opener.config().segment_bytes, 8192);
        let seg = creator.write(&pattern(8192, 11)).unwrap();
        let mut out = Vec::new();
        opener.read(&seg, &mut out).unwrap();
        assert_eq!(out, pattern(8192, 11));
        opener.free(seg.id).unwrap();
        assert_eq!(creator.in_use(), 0);
        drop(opener);
        drop(creator);
        assert!(matches!(
            LargePayloadPool::open(&name),
            Err(LargePayloadError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound
        ));
    }

    #[test]
    fn open_refuses_an_object_that_is_not_a_pool() {
        let name = unique("notpool");
        let map = Mapping::create_exclusive(Name::new(&name).unwrap(), 2 * DATA_ALIGN).unwrap();
        let put = |offset: usize, bytes: &[u8]| {
            // SAFETY: the header bytes lie inside the new mapping, and
            // nothing else maps it while the test writes them.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), map.ptr().add(offset), bytes.len())
            };
        };
        put(H_MAGIC, b"PMES");
        assert!(matches!(
            LargePayloadPool::open(&name),
            Err(LargePayloadError::LayoutMismatch { field: "magic", .. })
        ));
        put(H_MAGIC, &MAGIC.to_le_bytes());
        put(H_VERSION, &9u32.to_le_bytes());
        assert!(matches!(
            LargePayloadPool::open(&name),
            Err(LargePayloadError::LayoutMismatch {
                field: "version",
                have: 9,
                need: 1
            })
        ));
        put(H_VERSION, &LAYOUT_VERSION.to_le_bytes());
        put(H_COUNT, &4u32.to_le_bytes());
        put(H_SEGMENT_BYTES, &4096u64.to_le_bytes());
        // Four 4096-byte segments after a one-page header and state area;
        // the object holds two pages (macOS rounds it up to its own page).
        let need = (DATA_ALIGN + 4 * 4096) as u64;
        assert!(matches!(
            LargePayloadPool::open(&name),
            Err(LargePayloadError::RegionTooSmall { have, need: n }) if n == need && have < need
        ));
        shm::unlink(map.name()).unwrap();
    }

    /// Writers on several threads, each checking that the segment it holds
    /// keeps its own bytes until it frees it: no segment is ever claimed by
    /// two holders.
    #[test]
    fn concurrent_holders_never_share_a_segment() {
        let pool = pool("threads", 4, 4096);
        std::thread::scope(|s| {
            for t in 0..8u8 {
                let pool = &pool;
                s.spawn(move || {
                    let mut out = Vec::new();
                    let mut written = 0;
                    while written < 2000 {
                        let payload = pattern(4096, t);
                        let seg = match pool.write(&payload) {
                            Ok(seg) => seg,
                            Err(LargePayloadError::Exhausted { .. }) => {
                                std::thread::yield_now();
                                continue;
                            }
                            Err(e) => panic!("{e}"),
                        };
                        pool.read(&seg, &mut out).unwrap();
                        assert_eq!(out, payload);
                        pool.free(seg.id).unwrap();
                        written += 1;
                    }
                });
            }
        });
        assert_eq!(pool.in_use(), 0);
    }
}
