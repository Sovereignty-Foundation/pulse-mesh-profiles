// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! `hft_core::prefetch`: cache-line prefetch hints for a receive loop, and
//! the prefetch distances recommended per workload (module M09).

#![warn(missing_docs)]

/// Hint to the CPU prefetcher to bring the cache line at `addr` into L1: a
/// no-op on architectures other than x86_64 and aarch64.
///
/// # Safety
///
/// `addr` must be a valid pointer into mapped memory. Reading from a
/// prefetched address that turns out to be invalid is a fault.
#[inline(always)]
pub unsafe fn prefetch_l1<T>(addr: *const T) {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
        // SAFETY: the caller keeps `addr` a pointer into mapped memory;
        // _mm_prefetch is a hint with no effect beyond warming the line.
        unsafe {
            _mm_prefetch(addr as *const i8, _MM_HINT_T0);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: the caller keeps `addr` a pointer into mapped memory;
        // prfm is a hint with no architecturally visible effect.
        unsafe {
            std::arch::asm!(
                "prfm pldl1keep, [{}]",
                in(reg) addr,
                options(nostack, preserves_flags),
            );
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = addr;
    }
}

/// Recommended prefetch distances per workload, measured on an H100 host.
pub mod recommended {
    /// Quiet workload (no co-located noise). Prefetch is no-op or mild win.
    pub const QUIET: u32 = 0;
    /// Multi-tenant noise (44 background threads on 16 cores).
    pub const NOISE: u32 = 1;
    /// Burst microsecond cadence with cold cache. Conservative hint.
    pub const BURST: u32 = 2;
}

#[cfg(test)]
// `assert!` in a test names the ordering that broke.
#[allow(clippy::assertions_on_constants)]
mod tests {
    use super::*;

    #[test]
    fn prefetch_is_noop_on_invalid_arch() {
        let value: u64 = 42;
        // SAFETY: `&value` points into a live local, which is mapped
        // memory, as `prefetch_l1` requires.
        unsafe {
            prefetch_l1(&value as *const u64);
        }
        assert_eq!(value, 42);
    }

    #[test]
    fn recommended_constants_ordered() {
        assert!(recommended::QUIET < recommended::NOISE);
        assert!(recommended::NOISE < recommended::BURST);
    }
}
