// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! The system calls AF_XDP needs, each wrapped once: `bpf(2)` for the
//! XSKMAP, the program and its link to the interface, `mmap(2)` for the
//! UMEM and the rings, and the socket calls. Everything unsafe in the crate
//! but the rings' memory accesses ([`crate::ring`]) is here.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr::NonNull;

// bpf(2) commands, map and program types, attach types and flags, from
// <linux/bpf.h> and <linux/if_link.h>.
const BPF_MAP_CREATE: libc::c_int = 0;
const BPF_MAP_UPDATE_ELEM: libc::c_int = 2;
const BPF_PROG_LOAD: libc::c_int = 5;
const BPF_LINK_CREATE: libc::c_int = 28;
const BPF_MAP_TYPE_XSKMAP: u32 = 17;
const BPF_PROG_TYPE_XDP: u32 = 6;
const BPF_XDP: u32 = 37;
/// Attach in the driver's own XDP hook.
pub(crate) const XDP_FLAGS_DRV_MODE: u32 = 1 << 2;
/// Attach in the kernel's generic hook, which every interface has.
pub(crate) const XDP_FLAGS_SKB_MODE: u32 = 1 << 1;

/// `bpf(2)`'s attribute for `BPF_MAP_CREATE`, its leading fields.
#[repr(C)]
#[derive(Default)]
struct MapCreate {
    map_type: u32,
    key_size: u32,
    value_size: u32,
    max_entries: u32,
    map_flags: u32,
    inner_map_fd: u32,
    numa_node: u32,
    map_name: [u8; 16],
}

/// `bpf(2)`'s attribute for `BPF_MAP_UPDATE_ELEM`.
#[repr(C)]
#[derive(Default)]
struct MapElem {
    map_fd: u32,
    _pad: u32,
    key: u64,
    value: u64,
    flags: u64,
}

/// `bpf(2)`'s attribute for `BPF_PROG_LOAD`, its leading fields.
#[repr(C)]
#[derive(Default)]
struct ProgLoad {
    prog_type: u32,
    insn_cnt: u32,
    insns: u64,
    license: u64,
    log_level: u32,
    log_size: u32,
    log_buf: u64,
    kern_version: u32,
    prog_flags: u32,
    prog_name: [u8; 16],
    prog_ifindex: u32,
    expected_attach_type: u32,
}

/// `bpf(2)`'s attribute for `BPF_LINK_CREATE`, its leading fields.
#[repr(C)]
#[derive(Default)]
struct LinkCreate {
    prog_fd: u32,
    target_ifindex: u32,
    attach_type: u32,
    flags: u32,
}

/// Call `bpf(2)` with `attr`, returning the file descriptor or value it
/// returns.
fn bpf<T>(cmd: libc::c_int, attr: &mut T) -> io::Result<libc::c_long> {
    // SAFETY: `attr` is a live, initialised `#[repr(C)]` value of the
    // layout the kernel reads for `cmd`, passed with its exact size; the
    // kernel reads no further than that size and zero-fills the rest.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            cmd,
            attr as *mut T as *mut libc::c_void,
            std::mem::size_of::<T>() as libc::c_uint,
        )
    };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// Take ownership of the descriptor a successful call returned.
fn owned(fd: libc::c_long) -> OwnedFd {
    // SAFETY: `fd` was just returned by the kernel as a new descriptor that
    // nothing else owns.
    unsafe { OwnedFd::from_raw_fd(fd as RawFd) }
}

/// An XSKMAP of `entries` sockets, keyed by queue.
pub(crate) fn create_xskmap(entries: u32) -> io::Result<OwnedFd> {
    let mut name = [0u8; 16];
    name[..9].copy_from_slice(b"pulse_xsk");
    let mut attr = MapCreate {
        map_type: BPF_MAP_TYPE_XSKMAP,
        key_size: 4,
        value_size: 4,
        max_entries: entries,
        map_name: name,
        ..Default::default()
    };
    bpf(BPF_MAP_CREATE, &mut attr).map(owned)
}

