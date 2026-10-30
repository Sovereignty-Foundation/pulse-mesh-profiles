// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! The large-payload pool across processes: a payload written in one process
//! is read intact in another, each way, and the segment it held is freed for
//! reuse; and a pool `/dev/shm` has no room for is refused at creation.
//!
//! The second process is this test binary run again, with its role in the
//! environment.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr
)]

use std::io::Write;
use std::process::{Command, Stdio};

use robotics_stages::large_payload::{LargePayloadError, LargePayloadPool, PoolConfig, SegmentRef};

const ROLE: &str = "LP_TEST_ROLE";
const NAME: &str = "LP_TEST_NAME";
const REF: &str = "LP_TEST_REF";
const TEST: &str = "a_payload_crosses_processes_each_way_and_its_segment_is_reused";

/// 4 MiB, a LIDAR scan's size.
const PAYLOAD: usize = 4 << 20;

fn pattern(seed: u8) -> Vec<u8> {
    (0..PAYLOAD)
        .map(|i| ((i >> 3) as u8).wrapping_mul(29).wrapping_add(seed))
        .collect()
}

fn ref_hex(seg: &SegmentRef) -> String {
    seg.to_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn ref_from_hex(hex: &str) -> SegmentRef {
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    SegmentRef::from_bytes(&bytes)
}

/// Run this binary's cross-process test as `role` on the pool `name`, with
/// `seg` handed over when given; returns what it printed, once it exits 0.
fn child(role: &str, name: &str, seg: Option<&SegmentRef>) -> String {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(NAME, name)
        .stdin(Stdio::null());
    if let Some(seg) = seg {
        cmd.env(REF, ref_hex(seg));
    }
    let out = cmd.output().expect("run the child");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the {role} child failed: {}\n{text}",
        out.status
    );
    text
}

/// The child's half: `write` writes the 4 MiB pattern and prints its
/// reference, leaving the segment held for the parent; `read` reads the
/// parent's payload by the reference it was given, checks it, and frees it.
fn run_role(role: &str, name: &str) {
    let pool = LargePayloadPool::open(name).expect("open the parent's pool");
    match role {
        "write" => {
            let seg = pool.write(&pattern(2)).expect("write the payload");
            println!("ref {}", ref_hex(&seg));
        }
        "read" => {
            let seg = ref_from_hex(&std::env::var(REF).unwrap());
            let mut out = Vec::new();
            pool.read(&seg, &mut out)
                .expect("read the parent's payload");
            assert!(out == pattern(1), "the payload changed between processes");
            pool.free(seg.id).expect("free the segment");
            println!("read ok");
        }
        other => panic!("unknown role {other}"),
    }
}

#[test]
fn a_payload_crosses_processes_each_way_and_its_segment_is_reused() {
    if let Ok(role) = std::env::var(ROLE) {
        run_role(&role, &std::env::var(NAME).unwrap());
        std::io::stdout().flush().unwrap();
        return;
    }
    let name = format!("lp-xp-{:x}", std::process::id());
    let mut cfg = PoolConfig::default();
    cfg.name = name.clone();
    cfg.segment_count = 2;
    cfg.segment_bytes = PAYLOAD;
    let pool = LargePayloadPool::create(cfg).unwrap();

    // Written in the child, read here.
    let text = child("write", &name, None);
    let hex = text
        .lines()
        // The harness prints the test's name on the same line.
        .find_map(|line| line.split_once("ref ").and_then(|(_, hex)| hex.get(..32)))
        .unwrap_or_else(|| panic!("the writer printed no reference:\n{text}"));
    let from_child = ref_from_hex(hex);
    assert_eq!(
        pool.in_use(),
        1,
        "the child's segment stays held after it exits"
    );
    let mut out = Vec::new();
    pool.read(&from_child, &mut out).unwrap();
    assert!(
        out == pattern(2),
        "the child's payload changed between processes"
    );
    pool.free(from_child.id).unwrap();

    // Written here, read and freed in the child.
    let mine = pool.write(&pattern(1)).unwrap();
    let text = child("read", &name, Some(&mine));
    assert!(
        text.contains("read ok"),
        "the reader did not finish:\n{text}"
    );
    assert_eq!(pool.in_use(), 0, "the child's free reached this process");
    assert!(matches!(
        pool.read(&mine, &mut out),
        Err(LargePayloadError::InvalidSegment(_))
    ));

    // Both segments are free again: two more writes fit, a third does not.
    let a = pool.write(&pattern(3)).unwrap();
    let b = pool.write(&pattern(4)).unwrap();
    assert!(matches!(
        pool.alloc(),
        Err(LargePayloadError::Exhausted { used: 2, total: 2 })
    ));
    pool.free(a.id).unwrap();
    pool.free(b.id).unwrap();
}

/// Total and free bytes of the tmpfs behind `/dev/shm`, or `None` where
/// there is none with a size limit to measure.
#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_cast)] // the statvfs field types differ between targets
fn dev_shm_space() -> Option<(u64, u64)> {
    let path = std::ffi::CString::new("/dev/shm").unwrap();
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: statvfs reads the NUL-terminated path and writes one
    // `statvfs` into the buffer, which is read only when it returns 0.
    if unsafe { libc::statvfs(path.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so it initialised the buffer.
    let st = unsafe { st.assume_init() };
    let unit = st.f_frsize as u64;
    let total = st.f_blocks as u64 * unit;
    (total > 0).then(|| (total, st.f_bavail as u64 * unit))
}

/// A pool larger than all of `/dev/shm` is `NoSpace` at creation and leaves
/// no object behind: Linux reserves a pool's pages before it maps them, so
/// the first touch of a page cannot be SIGBUS. Asking for more than the
/// whole filesystem fails before any page is taken, so the test fills
/// nothing other tests use.
#[cfg(target_os = "linux")]
#[test]
fn a_pool_dev_shm_has_no_room_for_is_no_space_and_leaves_nothing() {
    let Some((total, _)) = dev_shm_space() else {
        eprintln!("no size-limited /dev/shm to measure; nothing checked");
        return;
    };
    let segment_bytes = 4usize << 20;
    let segment_count = u32::try_from(total / segment_bytes as u64 + 2).unwrap();
    let name = format!("lp-nospace-{:x}", std::process::id());
    let mut cfg = PoolConfig::default();
    cfg.name = name.clone();
    cfg.segment_count = segment_count;
    cfg.segment_bytes = segment_bytes;
    match LargePayloadPool::create(cfg) {
        Err(LargePayloadError::NoSpace { need, errno }) => {
            assert!(
                need > total,
                "{need} bytes asked of a {total}-byte /dev/shm"
            );
            assert!(
                errno == libc::ENOSPC || errno == libc::ENOMEM,
                "unexpected errno {errno}"
            );
        }
        Err(other) => panic!("expected NoSpace, got {other:?}"),
        Ok(_) => panic!("made a pool larger than a {total}-byte /dev/shm"),
    }
    assert!(
        !std::path::Path::new("/dev/shm").join(&name).exists(),
        "the refused pool left its object behind"
    );
}
