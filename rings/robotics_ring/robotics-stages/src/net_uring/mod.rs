// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! A multi-host transport over io_uring, Linux only: kernel-side submission
//! polling (`IORING_SETUP_SQPOLL`) keeps steady-state sends free of syscalls
//! where the kernel allows it, and a pool of persistent TCP flows spares each
//! send to a peer a new connection's warm-up.
//!
//! [`UringSpsc`] is one UDP endpoint on one io_uring instance.
//! `UringSpsc::bind_udp` binds its socket and registers it with the ring,
//! `UringSpsc::connect_udp` fixes its peer, and `UringSpsc::send`,
//! `UringSpsc::send_batch`, `UringSpsc::recv` and
//! `UringSpsc::recv_timeout` each submit their operations and wait for
//! their completions, so no caller's buffer is borrowed past the call. A
//! batch is many sends in flight on one submission. [`FlowPool`] keeps one
//! TCP flow per peer, closing the least recently used beyond its cap and
//! the idle on request; it works on every Unix. Both move the bytes their
//! caller hands them and frame nothing; for Pulse frames past the kernel's
//! stack, see the `pulse-wire-xdp` plugin in `plugins/hft-xdp`.

// Off Linux the endpoint is a stub, which leaves items unused.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Errors raised by [`UringSpsc`] / [`FlowPool`] operations.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum NetUringError {
    /// A socket call or an io_uring operation failed; a receive that waited
    /// its whole timeout is `TimedOut`.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The io_uring instance could not be created; the string says why.
    #[error("io_uring setup failed: {0}")]
    Setup(String),
    /// A send or receive before `UringSpsc::bind_udp` (`"bound"`), or a
    /// send before `UringSpsc::connect_udp` (`"connected"`).
    #[error("the endpoint is not {0}")]
    NotReady(&'static str),
    /// A payload longer than [`UringConfig::buffer_bytes`]. Nothing was sent.
    #[error("payload too large: {got} bytes > {limit}")]
    PayloadTooLarge {
        /// The payload's length.
        got: usize,
        /// [`UringConfig::buffer_bytes`].
        limit: usize,
    },
    /// A batch of more sends than [`UringConfig::buffer_count`], or than the
    /// submission queue holds. Nothing was sent.
    #[error("batch too large: {got} sends > {limit}")]
    BatchTooLarge {
        /// Sends in the batch.
        got: usize,
        /// The most one batch takes.
        limit: usize,
    },
    /// A thread panicked while holding [`FlowPool`]'s flow table, which may
    /// be inconsistent: rebuild the pool rather than retry.
    #[error("flow pool mutex poisoned (another thread panicked while holding it)")]
    MutexPoisoned,
}

/// Configuration for a [`UringSpsc`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UringConfig {
    /// Submission queue entries, rounded up to a power of two within
    /// `16..=32768`.
    pub sq_size: u32,
    /// Completion queue entries, rounded up to a power of two within
    /// `sq_size..=32768`; the default is twice `sq_size`.
    pub cq_size: u32,
    /// How long the kernel's submission-polling thread stays awake without
    /// work (default 2 ms): longer spares sends a wake-up syscall, at the
    /// cost of kernel CPU.
    pub sq_thread_idle: Duration,
    /// The largest payload one send takes, in bytes (default 64 KiB, about
    /// a UDP datagram's limit).
    pub buffer_bytes: usize,
    /// The most sends one `UringSpsc::send_batch` keeps in flight
    /// (default 256), and never more than the submission queue holds.
    pub buffer_count: usize,
}

impl Default for UringConfig {
    fn default() -> Self {
        Self {
            sq_size: 1024,
            cq_size: 2048,
            sq_thread_idle: Duration::from_millis(2),
            buffer_bytes: 65536,
            buffer_count: 256,
        }
    }
}