/// Put socket `xsk` in `map` under `queue`.
pub(crate) fn xskmap_set(map: &OwnedFd, queue: u32, xsk: &OwnedFd) -> io::Result<()> {
    let key = queue;
    let value = xsk.as_raw_fd() as u32;
    let mut attr = MapElem {
        map_fd: map.as_raw_fd() as u32,
        key: &key as *const u32 as u64,
        value: &value as *const u32 as u64,
        ..Default::default()
    };
    bpf(BPF_MAP_UPDATE_ELEM, &mut attr).map(|_| ())
}

/// Load `insns`, an XDP program; on the verifier's refusal, its log.
pub(crate) fn load_xdp(insns: &[u8], name: &str) -> Result<OwnedFd, (io::Error, String)> {
    // The kernel asks every program for a license string; nothing here
    // calls a helper restricted by it.
    let license = CString::from(c"Proprietary");
    let mut prog_name = [0u8; 16];
    let n = name.len().min(15);
    prog_name[..n].copy_from_slice(&name.as_bytes()[..n]);
    let mut log = vec![0u8; 64 * 1024];
    let mut attr = ProgLoad {
        prog_type: BPF_PROG_TYPE_XDP,
        insn_cnt: (insns.len() / 8) as u32,
        insns: insns.as_ptr() as u64,
        license: license.as_ptr() as u64,
        log_level: 1,
        log_size: log.len() as u32,
        log_buf: log.as_mut_ptr() as u64,
        prog_name,
        expected_attach_type: BPF_XDP,
        ..Default::default()
    };
    match bpf(BPF_PROG_LOAD, &mut attr) {
        Ok(fd) => Ok(owned(fd)),
        Err(e) => {
            let end = log.iter().position(|&b| b == 0).unwrap_or(log.len());
            Err((e, String::from_utf8_lossy(&log[..end]).into_owned()))
        }
    }
}

/// Attach `prog` to interface `ifindex` in the hook `flags` name; the
/// program stays attached while the returned link is open.
pub(crate) fn link_xdp(prog: &OwnedFd, ifindex: u32, flags: u32) -> io::Result<OwnedFd> {
    let mut attr = LinkCreate {
        prog_fd: prog.as_raw_fd() as u32,
        target_ifindex: ifindex,
        attach_type: BPF_XDP,
        flags,
    };
    bpf(BPF_LINK_CREATE, &mut attr).map(owned)
}

/// The index of the interface named `name`.
pub(crate) fn ifindex(name: &str) -> io::Result<u32> {
    let c = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `c` is a NUL-terminated string that lives across the call.
    let index = unsafe { libc::if_nametoindex(c.as_ptr()) };
    if index == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(index)
    }
}

