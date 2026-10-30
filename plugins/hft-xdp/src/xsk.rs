// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! An AF_XDP socket with its UMEM and rings ([`Socket`]), the receive
//! path, which feeds it through the XDP program ([`RxPath`]), and the send
//! path ([`TxPath`]).

use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use crate::ring::Ring;
use crate::sys::{self, Mapping};
use crate::{prog, AttachMode, XdpError, ZeroCopy};

/// A UMEM frame: one packet, in aligned mode, so a frame's address is its
/// index times this.
pub(crate) const FRAME_SIZE: u32 = 4096;

/// The longest a receive waits on `poll` before checking that its socket is
/// still bound: an interface removed under it leaves `poll` silent on
/// recent kernels.
const LIVENESS: Duration = Duration::from_secs(1);

/// An error from step `op` of setting up or running the socket.
fn io(op: &'static str) -> impl FnOnce(io::Error) -> XdpError {
    move |source| XdpError::Io { op, source }
}

/// An AF_XDP socket bound to one queue, with a UMEM of its own.
pub(crate) struct Socket {
    pub(crate) fd: OwnedFd,
    pub(crate) umem: Mapping,
    pub(crate) fill: Ring<u64>,
    pub(crate) comp: Ring<u64>,
    pub(crate) rx: Option<Ring<libc::xdp_desc>>,
    pub(crate) tx: Option<Ring<libc::xdp_desc>>,
    pub(crate) zero_copy: bool,
}

impl Socket {
    /// Open a socket on queue `queue` of interface `ifindex` with `frames`
    /// UMEM frames and rings of `ring` entries, with an RX ring, a TX ring
    /// or both.
    pub(crate) fn open(
        ifindex: u32,
        queue: u32,
        frames: u32,
        ring: u32,
        rx: bool,
        tx: bool,
        zero_copy: ZeroCopy,
    ) -> Result<Self, XdpError> {
        let fd = sys::xsk_socket().map_err(io("open an AF_XDP socket"))?;
        let len = frames as usize * FRAME_SIZE as usize;
        let umem = Mapping::anonymous(len).map_err(io("map the UMEM"))?;
        let reg = libc::xdp_umem_reg {
            addr: umem.as_ptr() as u64,
            len: len as u64,
            chunk_size: FRAME_SIZE,
            headroom: 0,
            flags: 0,
            tx_metadata_len: 0,
        };
        // The UMEM is locked memory: without CAP_IPC_LOCK it counts against
        // RLIMIT_MEMLOCK, and past it the kernel answers ENOBUFS.
        sys::set_xdp_opt(&fd, libc::XDP_UMEM_REG, &reg).map_err(io(
            "register the UMEM (locked memory: grant CAP_IPC_LOCK, raise RLIMIT_MEMLOCK, \
             or use fewer frames)",
        ))?;
        // The fill ring holds every frame a receiver lends the kernel; a
        // sender lends none, but the kernel wants the ring.
        let fill_size = if rx { frames } else { ring };
        sys::set_xdp_opt(&fd, libc::XDP_UMEM_FILL_RING, &fill_size)
            .map_err(io("size the fill ring"))?;
        sys::set_xdp_opt(&fd, libc::XDP_UMEM_COMPLETION_RING, &ring)
            .map_err(io("size the completion ring"))?;
        if rx {
            sys::set_xdp_opt(&fd, libc::XDP_RX_RING, &ring).map_err(io("size the RX ring"))?;
        }
        if tx {
            sys::set_xdp_opt(&fd, libc::XDP_TX_RING, &ring).map_err(io("size the TX ring"))?;
        }
        let off = sys::mmap_offsets(&fd).map_err(io("read the ring offsets"))?;
        let fill = Ring::map(
            &fd,
            libc::XDP_UMEM_PGOFF_FILL_RING,
            fill_size,
            off.fr.into(),
            true,
        )
        .map_err(io("map the fill ring"))?;
        let comp = Ring::map(
            &fd,
            libc::XDP_UMEM_PGOFF_COMPLETION_RING,
            ring,
            off.cr.into(),
            false,
        )
        .map_err(io("map the completion ring"))?;
        let rx_ring = rx
            .then(|| {
                Ring::map(
                    &fd,
                    libc::XDP_PGOFF_RX_RING as u64,
                    ring,
                    off.rx.into(),
                    false,
                )
            })
            .transpose()
            .map_err(io("map the RX ring"))?;
        let tx_ring = tx
            .then(|| {
                Ring::map(
                    &fd,
                    libc::XDP_PGOFF_TX_RING as u64,
                    ring,
                    off.tx.into(),
                    true,
                )
            })
            .transpose()
            .map_err(io("map the TX ring"))?;
        let flags = libc::XDP_USE_NEED_WAKEUP
            | match zero_copy {
                ZeroCopy::Auto => 0,
                ZeroCopy::Required => libc::XDP_ZEROCOPY,
                ZeroCopy::Off => libc::XDP_COPY,
            };
        // One socket per interface queue: a second one answers EBUSY.
        sys::xsk_bind(&fd, ifindex, queue, flags).map_err(io(
            "bind the AF_XDP socket (one socket per interface queue)",
        ))?;
        let zero_copy = sys::zero_copy(&fd).map_err(io("read the socket's mode"))?;
        Ok(Self {
            fd,
            umem,
            fill,
            comp,
            rx: rx_ring,
            tx: tx_ring,
            zero_copy,
        })
    }