/// A single-producer, single-consumer UDP endpoint over an io_uring
/// instance.
#[cfg(target_os = "linux")]
pub struct UringSpsc {
    cfg: UringConfig,
    /// Dropped before the socket, which it holds registered.
    ring: io_uring::IoUring,
    sq_size: usize,
    sqpoll: bool,
    socket: Option<std::net::UdpSocket>,
    connected: bool,
}

/// The io_uring endpoint's stub off Linux, where [`UringSpsc::new`] always
/// fails.
#[cfg(not(target_os = "linux"))]
pub struct UringSpsc {
    cfg: UringConfig,
}

/// The socket's slot in the ring's registered files.
#[cfg(target_os = "linux")]
const SOCKET: io_uring::types::Fixed = io_uring::types::Fixed(0);

#[cfg(target_os = "linux")]
impl UringSpsc {
    /// Open an io_uring instance with kernel-side submission polling, its
    /// queue sizes rounded as [`UringConfig`] says. A kernel that refuses
    /// polling to an unprivileged process (before 5.11) gets an instance
    /// without it ([`Self::uses_sqpoll`]).
    ///
    /// # Errors
    ///
    /// [`NetUringError::Setup`] when the kernel refuses the io_uring instance,
    /// for example a kernel without io_uring or a sandbox that blocks it.
    pub fn new(cfg: UringConfig) -> Result<Self, NetUringError> {
        let sq_size = cfg.sq_size.next_power_of_two().clamp(16, 32768);
        let cq_size = cfg.cq_size.next_power_of_two().clamp(sq_size, 32768);
        let idle_ms = u32::try_from(cfg.sq_thread_idle.as_millis()).unwrap_or(u32::MAX);
        let build = |sqpoll: bool| {
            let mut builder = io_uring::IoUring::builder();
            if sqpoll {
                builder.setup_sqpoll(idle_ms);
            }
            builder.setup_cqsize(cq_size).build(sq_size)
        };
        let setup = |e: std::io::Error, sqpoll: bool| {
            NetUringError::Setup(format!(
                "IoUring::build(sq={sq_size}, cq={cq_size}, sqpoll={sqpoll}, idle={:?}): {e}",
                cfg.sq_thread_idle
            ))
        };
        let (ring, sqpoll) = match build(true) {
            Ok(ring) => (ring, true),
            Err(e) if e.raw_os_error() == Some(libc::EPERM) => {
                (build(false).map_err(|e| setup(e, false))?, false)
            }
            Err(e) => return Err(setup(e, true)),
        };
        tracing::info!(
            sq_size, cq_size, sqpoll, sq_thread_idle = ?cfg.sq_thread_idle,
            "net_uring instance created"
        );
        Ok(Self {
            cfg,
            ring,
            sq_size: sq_size as usize,
            sqpoll,
            socket: None,
            connected: false,
        })
    }

    /// Whether the kernel polls the submission queue, so a submission needs
    /// no syscall while its thread is awake.
    pub fn uses_sqpoll(&self) -> bool {
        self.sqpoll
    }

    /// Bind the endpoint's UDP socket to `addr` and register it with the
    /// ring, replacing any socket bound before; returns the address bound
    /// (port 0 picks a free one).
    ///
    /// # Errors
    ///
    /// [`NetUringError::Io`] when the socket cannot be bound or registered.
    pub fn bind_udp(&mut self, addr: SocketAddr) -> Result<SocketAddr, NetUringError> {
        let socket = std::net::UdpSocket::bind(addr)?;
        let local = socket.local_addr()?;
        if self.socket.take().is_some() {
            self.ring.submitter().unregister_files()?;
        }
        self.ring
            .submitter()
            .register_files(&[socket.as_raw_fd()])?;
        self.socket = Some(socket);
        self.connected = false;
        Ok(local)
    }