/// A new AF_XDP socket.
pub(crate) fn xsk_socket() -> io::Result<OwnedFd> {
    // SAFETY: plain socket(2) call; the result is checked before use.
    let fd = unsafe { libc::socket(libc::AF_XDP, libc::SOCK_RAW | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(owned(libc::c_long::from(fd)))
    }
}

/// `setsockopt(SOL_XDP, opt, value)`.
pub(crate) fn set_xdp_opt<T>(fd: &OwnedFd, opt: libc::c_int, value: &T) -> io::Result<()> {
    // SAFETY: `value` is a live `T` passed with its exact size, the layout
    // the kernel reads for `opt`.
    let ret = unsafe {
        libc::setsockopt(
            fd.as_raw_fd(),
            libc::SOL_XDP,
            opt,
            value as *const T as *const libc::c_void,
            std::mem::size_of::<T>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `getsockopt(SOL_XDP, opt)` into `value`.
fn get_xdp_opt<T: Copy>(fd: &OwnedFd, opt: libc::c_int, value: &mut T) -> io::Result<()> {
    let mut len = std::mem::size_of::<T>() as libc::socklen_t;
    // SAFETY: `value` is a live `T` of `len` bytes for the kernel to fill;
    // `len` lives across the call.
    let ret = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_XDP,
            opt,
            value as *mut T as *mut libc::c_void,
            &mut len,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    if len as usize != std::mem::size_of::<T>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the kernel returned a socket option of another size",
        ));
    }
    Ok(())
}

/// Where each ring lies in its mapping (`XDP_MMAP_OFFSETS`).
pub(crate) fn mmap_offsets(fd: &OwnedFd) -> io::Result<libc::xdp_mmap_offsets> {
    // SAFETY: plain integers, for which all-zero bytes are a valid value.
    let mut value: libc::xdp_mmap_offsets = unsafe { std::mem::zeroed() };
    get_xdp_opt(fd, libc::XDP_MMAP_OFFSETS, &mut value)?;
    Ok(value)
}

/// True when the socket is bound in zero-copy mode (`XDP_OPTIONS`).
pub(crate) fn zero_copy(fd: &OwnedFd) -> io::Result<bool> {
    let mut value = libc::xdp_options { flags: 0 };
    get_xdp_opt(fd, libc::XDP_OPTIONS, &mut value)?;
    Ok(value.flags & libc::XDP_OPTIONS_ZEROCOPY != 0)
}

/// The socket's counts of what the kernel dropped (`XDP_STATISTICS`).
pub(crate) fn statistics(fd: &OwnedFd) -> io::Result<libc::xdp_statistics> {
    // SAFETY: plain integers, for which all-zero bytes are a valid value.
    let mut value: libc::xdp_statistics = unsafe { std::mem::zeroed() };
    get_xdp_opt(fd, libc::XDP_STATISTICS, &mut value)?;
    Ok(value)
}

/// Bind `fd` to queue `queue` of interface `ifindex` with `flags`.
pub(crate) fn xsk_bind(fd: &OwnedFd, ifindex: u32, queue: u32, flags: u16) -> io::Result<()> {
    let addr = libc::sockaddr_xdp {
        sxdp_family: libc::AF_XDP as u16,
        sxdp_flags: flags,
        sxdp_ifindex: ifindex,
        sxdp_queue_id: queue,
        sxdp_shared_umem_fd: 0,
    };
    // SAFETY: `addr` is an initialised `sockaddr_xdp` passed with its size.
    let ret = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            &addr as *const libc::sockaddr_xdp as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_xdp>() as libc::socklen_t,
        )
    };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Wake the kernel to send what the TX ring holds.
pub(crate) fn kick_tx(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: a zero-length send with no buffer and no address, which
    // AF_XDP takes as a wakeup.
    let ret = unsafe {
        libc::sendto(
            fd.as_raw_fd(),
            std::ptr::null(),
            0,
            libc::MSG_DONTWAIT,
            std::ptr::null(),
            0,
        )
    };
    if ret >= 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    // The kernel is busy, or out of room for now: the frames stay queued
    // and the next kick sends them. Anything else, an interface gone down
    // included, is the caller's to see.
    match e.raw_os_error() {
        Some(libc::EAGAIN | libc::EBUSY | libc::ENOBUFS) => Ok(()),
        _ => Err(e),
    }
}

/// Wait up to `timeout_ms` for `fd` to have frames to read; true when it
/// does.
///
/// # Errors
/// A failed `poll`, or the socket's error when `poll` reports one (an
/// interface removed under it unbinds it, with `ENETDOWN`).
pub(crate) fn poll_in(fd: &OwnedFd, timeout_ms: libc::c_int) -> io::Result<bool> {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: one live `pollfd`, counted as one.
        let ret = unsafe { libc::poll(&mut p, 1, timeout_ms) };
        if ret < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if p.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(
                socket_error(fd).unwrap_or_else(|| io::Error::from_raw_os_error(libc::ENETDOWN))
            );
        }
        return Ok(ret > 0);
    }
}

/// The error pending on `fd` (`SO_ERROR`), if any.
fn socket_error(fd: &OwnedFd) -> Option<io::Error> {
    let mut err: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `err` is a live `c_int` of `len` bytes for the kernel to fill.
    let ret = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            &mut err as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        )
    };
    (ret == 0 && err != 0).then(|| io::Error::from_raw_os_error(err))
}