    /// The kernel's counts of what it dropped on this socket.
    pub(crate) fn statistics(&self) -> io::Result<libc::xdp_statistics> {
        sys::statistics(&self.fd)
    }
}

/// The receive path: a socket whose frames are all lent to the kernel
/// through the fill ring, and the XDP program that sends it the packets for
/// its port, which stays attached while this lives.
pub(crate) struct RxPath {
    // Fields drop in order: the link first, so the program stops sending
    // packets here before the socket closes.
    _link: OwnedFd,
    _prog: OwnedFd,
    _map: OwnedFd,
    pub(crate) sock: Socket,
    pub(crate) native: bool,
    /// Spin on non-blocking reads, which run the queue's NAPI loop, rather
    /// than wait in `poll` for its interrupt.
    busy_poll: bool,
}

impl RxPath {
    /// Open the receive path on queue `queue` of `ifindex` for UDP `port`
    /// and, if given, IPv4 address `addr`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        ifindex: u32,
        queue: u32,
        port: u16,
        addr: Option<Ipv4Addr>,
        frames: u32,
        ring: u32,
        attach: AttachMode,
        zero_copy: ZeroCopy,
    ) -> Result<Self, XdpError> {
        let entries = queue
            .checked_add(1)
            .ok_or(XdpError::InvalidConfig("no such queue"))?;
        let mut sock = Socket::open(ifindex, queue, frames, ring, true, false, zero_copy)?;
        let lend = sock.fill.free(frames);
        for i in 0..lend {
            sock.fill.push(u64::from(i) * u64::from(FRAME_SIZE));
        }
        sock.fill.submit();

        let map = sys::create_xskmap(entries).map_err(io("create the XSKMAP"))?;
        sys::xskmap_set(&map, queue, &sock.fd).map_err(io("put the socket in the XSKMAP"))?;
        let program = prog::to_bytes(&prog::redirect_program(port, addr, map.as_raw_fd()));
        let prog = sys::load_xdp(&program, "pulse_xsk_rx")
            .map_err(|(source, log)| XdpError::Verifier { log, source })?;
        let (link, native) = match attach {
            AttachMode::Native => (
                sys::link_xdp(&prog, ifindex, sys::XDP_FLAGS_DRV_MODE)
                    .map_err(io("attach the XDP program in the driver's hook"))?,
                true,
            ),
            AttachMode::Generic => (
                sys::link_xdp(&prog, ifindex, sys::XDP_FLAGS_SKB_MODE)
                    .map_err(io("attach the XDP program in the generic hook"))?,
                false,
            ),
            AttachMode::Auto => match sys::link_xdp(&prog, ifindex, sys::XDP_FLAGS_DRV_MODE) {
                Ok(link) => (link, true),
                // EBUSY is another program holding the interface, which the
                // generic hook reports too; anything else is the driver
                // lacking a hook of its own.
                Err(_) => (
                    sys::link_xdp(&prog, ifindex, sys::XDP_FLAGS_SKB_MODE).map_err(io(
                        "attach the XDP program (the driver's hook refused, then the generic \
                         hook; EBUSY is another program on the interface)",
                    ))?,
                    false,
                ),
            },
        };
        Ok(Self {
            _link: link,
            _prog: prog,
            _map: map,
            sock,
            native,
            busy_poll: false,
        })
    }

    /// Busy-poll the queue from here on, `usecs` at a time and `budget`
    /// packets per run of its NAPI loop.
    pub(crate) fn enable_busy_poll(&mut self, usecs: u32, budget: u32) -> Result<(), XdpError> {
        sys::set_busy_poll(&self.sock.fd, usecs, budget)
            .map_err(io("ask for busy polling (Linux 5.11 or later)"))?;
        self.busy_poll = true;
        Ok(())
    }

    /// Wait until the RX ring holds a packet, or `deadline` passes.
    fn wait(&mut self, deadline: Option<Instant>) -> Result<(), XdpError> {
        loop {
            let rx = self
                .sock
                .rx
                .as_mut()
                .ok_or(XdpError::InvalidConfig("no RX ring"))?;
            if rx.ready() > 0 {
                return Ok(());
            }
            let left = match deadline {
                None => None,
                Some(d) => {
                    let left = d.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(XdpError::Io {
                            op: "receive",
                            source: io::ErrorKind::TimedOut.into(),
                        });
                    }
                    Some(left)
                }
            };
            if self.busy_poll {
                // A non-blocking read runs the queue's NAPI loop here, and
                // fails once the socket is unbound.
                sys::check_bound(&self.sock.fd).map_err(io("wait for a packet"))?;
                continue;
            }
            let slice = left.map_or(LIVENESS, |l| l.min(LIVENESS));
            let ms = slice.as_millis().clamp(1, i32::MAX as u128) as i32;
            if !sys::poll_in(&self.sock.fd, ms).map_err(io("wait for a packet"))? {
                sys::check_bound(&self.sock.fd).map_err(io("wait for a packet"))?;
            }
        }
    }

    /// Hand up to `max` packets the kernel delivered to `take`, in order,
    /// then lend their frames back with one submit; wait until `deadline`
    /// (`None`: for ever) for the first. Returns how many were handed over.
    ///
    /// # Errors
    /// As [`Self::next`].
    pub(crate) fn next_batch(
        &mut self,
        deadline: Option<Instant>,
        max: u32,
        mut take: impl FnMut(&[u8]),
    ) -> Result<u32, XdpError> {
        self.wait(deadline)?;
        let Socket { rx, umem, fill, .. } = &mut self.sock;
        let rx = rx.as_mut().ok_or(XdpError::InvalidConfig("no RX ring"))?;
        let n = rx.ready().min(max.max(1));
        for i in 0..n {
            let desc = rx.peek_nth(i);
            if let Some(frame) = umem.frame(desc.addr, desc.len as usize) {
                take(frame);
            }
        }
        // Every frame the kernel does not hold was in the fill ring's
        // count, so there is room for these.
        let room = fill.free(n);
        for i in 0..room {
            fill.push(rx.peek_nth(i).addr & !u64::from(FRAME_SIZE - 1));
        }
        rx.release_n(n);
        fill.submit();
        Ok(n)
    }

    /// Hand the next packet the kernel delivered to `take`, then lend its
    /// frame back; wait until `deadline` (`None`: for ever) for one.
    ///
    /// # Errors
    /// `TimedOut` when none came in time; a failed `poll`, or the socket's
    /// error, an interface removed under it included.
    pub(crate) fn next<R>(
        &mut self,
        deadline: Option<Instant>,
        take: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, XdpError> {
        self.wait(deadline)?;
        {
            let rx = self
                .sock
                .rx
                .as_mut()
                .ok_or(XdpError::InvalidConfig("no RX ring"))?;
            {
                let desc = rx.peek();
                rx.release();
                let result = self
                    .sock
                    .umem
                    .frame(desc.addr, desc.len as usize)
                    .map(take)
                    .ok_or(XdpError::InvalidConfig("a descriptor outside the UMEM"));
                // The frame goes back to the kernel whatever it held; there
                // is room, since every frame the kernel does not hold was in
                // the fill ring's count.
                let base = desc.addr & !u64::from(FRAME_SIZE - 1);
                if self.sock.fill.free(1) == 1 {
                    self.sock.fill.push(base);
                    self.sock.fill.submit();
                }
                result
            }
        }
    }
}