    /// The address the socket is bound to, once bound.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.socket.as_ref()?.local_addr().ok()
    }

    /// Send every datagram to `peer`, and receive only from it.
    ///
    /// # Errors
    ///
    /// [`NetUringError::NotReady`] before [`Self::bind_udp`];
    /// [`NetUringError::Io`] from `connect(2)`.
    pub fn connect_udp(&mut self, peer: SocketAddr) -> Result<(), NetUringError> {
        self.socket
            .as_ref()
            .ok_or(NetUringError::NotReady("bound"))?
            .connect(peer)?;
        self.connected = true;
        Ok(())
    }

    /// Send one payload as one datagram to the connected peer.
    ///
    /// # Errors
    ///
    /// Those of [`Self::send_batch`].
    pub fn send(&mut self, payload: &[u8]) -> Result<(), NetUringError> {
        self.send_batch(&[payload]).map(drop)
    }

    /// Send each payload as one datagram to the connected peer, all in
    /// flight on one submission, and wait for every completion; returns the
    /// number sent, all of them.
    ///
    /// # Errors
    ///
    /// [`NetUringError::NotReady`] before [`Self::bind_udp`] or
    /// [`Self::connect_udp`]; [`NetUringError::PayloadTooLarge`] and
    /// [`NetUringError::BatchTooLarge`] before anything is sent; otherwise
    /// [`NetUringError::Io`] with the first failed send's error, the others
    /// having completed, sent or failed.
    pub fn send_batch(&mut self, payloads: &[&[u8]]) -> Result<usize, NetUringError> {
        if self.socket.is_none() {
            return Err(NetUringError::NotReady("bound"));
        }
        if !self.connected {
            return Err(NetUringError::NotReady("connected"));
        }
        let limit = self.cfg.buffer_count.min(self.sq_size);
        if payloads.len() > limit {
            return Err(NetUringError::BatchTooLarge {
                got: payloads.len(),
                limit,
            });
        }
        let max = self.cfg.buffer_bytes;
        if let Some(p) = payloads
            .iter()
            .find(|p| p.len() > max || u32::try_from(p.len()).is_err())
        {
            return Err(NetUringError::PayloadTooLarge {
                got: p.len(),
                limit: max,
            });
        }
        let entries: Vec<io_uring::squeue::Entry> = payloads
            .iter()
            .enumerate()
            .map(|(i, p)| {
                // `len` fits: checked against `u32` above.
                io_uring::opcode::Send::new(SOCKET, p.as_ptr(), p.len() as u32)
                    .build()
                    .user_data(i as u64)
            })
            .collect();
        // SAFETY: every entry reads a payload the caller lends for this call,
        // and the call returns only after `complete` has reaped every one of
        // their completions, so no buffer is read after it is given back.
        // The socket they name is registered and outlives the ring's use.
        unsafe { self.ring.submission().push_multiple(&entries) }.map_err(|_| queue_full())?;
        let results = self.complete(entries.len())?;
        match results.iter().find(|(_, res)| *res < 0) {
            Some((_, res)) => Err(std::io::Error::from_raw_os_error(-res).into()),
            None => Ok(payloads.len()),
        }
    }

    /// Receive one datagram into `out`, waiting as long as it takes;
    /// returns its length, cut to `out`'s.
    ///
    /// # Errors
    ///
    /// [`NetUringError::NotReady`] before [`Self::bind_udp`];
    /// [`NetUringError::Io`] when the receive fails.
    pub fn recv(&mut self, out: &mut [u8]) -> Result<usize, NetUringError> {
        if self.socket.is_none() {
            return Err(NetUringError::NotReady("bound"));
        }
        let entry = recv_entry(out).user_data(0);
        // SAFETY: the receive writes into `out`, which the caller lends for
        // this call, and the call returns only after its completion is
        // reaped.
        unsafe { self.ring.submission().push(&entry) }.map_err(|_| queue_full())?;
        let results = self.complete(1)?;
        let res = results.first().map_or(-libc::EIO, |&(_, res)| res);
        if res < 0 {
            return Err(std::io::Error::from_raw_os_error(-res).into());
        }
        Ok(res as usize)
    }

    /// [`Self::recv`], waiting at most `timeout` (a timeout linked to the
    /// receive in the ring).
    ///
    /// # Errors
    ///
    /// Those of [`Self::recv`], and [`NetUringError::Io`] with `TimedOut`
    /// when no datagram came in time.
    pub fn recv_timeout(
        &mut self,
        out: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, NetUringError> {
        if self.socket.is_none() {
            return Err(NetUringError::NotReady("bound"));
        }
        let deadline = io_uring::types::Timespec::from(timeout);
        let entries = [
            recv_entry(out)
                .flags(io_uring::squeue::Flags::IO_LINK)
                .user_data(0),
            io_uring::opcode::LinkTimeout::new(&deadline)
                .build()
                .user_data(1),
        ];
        // SAFETY: the receive writes into `out` and the timeout reads
        // `deadline`, both alive for this call, which returns only after
        // both completions are reaped.
        unsafe { self.ring.submission().push_multiple(&entries) }.map_err(|_| queue_full())?;
        let results = self.complete(2)?;
        let res = results
            .iter()
            .find(|(data, _)| *data == 0)
            .map_or(-libc::EIO, |&(_, res)| res);
        match res {
            n if n >= 0 => Ok(n as usize),
            e if e == -libc::ECANCELED => {
                Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into())
            }
            e => Err(std::io::Error::from_raw_os_error(-e).into()),
        }
    }

    /// Submit what is queued and wait until `want` completions are reaped;
    /// returns each one's user data and result.
    fn complete(&mut self, want: usize) -> Result<Vec<(u64, i32)>, NetUringError> {
        let mut results = Vec::with_capacity(want);
        while results.len() < want {
            match self.ring.submit_and_wait(want - results.len()) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
            results.extend(self.ring.completion().map(|c| (c.user_data(), c.result())));
        }
        Ok(results)
    }

    /// The io_uring instance, for callers that submit operations of their own.
    pub fn ring(&self) -> &io_uring::IoUring {
        &self.ring
    }

    /// Returns the configuration this instance was constructed with.
    pub fn config(&self) -> &UringConfig {
        &self.cfg
    }
}

