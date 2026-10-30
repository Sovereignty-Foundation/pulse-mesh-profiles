// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! A NUMA backing is created, never reset, and the NUMA helpers refuse what
//! they cannot do instead of panicking or aborting.
//!
//! An allocation on the path of a live backing is refused and leaves the
//! file alone, since cutting it would SIGBUS the processes that have it
//! mapped; a failed allocation removes the file it created; a dropped
//! backing removes its file, but not a file that has taken its path since.
//! The page rounding is checked, and a CPU number past what a `cpu_set_t`
//! can name is an error, not an abort.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::ErrorKind;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use core_ring::{PulseRing, HEADER_SIZE, PULSE_SIZE};
use hft_stages::numa::{
    allocate_shm_backing, dup_fd, NumaError, PageSize, ShmBacking, ShmNumaConfig,
};
use pulse_canon::Pulse;

/// A directory of its own for this test's backing files: under `/dev/shm`
/// where there is one, as on Linux, else in the system's temporary
/// directory.
fn backing_dir() -> tempfile::TempDir {
    if Path::new("/dev/shm").is_dir() {
        if let Ok(dir) = tempfile::tempdir_in("/dev/shm") {
            return dir;
        }
    }
    tempfile::tempdir().expect("tempdir")
}

/// A backing for a ring of `slots` 128-byte slots at `path`, on regular
/// pages, bound to no node.
fn config(path: &Path, slots: u64) -> ShmNumaConfig<'_> {
    ShmNumaConfig::new(path, HEADER_SIZE + slots as usize * PULSE_SIZE)
        .with_page_size(PageSize::Regular)
        .with_numa_node(None)
}

/// A pulse carrying `seq` in two fields.
fn tagged(seq: u64) -> Pulse {
    let mut pulse = Pulse::zeroed();
    pulse.monotonic_nanos = seq;
    pulse.chain_step = !seq;
    pulse
}

/// Whether an allocation was refused because its file already exists.
fn already_exists(allocated: &Result<ShmBacking, NumaError>) -> bool {
    matches!(allocated, Err(NumaError::Io(e)) if e.kind() == ErrorKind::AlreadyExists)
}

/// Raises the flag when dropped, so the reader stops even when a failed
/// assertion unwinds the main thread, and the test fails instead of
/// hanging.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[test]
fn a_second_allocation_on_a_live_backing_is_refused_and_changes_nothing() {
    let dir = backing_dir();
    let path = dir.path().join("live.shm");
    let cfg = config(&path, 64);
    let backing = allocate_shm_backing(&cfg).expect("allocate the backing");
    let len = backing.file().metadata().expect("metadata").len();
    let mut ring = PulseRing::from_fd("numa-live", backing.as_raw_fd(), 64, false, false)
        .expect("a ring over the backing");
    for seq in 0..3 {
        ring.publish(&tagged(seq));
    }
    let again = allocate_shm_backing(&cfg);
    assert!(
        already_exists(&again),
        "expected AlreadyExists, got {again:?}"
    );
    assert_eq!(
        std::fs::metadata(&path).expect("metadata").len(),
        len,
        "the live backing was resized"
    );
    assert_eq!(ring.cursor_position(), 3, "the ring's cursor was reset");
    for seq in 0..3 {
        let pulse = ring.read_slot(seq).expect("a slot the ring wrote");
        assert_eq!(pulse.monotonic_nanos, seq, "the ring's contents changed");
    }
}

/// Wait until the reader has finished `walks` walks of the mapping, or has
/// stopped: a failed assertion ends it, and its join reports the failure.
fn wait_for_walks(done: &AtomicU64, walks: u64, reader: &std::thread::ScopedJoinHandle<'_, ()>) {
    while done.load(Ordering::SeqCst) < walks && !reader.is_finished() {
        std::thread::yield_now();
    }
}

/// A reader walks the mapping of a live backing while allocations on its
/// path are attempted over and over; each is refused without touching the
/// file, so the reader never meets a cut page.
///
/// The allocations start once the reader has walked the mapping, and the
/// reader stops only after a walk that began after the last of them, so
/// even a reader the scheduler runs late walks the mapping before and after
/// them, and finds a backing any of them had cut changed.
#[test]
fn a_reader_keeps_its_backing_while_allocations_on_its_path_are_refused() {
    const SLOTS: u64 = 4096;
    let dir = backing_dir();
    let path = dir.path().join("race.shm");
    let cfg = config(&path, SLOTS);
    let backing = allocate_shm_backing(&cfg).expect("allocate the backing");
    let mut ring = PulseRing::from_fd("numa-race", backing.as_raw_fd(), SLOTS, false, false)
        .expect("a ring over the backing");
    for seq in 0..SLOTS {
        ring.publish(&tagged(seq));
    }
    let stop = AtomicBool::new(false);
    let walks = AtomicU64::new(0);
    std::thread::scope(|s| {
        let reader = s.spawn(|| loop {
            for slot in (0..SLOTS).step_by(32) {
                let pulse = ring.read_slot(slot).expect("a slot the ring wrote");
                assert_eq!(pulse.monotonic_nanos, slot, "the backing changed");
            }
            walks.fetch_add(1, Ordering::SeqCst);
            if stop.load(Ordering::SeqCst) {
                break;
            }
        });
        let _stop = StopOnDrop(&stop);
        wait_for_walks(&walks, 1, &reader);
        for attempt in 0..2_000 {
            let again = allocate_shm_backing(&cfg);
            assert!(
                already_exists(&again),
                "attempt {attempt}: expected AlreadyExists, got {again:?}"
            );
        }
        // A walk under way now may have begun before the last allocation;
        // the one after it began after.
        let finished = walks.load(Ordering::SeqCst);
        wait_for_walks(&walks, finished + 2, &reader);
        stop.store(true, Ordering::SeqCst);
        reader.join().expect("the reader");
    });
    assert!(
        walks.load(Ordering::SeqCst) >= 2,
        "the reader did not walk the backing"
    );
}