/// The send path: a socket whose frames this side owns until it sends
/// them, and the kernel's again until the completion ring returns them.
pub(crate) struct TxPath {
    pub(crate) sock: Socket,
    free: Vec<u64>,
}

impl TxPath {
    /// Open the send path on queue `queue` of `ifindex`.
    pub(crate) fn open(
        ifindex: u32,
        queue: u32,
        frames: u32,
        ring: u32,
        zero_copy: ZeroCopy,
    ) -> Result<Self, XdpError> {
        // Auto copies: some drivers (Intel's) build the rings a zero-copy
        // send needs only once an XDP program is attached to the
        // interface, which a sender does not attach.
        let zero_copy = match zero_copy {
            ZeroCopy::Auto => ZeroCopy::Off,
            other => other,
        };
        let sock = Socket::open(ifindex, queue, frames, ring, false, true, zero_copy)?;
        let free = (0..frames)
            .rev()
            .map(|i| u64::from(i) * u64::from(FRAME_SIZE))
            .collect();
        Ok(Self { sock, free })
    }

    /// Take back the frames the kernel has sent.
    fn reclaim(&mut self) {
        while self.sock.comp.ready() > 0 {
            self.free.push(self.sock.comp.peek());
            self.sock.comp.release();
        }
    }

    /// Send one packet that `write` writes into a frame of `FRAME_SIZE`
    /// bytes, returning its length; wait up to `timeout` for a frame and a
    /// TX ring entry to be free.
    ///
    /// # Errors
    /// [`XdpError::TxFull`] when none came free in time; a failed wakeup,
    /// an interface gone down included.
    pub(crate) fn send(
        &mut self,
        timeout: Duration,
        write: impl FnOnce(&mut [u8]) -> usize,
    ) -> Result<(), XdpError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.reclaim();
            let tx = self
                .sock
                .tx
                .as_mut()
                .ok_or(XdpError::InvalidConfig("no TX ring"))?;
            if !self.free.is_empty() && tx.free(1) == 1 {
                break;
            }
            // Out of frames or ring entries: the kernel sends what it holds
            // when woken, and returns the frames.
            sys::kick_tx(&self.sock.fd).map_err(io("wake the kernel to send"))?;
            if Instant::now() >= deadline {
                return Err(XdpError::TxFull);
            }
            std::thread::yield_now();
        }
        let addr = self.free.pop().ok_or(XdpError::TxFull)?;
        let frame = self
            .sock
            .umem
            .frame_mut(addr, FRAME_SIZE as usize)
            .ok_or(XdpError::InvalidConfig("a frame outside the UMEM"))?;
        let len = write(frame);
        let tx = self
            .sock
            .tx
            .as_mut()
            .ok_or(XdpError::InvalidConfig("no TX ring"))?;
        tx.push(libc::xdp_desc {
            addr,
            len: len as u32,
            options: 0,
        });
        tx.submit();
        if tx.needs_wakeup() {
            sys::kick_tx(&self.sock.fd).map_err(io("wake the kernel to send"))?;
        }
        Ok(())
    }

    /// Send `n` packets with one submit and one wakeup per run of free
    /// frames: `write(i, frame)` writes the `i`-th into a frame of
    /// `FRAME_SIZE` bytes and returns its length (0 to skip it). Waits up to
    /// `timeout` for frames and ring entries to come free.
    ///
    /// # Errors
    /// [`XdpError::TxFull`] when none came free in time (the packets before
    /// are handed over); a failed wakeup.
    pub(crate) fn send_batch(
        &mut self,
        timeout: Duration,
        n: usize,
        mut write: impl FnMut(usize, &mut [u8]) -> usize,
    ) -> Result<(), XdpError> {
        let deadline = Instant::now() + timeout;
        let mut done = 0;
        while done < n {
            self.reclaim();
            let Socket { tx, umem, fd, .. } = &mut self.sock;
            let tx = tx.as_mut().ok_or(XdpError::InvalidConfig("no TX ring"))?;
            let want = u32::try_from(n - done).unwrap_or(u32::MAX);
            let room = tx
                .free(want)
                .min(u32::try_from(self.free.len()).unwrap_or(u32::MAX));
            if room == 0 {
                sys::kick_tx(fd).map_err(io("wake the kernel to send"))?;
                if Instant::now() >= deadline {
                    return Err(XdpError::TxFull);
                }
                std::thread::yield_now();
                continue;
            }
            for _ in 0..room {
                let addr = self.free.pop().ok_or(XdpError::TxFull)?;
                let frame = umem
                    .frame_mut(addr, FRAME_SIZE as usize)
                    .ok_or(XdpError::InvalidConfig("a frame outside the UMEM"))?;
                let len = write(done, frame);
                done += 1;
                if len == 0 {
                    self.free.push(addr);
                    continue;
                }
                tx.push(libc::xdp_desc {
                    addr,
                    len: len as u32,
                    options: 0,
                });
            }
            tx.submit();
            if tx.needs_wakeup() {
                sys::kick_tx(fd).map_err(io("wake the kernel to send"))?;
            }
        }
        Ok(())
    }

    /// Wake the kernel until it has taken everything in the TX ring, or
    /// `timeout` passes: a wakeup the kernel answered busy leaves frames
    /// waiting for the next send otherwise.
    ///
    /// # Errors
    /// [`XdpError::TxFull`] when frames are still waiting at the timeout; a
    /// failed wakeup.
    pub(crate) fn flush(&mut self, timeout: Duration) -> Result<(), XdpError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.reclaim();
            let tx = self
                .sock
                .tx
                .as_mut()
                .ok_or(XdpError::InvalidConfig("no TX ring"))?;
            if tx.pending() == 0 {
                return Ok(());
            }
            sys::kick_tx(&self.sock.fd).map_err(io("wake the kernel to send"))?;
            if Instant::now() >= deadline {
                return Err(XdpError::TxFull);
            }
            std::thread::yield_now();
        }
    }
}
