// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! The four AF_XDP rings, shared with the kernel: the fill ring hands it
//! empty UMEM frames to receive into, the RX ring hands back the frames it
//! filled, the TX ring hands it frames to send, and the completion ring
//! hands back the frames it sent. Each is a power-of-two array of entries
//! with a producer and a consumer index, free-running `u32`s: the producer
//! writes entries, then publishes its index with release ordering; the
//! consumer reads the producer's index with acquire ordering, reads the
//! entries, then publishes its own index with release ordering, as the
//! kernel's side does (`smp_load_acquire`, `smp_store_release`).
#![allow(unsafe_code)]

use std::io;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::sys::Mapping;

/// The parts of a ring's mapping, from `XDP_MMAP_OFFSETS`.
pub(crate) struct Offsets {
    pub(crate) producer: u64,
    pub(crate) consumer: u64,
    pub(crate) desc: u64,
    pub(crate) flags: u64,
}

impl From<libc::xdp_ring_offset> for Offsets {
    fn from(o: libc::xdp_ring_offset) -> Self {
        Self {
            producer: o.producer,
            consumer: o.consumer,
            desc: o.desc,
            flags: o.flags,
        }
    }
}

/// One ring of `size` entries of `T`: `u64` frame addresses for the fill
/// and completion rings, `xdp_desc` for RX and TX.
pub(crate) struct Ring<T> {
    map: Mapping,
    producer: usize,
    consumer: usize,
    flags: usize,
    desc: usize,
    mask: u32,
    size: u32,
    /// This side's index, published or not.
    local: u32,
    /// The other side's index as last read.
    cached: u32,
    _entry: std::marker::PhantomData<T>,
}

impl<T: Copy> Ring<T> {
    /// Map the ring of `size` entries at `pgoff` of socket `fd`. As a
    /// producer, this side starts with every entry free; as a consumer,
    /// with none to read.
    pub(crate) fn map(
        fd: &OwnedFd,
        pgoff: u64,
        size: u32,
        offsets: Offsets,
        producer: bool,
    ) -> io::Result<Self> {
        let len = offsets.desc as usize + size as usize * std::mem::size_of::<T>();
        let map = Mapping::ring(fd, pgoff, len)?;
        let mut ring = Self {
            map,
            producer: offsets.producer as usize,
            consumer: offsets.consumer as usize,
            flags: offsets.flags as usize,
            desc: offsets.desc as usize,
            mask: size - 1,
            size,
            local: 0,
            cached: 0,
            _entry: std::marker::PhantomData,
        };
        if producer {
            ring.local = ring.index(ring.producer).load(Ordering::Relaxed);
            ring.cached = ring.index(ring.consumer).load(Ordering::Acquire);
        } else {
            ring.local = ring.index(ring.consumer).load(Ordering::Relaxed);
            ring.cached = ring.index(ring.producer).load(Ordering::Acquire);
        }
        Ok(ring)
    }

    fn index(&self, at: usize) -> &AtomicU32 {
        // SAFETY: `at` is an offset the kernel gave for a 4-byte-aligned
        // `u32` inside the mapping, which lives as long as `self`; the
        // kernel accesses it only atomically.
        unsafe { &*(self.map.as_ptr().add(at) as *const AtomicU32) }
    }

    fn entry(&self, i: u32) -> *mut T {
        let at = self.desc + (i & self.mask) as usize * std::mem::size_of::<T>();
        debug_assert!(at + std::mem::size_of::<T>() <= self.map.len());
        // SAFETY: `i & mask` is below the ring's size, so the entry lies
        // inside the mapping (asserted in debug builds).
        unsafe { self.map.as_ptr().add(at) as *mut T }
    }

    /// True when the kernel asks to be woken to make progress on this ring
    /// (`XDP_USE_NEED_WAKEUP`).
    pub(crate) fn needs_wakeup(&self) -> bool {
        self.index(self.flags).load(Ordering::Relaxed) & libc::XDP_RING_NEED_WAKEUP != 0
    }

    /// As producer: entries free to write, at most `want`.
    pub(crate) fn free(&mut self, want: u32) -> u32 {
        let mut free = self.size - self.local.wrapping_sub(self.cached);
        if free < want {
            self.cached = self.index(self.consumer).load(Ordering::Acquire);
            free = self.size - self.local.wrapping_sub(self.cached);
        }
        free.min(want)
    }

    /// As producer: write `value` into the next free entry, which
    /// [`Self::free`] has counted; it is the kernel's after
    /// [`Self::submit`].
    pub(crate) fn push(&mut self, value: T) {
        // SAFETY: the entry is free (counted by `free`), so the kernel does
        // not read it until the producer index passes it.
        unsafe { self.entry(self.local).write(value) };
        self.local = self.local.wrapping_add(1);
    }

    /// As producer: entries handed over that the kernel has not taken yet.
    pub(crate) fn pending(&mut self) -> u32 {
        self.cached = self.index(self.consumer).load(Ordering::Acquire);
        self.local.wrapping_sub(self.cached)
    }

    /// As producer: hand the entries pushed to the kernel.
    pub(crate) fn submit(&self) {
        self.index(self.producer)
            .store(self.local, Ordering::Release);
    }

    /// As consumer: entries ready to read.
    pub(crate) fn ready(&mut self) -> u32 {
        let mut ready = self.cached.wrapping_sub(self.local);
        if ready == 0 {
            self.cached = self.index(self.producer).load(Ordering::Acquire);
            ready = self.cached.wrapping_sub(self.local);
        }
        ready
    }

    /// As consumer: read the next entry, which [`Self::ready`] has counted.
    pub(crate) fn peek(&self) -> T {
        // SAFETY: the entry is ready (counted by `ready` after an acquire
        // load of the producer index), so the kernel has written it and
        // does not write it again until the consumer index passes it.
        unsafe { self.entry(self.local).read() }
    }

    /// As consumer: give the entry read back to the kernel.
    pub(crate) fn release(&mut self) {
        self.release_n(1);
    }

    /// As consumer: read the entry `n` after the next, which
    /// [`Self::ready`] has counted (`n` below it).
    pub(crate) fn peek_nth(&self, n: u32) -> T {
        // SAFETY: as for `peek`: `n` is below the count `ready` returned,
        // so the kernel has written the entry and holds off until the
        // consumer index passes it.
        unsafe { self.entry(self.local.wrapping_add(n)).read() }
    }

    /// As consumer: give the next `n` entries read back to the kernel, with
    /// one store.
    pub(crate) fn release_n(&mut self, n: u32) {
        self.local = self.local.wrapping_add(n);
        self.index(self.consumer)
            .store(self.local, Ordering::Release);
    }
}