#[test]
fn a_failed_allocation_removes_the_file_it_created() {
    let dir = backing_dir();
    let path = dir.path().join("unbound.shm");
    // No host has a NUMA node 63, and a build for another platform binds no
    // node at all, so the binding fails after the file is created and sized.
    let bound = config(&path, 64).with_numa_node(Some(63));
    match allocate_shm_backing(&bound) {
        Err(NumaError::NumaPin { node: 63, .. } | NumaError::LibnumaMissing) => {}
        other => panic!("expected the binding to fail, got {other:?}"),
    }
    assert!(!path.exists(), "the failed allocation left its file");
    // The path is free: the same backing without the binding is made.
    allocate_shm_backing(&config(&path, 64)).expect("allocate on the freed path");
}

#[test]
fn a_dropped_backing_removes_its_file_and_its_rings_live_on() {
    let dir = backing_dir();
    let path = dir.path().join("dropped.shm");
    let backing = allocate_shm_backing(&config(&path, 64)).expect("allocate the backing");
    assert_eq!(backing.path(), path);
    // A ring over a descriptor of its own keeps the file's pages, as a
    // ring's readers keep a ring whose name its creator unlinked.
    let fd = dup_fd(backing.as_raw_fd()).expect("dup the descriptor");
    let mut ring = PulseRing::from_fd("numa-drop", fd, 64, true, false).expect("a ring");
    ring.publish(&tagged(7));

    drop(backing);
    assert!(!path.exists(), "the dropped backing left its file");
    let pulse = ring.read_slot(0).expect("the ring still reads");
    assert_eq!(pulse.monotonic_nanos, 7);
    ring.publish(&tagged(8));
    assert_eq!(
        ring.read_slot(1)
            .expect("and still takes pulses")
            .monotonic_nanos,
        8
    );

    // The path is free for a new backing.
    let again = allocate_shm_backing(&config(&path, 64)).expect("a backing on the freed path");
    drop(again);
    assert!(!path.exists(), "the second backing left its file");
}

#[test]
fn a_dropped_backing_leaves_alone_a_file_that_took_its_path() {
    let dir = backing_dir();
    let path = dir.path().join("replaced.shm");
    let backing = allocate_shm_backing(&config(&path, 64)).expect("allocate the backing");
    // Another owner removes the file and puts one of its own there.
    std::fs::remove_file(&path).expect("remove the backing's file");
    std::fs::write(&path, b"another owner's").expect("write a new file");
    drop(backing);
    assert_eq!(
        std::fs::read(&path).expect("the new file is still there"),
        b"another owner's",
        "the backing removed a file it did not create"
    );
}

#[test]
fn a_capacity_whose_rounding_overflows_is_an_error_and_creates_nothing() {
    let dir = backing_dir();
    let path = dir.path().join("overflow.shm");
    let cfg = config(&path, 64).with_capacity_bytes(usize::MAX);
    match allocate_shm_backing(&cfg) {
        Err(NumaError::Io(e)) if e.kind() == ErrorKind::InvalidInput => {}
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    assert!(!path.exists(), "a file was created for an impossible size");
}

#[cfg(target_os = "linux")]
#[test]
fn a_memfd_capacity_that_rounds_past_a_file_offset_is_an_error() {
    use hft_stages::numa::allocate_hugepage_memfd;
    // The first rounds past a usize, the second fits one but not an off_t.
    for capacity in [usize::MAX, 1 << 63] {
        match allocate_hugepage_memfd("numa-overflow", capacity, PageSize::Regular) {
            Err(NumaError::HugePageAlloc(reason)) => assert!(
                reason.contains("does not fit"),
                "capacity {capacity}: unexpected reason {reason}"
            ),
            other => panic!("capacity {capacity}: expected HugePageAlloc, got {other:?}"),
        }
    }
}

/// The CPUs the calling thread may run on.
#[cfg(target_os = "linux")]
fn affinity_count() -> i32 {
    // SAFETY: `cpu_set_t` is a plain bitmask for which all-zero is a valid
    // value, the empty set.
    let mut set = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
    // SAFETY: sched_getaffinity(0, ...) writes at most the size passed,
    // `size_of::<cpu_set_t>()` bytes, into the local set, and touches
    // nothing else.
    let rc =
        unsafe { libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) };
    assert_eq!(rc, 0, "sched_getaffinity failed");
    // SAFETY: CPU_COUNT only reads the local set, which is initialised.
    unsafe { libc::CPU_COUNT(&set) }
}

#[cfg(target_os = "linux")]
#[test]
fn a_cpu_number_past_the_cpu_set_is_an_error_not_an_abort() {
    use hft_stages::numa::pin_thread_to_numa_node;
    let before = affinity_count();
    // CPU 1024 is the first a cpu_set_t cannot name; u32::MAX does not fit
    // the error's i32 either. CPU 0 is valid, and the call still changes
    // nothing when a later CPU is refused.
    for (cpu, reported) in [(1024u32, 1024i32), (u32::MAX, -1)] {
        match pin_thread_to_numa_node(0, Some(&[0, cpu])) {
            Err(NumaError::NumaPin {
                cpu: got, node: 0, ..
            }) => assert_eq!(got, reported),
            other => panic!("CPU {cpu}: expected NumaPin, got {other:?}"),
        }
    }
    assert_eq!(affinity_count(), before, "the thread's affinity changed");
}