/// A submission queue with no room: every call reaps what it submits, so
/// only another user of `UringSpsc::ring` can leave it full.
#[cfg(target_os = "linux")]
fn queue_full() -> NetUringError {
    std::io::Error::other("the io_uring submission queue is full").into()
}

/// A receive into `out` on the registered socket.
#[cfg(target_os = "linux")]
fn recv_entry(out: &mut [u8]) -> io_uring::squeue::Entry {
    let len = u32::try_from(out.len()).unwrap_or(u32::MAX);
    io_uring::opcode::Recv::new(SOCKET, out.as_mut_ptr(), len).build()
}

#[cfg(not(target_os = "linux"))]
impl UringSpsc {
    /// Fails: io_uring exists only on Linux.
    ///
    /// # Errors
    ///
    /// Always [`NetUringError::Setup`]; gate the call on
    /// `#[cfg(target_os = "linux")]` rather than rely on it.
    pub fn new(cfg: UringConfig) -> Result<Self, NetUringError> {
        let _ = cfg;
        Err(NetUringError::Setup("io_uring requires Linux".into()))
    }
    /// The configuration, kept for parity with the Linux type; nothing
    /// constructs the stub.
    pub fn config(&self) -> &UringConfig {
        &self.cfg
    }
}

/// Configuration for a [`FlowPool`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FlowPoolConfig {
    /// The most flows open at once; beyond it the least recently used closes.
    pub max_flows: usize,
    /// Idle time after which [`FlowPool::reap_idle`] closes a flow (default
    /// 30 s).
    pub idle_timeout: Duration,
    /// Set `TCP_NODELAY` on every flow (default true); false suits bulk
    /// transfers, where coalescing helps throughput.
    pub tcp_nodelay: bool,
    /// Set `SO_REUSEADDR` and `SO_REUSEPORT` on every flow's socket before
    /// it connects.
    pub reuse_port: bool,
}