/// Ask for busy polling on `fd`: a read or `poll` on it runs the queue's
/// NAPI loop in the caller's context for up to `usecs` microseconds,
/// `budget` packets at a time, rather than waiting for its interrupt
/// (`SO_PREFER_BUSY_POLL`, Linux 5.11).
pub(crate) fn set_busy_poll(fd: &OwnedFd, usecs: u32, budget: u32) -> io::Result<()> {
    for (opt, value) in [
        (libc::SO_PREFER_BUSY_POLL, 1),
        (libc::SO_BUSY_POLL, usecs as libc::c_int),
        (libc::SO_BUSY_POLL_BUDGET, budget as libc::c_int),
    ] {
        // SAFETY: a live `c_int` passed with its size.
        let ret = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                &value as *const libc::c_int as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Whether `fd` is still bound: an interface removed under an AF_XDP
/// socket unbinds it, and from then a read answers `ENXIO` while `poll`,
/// on recent kernels, reports nothing at all.
///
/// # Errors
/// `ENXIO` or `ENETDOWN` for a socket that is no longer bound.
pub(crate) fn check_bound(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: a zero-length read with no buffer and no address.
    let ret = unsafe {
        libc::recvfrom(
            fd.as_raw_fd(),
            std::ptr::null_mut(),
            0,
            libc::MSG_DONTWAIT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ret >= 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ENXIO | libc::ENETDOWN) => Err(e),
        _ => Ok(()),
    }
}

/// An anonymous, private, read-write mapping, unmapped when dropped.
pub(crate) struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is plain memory owned by this value; access to it is
// governed by the borrows of the value that holds it.
unsafe impl Send for Mapping {}

impl Mapping {
    /// `len` bytes of anonymous memory, faulted in up front.
    pub(crate) fn anonymous(len: usize) -> io::Result<Self> {
        Self::map(
            len,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_POPULATE,
            -1,
            0,
        )
    }

    /// The ring at `offset` of the AF_XDP socket `fd`, `len` bytes, shared
    /// with the kernel.
    pub(crate) fn ring(fd: &OwnedFd, offset: u64, len: usize) -> io::Result<Self> {
        Self::map(
            len,
            libc::MAP_SHARED | libc::MAP_POPULATE,
            fd.as_raw_fd(),
            offset as libc::off_t,
        )
    }

    fn map(len: usize, flags: libc::c_int, fd: RawFd, offset: libc::off_t) -> io::Result<Self> {
        // SAFETY: a fresh mapping at an address the kernel picks; nothing
        // existing is replaced, and the result is checked.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                flags,
                fd,
                offset,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let ptr =
            NonNull::new(ptr.cast::<u8>()).ok_or_else(|| io::Error::from(io::ErrorKind::Other))?;
        Ok(Self { ptr, len })
    }

    /// The mapping's first byte.
    pub(crate) fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    /// Its length in bytes.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The `len` bytes at `offset`, if they lie inside the mapping.
    ///
    /// The caller passes only a frame it owns: one the kernel handed back
    /// through the RX or completion ring and that it has not lent again.
    /// The kernel writes the frames it holds, so a reference never covers
    /// more than the frame asked for.
    pub(crate) fn frame(&self, offset: u64, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(len)?;
        if end > self.len {
            return None;
        }
        // SAFETY: `start..end` lies inside the mapping, which lives as long
        // as `self`, and is a frame the caller owns, which the kernel does
        // not write until the caller lends it again (see above).
        Some(unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().add(start), len) })
    }

    /// [`Self::frame`], to write.
    pub(crate) fn frame_mut(&mut self, offset: u64, len: usize) -> Option<&mut [u8]> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(len)?;
        if end > self.len {
            return None;
        }
        // SAFETY: as for `frame`; the kernel neither reads nor writes a
        // frame the caller owns, and `&mut self` makes this the only view
        // this side holds.
        Some(unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().add(start), len) })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: this value owns the mapping, made by `mmap` with this
        // address and length, and nothing uses it after the drop.
        unsafe {
            libc::munmap(self.ptr.as_ptr().cast(), self.len);
        }
    }
}