impl Default for FlowPoolConfig {
    fn default() -> Self {
        Self {
            max_flows: 256,
            idle_timeout: Duration::from_secs(30),
            tcp_nodelay: true,
            reuse_port: false,
        }
    }
}

/// One persistent flow to a peer.
pub struct PersistentFlow {
    /// The peer's address.
    pub dst: SocketAddr,
    last_used: Instant,
    /// The connected TCP stream, closed when the flow drops.
    stream: TcpStream,
}

impl PersistentFlow {
    /// When the flow was last used.
    pub fn last_used(&self) -> Instant {
        self.last_used
    }
    /// Mark the flow used now.
    pub fn touch(&mut self) {
        self.last_used = Instant::now();
    }

    /// The flow's socket descriptor.
    pub fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

/// A pool of persistent TCP flows, one per peer, reused across publishes so
/// that no send pays a new connection's warm-up (the handshake, and slow
/// start's ramp of the congestion window).
pub struct FlowPool {
    cfg: FlowPoolConfig,
    flows: Mutex<HashMap<SocketAddr, PersistentFlow>>,
}

impl FlowPool {
    /// An empty pool.
    pub fn new(cfg: FlowPoolConfig) -> Self {
        Self {
            cfg,
            flows: Mutex::new(HashMap::new()),
        }
    }

    /// The descriptor of the pool's flow to `dst`, opening one when there
    /// is none and closing the least recently used flow when the pool is
    /// full; the flow is marked used. The pool keeps the descriptor open
    /// until the flow is closed. The connect runs without the pool's lock,
    /// so a slow peer holds up no other; two callers that race to open the
    /// same peer's flow keep the first.
    ///
    /// # Errors
    ///
    /// [`NetUringError::Io`] when the connection cannot be made;
    /// [`NetUringError::MutexPoisoned`].
    pub fn acquire(&self, dst: SocketAddr) -> Result<RawFd, NetUringError> {
        if let Some(flow) = self.lock()?.get_mut(&dst) {
            flow.touch();
            return Ok(flow.raw_fd());
        }
        let stream = self.connect(dst)?;
        let mut flows = self.lock()?;
        if !flows.contains_key(&dst) && flows.len() >= self.cfg.max_flows.max(1) {
            let oldest = flows
                .iter()
                .min_by_key(|(_, f)| f.last_used)
                .map(|(addr, _)| *addr);
            if let Some(addr) = oldest {
                flows.remove(&addr);
            }
        }
        let flow = flows.entry(dst).or_insert_with(|| PersistentFlow {
            dst,
            last_used: Instant::now(),
            stream,
        });
        flow.touch();
        Ok(flow.raw_fd())
    }

    /// A connected TCP stream to `dst`, with the pool's socket options.
    fn connect(&self, dst: SocketAddr) -> std::io::Result<TcpStream> {
        let socket = socket2::Socket::new(
            socket2::Domain::for_address(dst),
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        if self.cfg.reuse_port {
            socket.set_reuse_address(true)?;
            socket.set_reuse_port(true)?;
        }
        socket.connect(&dst.into())?;
        socket.set_nodelay(self.cfg.tcp_nodelay)?;
        Ok(socket.into())
    }

    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, HashMap<SocketAddr, PersistentFlow>>, NetUringError> {
        self.flows.lock().map_err(|_| NetUringError::MutexPoisoned)
    }

    /// Close the flows idle longer than [`FlowPoolConfig::idle_timeout`] and
    /// return how many; call it periodically.
    ///
    /// # Errors
    ///
    /// [`NetUringError::MutexPoisoned`].
    pub fn reap_idle(&self) -> Result<usize, NetUringError> {
        let mut flows = self.lock()?;
        let before = flows.len();
        let now = Instant::now();
        flows.retain(|_, f| now.duration_since(f.last_used) <= self.cfg.idle_timeout);
        Ok(before - flows.len())
    }

    /// Returns the configuration this pool was constructed with.
    pub fn config(&self) -> &FlowPoolConfig {
        &self.cfg
    }
    /// The number of open flows.
    ///
    /// # Errors
    ///
    /// [`NetUringError::MutexPoisoned`].
    pub fn active_flow_count(&self) -> Result<usize, NetUringError> {
        Ok(self.lock()?.len())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod flow_pool_tests {
    use super::*;
    use std::net::TcpListener;

    fn listener() -> (TcpListener, SocketAddr) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        (l, addr)
    }

    #[test]
    fn flow_pool_default_config_sane() {
        let cfg = FlowPoolConfig::default();
        assert_eq!(cfg.max_flows, 256);
        assert_eq!(cfg.idle_timeout, Duration::from_secs(30));
        assert!(cfg.tcp_nodelay);
    }

    #[test]
    fn flow_pool_starts_empty() {
        let pool = FlowPool::new(FlowPoolConfig::default());
        assert_eq!(pool.active_flow_count().unwrap(), 0);
        assert_eq!(pool.reap_idle().unwrap(), 0);
    }

    #[test]
    fn a_flow_is_opened_once_and_reused() {
        let (l, addr) = listener();
        let pool = FlowPool::new(FlowPoolConfig::default());
        let fd = pool.acquire(addr).unwrap();
        assert_eq!(pool.acquire(addr).unwrap(), fd, "the same flow");
        assert_eq!(pool.active_flow_count().unwrap(), 1);
        // The peer saw one connection. A connect can return before the peer
        // has queued the connection for accept (macOS on loopback does), so
        // the first accept waits for it, and the check for a second gives
        // one the same time to show up.
        l.set_nonblocking(true).unwrap();
        let accepted = |within: Duration| {
            let deadline = Instant::now() + within;
            loop {
                match l.accept() {
                    Ok(_) => return true,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => return false,
                }
            }
        };
        assert!(
            accepted(Duration::from_secs(5)),
            "the flow never reached the peer"
        );
        assert!(
            !accepted(Duration::from_millis(100)),
            "a second connection reached the peer"
        );
    }

    #[test]
    fn a_full_pool_closes_its_least_recently_used_flow() {
        let (_a, a) = listener();
        let (_b, b) = listener();
        let (_c, c) = listener();
        let pool = FlowPool::new(FlowPoolConfig {
            max_flows: 2,
            ..Default::default()
        });
        pool.acquire(a).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        pool.acquire(b).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        pool.acquire(a).unwrap(); // a is now the most recently used
        std::thread::sleep(Duration::from_millis(2));
        pool.acquire(c).unwrap();
        assert_eq!(pool.active_flow_count().unwrap(), 2);
        let flows = pool.flows.lock().unwrap();
        assert!(
            flows.contains_key(&a) && flows.contains_key(&c),
            "b was the least recently used"
        );
    }

    #[test]
    fn idle_flows_are_reaped() {
        let (_l, addr) = listener();
        let pool = FlowPool::new(FlowPoolConfig {
            idle_timeout: Duration::from_millis(1),
            ..Default::default()
        });
        pool.acquire(addr).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(pool.reap_idle().unwrap(), 1);
        assert_eq!(pool.active_flow_count().unwrap(), 0);
    }

    #[test]
    fn a_peer_that_refuses_is_an_io_error() {
        let (l, addr) = listener();
        drop(l);
        let pool = FlowPool::new(FlowPoolConfig::default());
        assert!(matches!(pool.acquire(addr), Err(NetUringError::Io(_))));
        assert_eq!(pool.active_flow_count().unwrap(), 0);
    }
}

#[cfg(all(test, target_os = "linux"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]
mod tests {
    use super::*;

    /// An endpoint, or `None` where the kernel or a sandbox refuses
    /// io_uring, which the test then skips.
    fn endpoint() -> Option<UringSpsc> {
        match UringSpsc::new(UringConfig::default()) {
            Ok(e) => Some(e),
            Err(NetUringError::Setup(msg)) => {
                eprintln!("io_uring setup failed (sandbox?), skipping: {msg}");
                None
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    /// Two endpoints on loopback, each connected to the other.
    fn pair() -> Option<(UringSpsc, UringSpsc)> {
        let (mut a, mut b) = (endpoint()?, endpoint()?);
        let any: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let a_addr = a.bind_udp(any).unwrap();
        let b_addr = b.bind_udp(any).unwrap();
        a.connect_udp(b_addr).unwrap();
        b.connect_udp(a_addr).unwrap();
        Some((a, b))
    }

    #[test]
    fn default_config_creates_ring_or_skips() {
        let Some(spsc) = endpoint() else { return };
        let cfg = spsc.config();
        assert!(cfg.sq_size >= 16);
        assert!(cfg.cq_size >= cfg.sq_size);
    }

    #[test]
    fn datagrams_cross_loopback_one_at_a_time() {
        let Some((mut a, mut b)) = pair() else { return };
        let mut buf = [0u8; 2048];
        for i in 0..200u32 {
            let payload = i.to_le_bytes().repeat(1 + (i as usize % 64));
            a.send(&payload).unwrap();
            let n = b.recv_timeout(&mut buf, Duration::from_secs(5)).unwrap();
            assert_eq!(&buf[..n], &payload[..]);
        }
        b.send(b"back").unwrap();
        let n = a.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"back");
    }

    #[test]
    fn a_batch_goes_out_on_one_submission() {
        let Some((mut a, mut b)) = pair() else { return };
        let payloads: Vec<Vec<u8>> = (0..64u8).map(|i| vec![i; 100 + usize::from(i)]).collect();
        let refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
        assert_eq!(a.send_batch(&refs).unwrap(), 64);
        let mut buf = [0u8; 2048];
        for p in &payloads {
            let n = b.recv_timeout(&mut buf, Duration::from_secs(5)).unwrap();
            assert_eq!(&buf[..n], &p[..]);
        }
    }

    #[test]
    fn a_receive_with_nothing_to_receive_times_out() {
        let Some((_a, mut b)) = pair() else { return };
        let mut buf = [0u8; 64];
        let started = Instant::now();
        match b.recv_timeout(&mut buf, Duration::from_millis(50)) {
            Err(NetUringError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::TimedOut),
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn sends_before_bind_or_connect_and_oversized_ones_are_refused() {
        let Some(mut e) = endpoint() else { return };
        assert!(matches!(
            e.send(b"x"),
            Err(NetUringError::NotReady("bound"))
        ));
        assert!(matches!(
            e.recv(&mut [0u8; 8]),
            Err(NetUringError::NotReady("bound"))
        ));
        e.bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        assert!(matches!(
            e.send(b"x"),
            Err(NetUringError::NotReady("connected"))
        ));
        e.connect_udp("127.0.0.1:9".parse().unwrap()).unwrap();
        let big = vec![0u8; e.config().buffer_bytes + 1];
        assert!(matches!(
            e.send(&big),
            Err(NetUringError::PayloadTooLarge { .. })
        ));
        let many: Vec<&[u8]> = vec![b"x"; e.config().buffer_count + 1];
        assert!(matches!(
            e.send_batch(&many),
            Err(NetUringError::BatchTooLarge { .. })
        ));
    }

    #[test]
    fn a_rebind_moves_the_registered_socket() {
        let Some((mut a, mut b)) = pair() else { return };
        let fresh = b.bind_udp("127.0.0.1:0".parse().unwrap()).unwrap();
        a.connect_udp(fresh).unwrap();
        b.connect_udp(a.local_addr().unwrap()).unwrap();
        a.send(b"after rebind").unwrap();
        let mut buf = [0u8; 64];
        let n = b.recv_timeout(&mut buf, Duration::from_secs(5)).unwrap();
        assert_eq!(&buf[..n], b"after rebind");
    }
}
