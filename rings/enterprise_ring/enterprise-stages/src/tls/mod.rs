// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! TLS 1.3 and DTLS encapsulation of the wire transport on port 443, so that
//! Pulse and envelope datagrams cross NATs, firewalls and middleboxes looking
//! like ordinary HTTPS.
//!
//! One of the three [`Mode`]s works in this release: [`Mode::TlsTcpStream`],
//! TLS 1.3 over TCP on rustls with its `ring` provider.
//! [`TlsSender::connect`] completes the handshake and verifies the server's
//! certificate against [`TlsConfig::trust_roots`] and the server name, and
//! [`TlsSender::send`] writes each frame as a 4-byte little-endian length and
//! then exactly the bytes [`pulse_wire_udp::encode_into`] produces.
//! [`TlsReceiver::bind`] listens, [`TlsReceiver::accept`] completes a
//! handshake with the configured certificate chain and key, and
//! [`TlsConnection::recv`] yields each frame as [`pulse_wire_udp::decode`]
//! decodes it, so one decoder serves tunnelled and raw UDP traffic alike.
//! The other two modes, [`Mode::Dtls`] and [`Mode::QuicTunnel`], return
//! [`TlsError::IntegrationDeferred`] from both entry points.
//!
//! TLS here authenticates the server only: the receiver asks for no client
//! certificate, so anything that reaches its address can connect and send
//! frames. A sealed session (ADR-0018) is what authenticates a peer node.
//! Only TLS 1.3 is offered and accepted.
//!
//! The modes trade latency against how many middleboxes let the traffic
//! through. Endpoints are meant to negotiate the best mode both support; in
//! this release each end is configured with one.

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![forbid(unsafe_code)]

use pulse_canon::Pulse;
use pulse_wire_udp::{
    datagram_size, decode, encode_into, Frame, WireError, HEADER_BYTES, MAX_ENVELOPE, TRAILER_BYTES,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, ConnectionCommon, DigitallySignedStruct, RootCertStore,
    ServerConfig, ServerConnection, SideData, SignatureScheme, StreamOwned,
};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Bytes of the length written before each frame on a
/// [`Mode::TlsTcpStream`] connection: a little-endian `u32`.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// The longest frame a [`TlsConnection`] accepts, the length of a datagram
/// carrying an envelope of [`MAX_ENVELOPE`] bytes; a longer length prefix is
/// refused before anything more is read.
pub const MAX_FRAME_BYTES: usize = HEADER_BYTES + MAX_ENVELOPE + TRAILER_BYTES;

/// The shortest frame there is: one with no envelope.
const MIN_FRAME_BYTES: usize = HEADER_BYTES + TRAILER_BYTES;

/// [`TlsConfig::handshake_timeout`]'s default.
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long dropping a [`TlsSender`] may wait to write its `close_notify`.
const CLOSE_DEADLINE: Duration = Duration::from_secs(1);

/// Errors from [`TlsSender`], [`TlsReceiver`] and [`TlsConnection`].
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum TlsError {
    /// The mode asked for is not integrated in this release: DTLS
    /// ([`Mode::Dtls`]) and the QUIC tunnel ([`Mode::QuicTunnel`]).
    /// [`Mode::TlsTcpStream`] is.
    #[error(
        "the DTLS and QUIC-tunnel modes are not integrated in this release; \
         Mode::TlsTcpStream is"
    )]
    IntegrationDeferred,

    /// The host operating system or this build doesn't support the
    /// requested mode.
    #[error("TLS mode {0:?} is unavailable on this build / OS. Try a different mode.")]
    ModeUnavailable(Mode),

    /// A socket operation failed: resolving or connecting to the target,
    /// binding, accepting, reading or writing. A handshake that takes longer
    /// than [`TlsConfig::handshake_timeout`] ends here, with
    /// [`io::ErrorKind::TimedOut`].
    #[error("TLS stage socket: {0}")]
    Io(#[source] io::Error),

    /// TLS refused the connection: the server's certificate is not trusted or
    /// does not name the server, the peer broke off the handshake (an alert),
    /// or the receiver's certificate and key do not go together.
    #[error("TLS: {0}")]
    Tls(#[source] rustls::Error),

    /// A PEM field of the [`TlsConfig`] holds nothing usable; `field` names
    /// it.
    #[error("TlsConfig::{field}: {reason}")]
    Pem {
        /// The field: `trust_roots`, `cert_chain` or `private_key`.
        field: &'static str,
        /// What is wrong with it.
        reason: String,
    },

    /// The server name to verify, from [`TlsConfig::sni`] or the host of the
    /// target, is neither a DNS name nor an IP address.
    #[error("{0:?} is not a server name TLS can verify: a DNS name or an IP address")]
    InvalidServerName(String),

    /// A frame's length prefix is outside the lengths a frame can have, so
    /// the stream cannot be read on; the connection is not usable after it.
    #[error("frame length {len} on the TLS stream is outside {min}..={max}")]
    FrameLength {
        /// The length the prefix gave.
        len: usize,
        /// The shortest frame, one with no envelope.
        min: usize,
        /// [`MAX_FRAME_BYTES`].
        max: usize,
    },

    /// A frame decoded, but it is shorter than its length prefix said.
    #[error("a frame of {frame} bytes came under a length prefix of {prefix}")]
    LengthMismatch {
        /// The length the prefix gave.
        prefix: usize,
        /// The length of the frame it held.
        frame: usize,
    },

    /// A frame could not be encoded (its envelope is over [`MAX_ENVELOPE`])
    /// or decoded (see [`WireError`]).
    #[error("wire frame: {0}")]
    Wire(#[from] WireError),
}

/// How the tunnel carries the traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// UDP/443 with DTLS 1.3: datagram semantics and the best latency, but
    /// some firewalls block UDP/443. Not integrated in this release.
    Dtls,
    /// UDP/443 with QUIC carrying the traffic as a stream: passes where
    /// HTTP/3 does, at the cost of QUIC's congestion control and
    /// acknowledgements. Not integrated in this release.
    QuicTunnel,
    /// TCP/443 with TLS 1.3 carrying length-prefixed frames: passes nearly
    /// every NAT and firewall, at the cost of head-of-line blocking.
    TlsTcpStream,
}

/// Configuration for a tunnelled peer, built from its [`Default`] and set
/// field by field. A sender reads `mode`, `sni`, `trust_roots`,
/// `accept_self_signed` and `handshake_timeout`; a receiver reads `mode`,
/// `cert_chain`, `private_key` and `handshake_timeout`.
#[derive(Clone)]
#[non_exhaustive]
pub struct TlsConfig {
    /// Tunnel mode.
    pub mode: Mode,
    /// The server name the sender verifies the server's certificate against
    /// and indicates (SNI); `None` takes the host of the target given to
    /// [`TlsSender::connect`]. An IP address is checked against the
    /// certificate's IP addresses and is not sent as SNI, which carries DNS
    /// names only.
    pub sni: Option<String>,
    /// PEM-encoded root certificates the sender trusts, and the only ones it
    /// trusts. Empty, the default, trusts the Mozilla root program's roots
    /// as the `webpki-roots` crate carries them, as a browser would.
    pub trust_roots: Vec<u8>,
    /// Let the sender accept, beside a certificate that chains to
    /// `trust_roots`, a self-signed one: a certificate that verifies under
    /// its own key, names the server and is within its validity period.
    ///
    /// This disables authentication of the server. Anyone can make a
    /// self-signed certificate for any name, so whoever sits on the path can
    /// pose as the server and read and change everything sent. It is for
    /// loopback tests; the default is `false` and must stay so anywhere
    /// else.
    pub accept_self_signed: bool,
    /// The PEM-encoded certificate chain the receiver presents, its own
    /// certificate first.
    pub cert_chain: Vec<u8>,
    /// The PEM-encoded private key of `cert_chain`'s first certificate, for
    /// the receiver: PKCS #8, PKCS #1 (RSA) or SEC1 (EC). Left out of this
    /// configuration's `Debug` output.
    pub private_key: Vec<u8>,
    /// How long the sender's TCP connect may take, and the TLS handshake on
    /// either side, before it fails with a [`TlsError::Io`] of
    /// [`io::ErrorKind::TimedOut`]; zero waits as long as it takes. Default
    /// [`DEFAULT_HANDSHAKE_TIMEOUT`].
    pub handshake_timeout: Duration,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            mode: Mode::TlsTcpStream,
            sni: None,
            trust_roots: Vec::new(),
            accept_self_signed: false,
            cert_chain: Vec::new(),
            private_key: Vec::new(),
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
        }
    }
}

impl fmt::Debug for TlsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsConfig")
            .field("mode", &self.mode)
            .field("sni", &self.sni)
            .field(
                "trust_roots",
                &format_args!("{} bytes", self.trust_roots.len()),
            )
            .field("accept_self_signed", &self.accept_self_signed)
            .field(
                "cert_chain",
                &format_args!("{} bytes", self.cert_chain.len()),
            )
            .field(
                "private_key",
                &format_args!(
                    "{}",
                    if self.private_key.is_empty() {
                        "none"
                    } else {
                        "elided"
                    }
                ),
            )
            .field("handshake_timeout", &self.handshake_timeout)
            .finish()
    }
}

/// The sending half of a tunnelled connection: one TLS 1.3 connection to a
/// [`TlsReceiver`], carrying frames in order. Dropping it sends TLS's
/// `close_notify`, so the receiver can tell the end of the stream from a cut.
pub struct TlsSender {
    stream: StreamOwned<ClientConnection, TcpStream>,
    /// The length prefix and the frame being written, reused.
    frame: Vec<u8>,
    /// `close_notify` has been sent, or tried.
    closed: bool,
}

impl TlsSender {
    /// Connect to the peer at `target` (`host:port`) and complete the
    /// handshake `config.mode` names, verifying the server's certificate.
    ///
    /// # Errors
    ///
    /// [`TlsError::IntegrationDeferred`] for [`Mode::Dtls`] and
    /// [`Mode::QuicTunnel`]; [`TlsError::Pem`] for `trust_roots` that are
    /// not PEM certificates; [`TlsError::InvalidServerName`]; [`TlsError::Io`]
    /// when `target` does not resolve or connect, or the handshake times
    /// out; [`TlsError::Tls`] when the server's certificate is not trusted
    /// or does not name the server, or the handshake otherwise fails.
    pub fn connect(target: &str, config: &TlsConfig) -> Result<Self, TlsError> {
        if config.mode != Mode::TlsTcpStream {
            return Err(TlsError::IntegrationDeferred);
        }
        let name = server_name(config.sni.as_deref().unwrap_or_else(|| host_of(target)))?;
        let client = Arc::new(client_config(config)?);
        let tcp = connect_tcp(target, config.handshake_timeout)?;
        // Each frame is written whole and flushed, and waits for nothing.
        tcp.set_nodelay(true).map_err(TlsError::Io)?;
        let conn = ClientConnection::new(client, name).map_err(TlsError::Tls)?;
        let stream = handshake(conn, tcp, config.handshake_timeout)?;
        Ok(Self {
            stream,
            frame: Vec::with_capacity(LENGTH_PREFIX_BYTES + MAX_FRAME_BYTES),
            closed: false,
        })
    }

    /// Send one frame of `(seq, send_ns, pulse, envelope)`: its length, then
    /// the bytes [`encode_into`] makes of it, flushed to the socket. It
    /// blocks while the socket's send buffer is full.
    ///
    /// # Errors
    ///
    /// [`TlsError::Wire`] when `envelope` is longer than [`MAX_ENVELOPE`],
    /// with nothing sent; [`TlsError::Io`] or [`TlsError::Tls`] when the
    /// connection fails, after which it is not usable.
    pub fn send(
        &mut self,
        seq: u64,
        send_ns: u64,
        pulse: &Pulse,
        envelope: &[u8],
    ) -> Result<(), TlsError> {
        self.frame.clear();
        self.frame.extend_from_slice(&[0; LENGTH_PREFIX_BYTES]);
        encode_into(seq, send_ns, pulse, envelope, &mut self.frame)?;
        // At most MAX_FRAME_BYTES, which encode_into has just checked.
        let len = (self.frame.len() - LENGTH_PREFIX_BYTES) as u32;
        self.frame[..LENGTH_PREFIX_BYTES].copy_from_slice(&len.to_le_bytes());
        self.stream.write_all(&self.frame).map_err(from_io)?;
        self.stream.flush().map_err(from_io)
    }

    /// The receiver's address.
    ///
    /// # Errors
    ///
    /// [`TlsError::Io`] when the socket cannot say.
    pub fn peer_addr(&self) -> Result<SocketAddr, TlsError> {
        self.stream.sock.peer_addr().map_err(TlsError::Io)
    }

    /// End the stream: send `close_notify`, so the receiver's
    /// [`TlsConnection::recv`] returns `None`, and close the connection.
    /// Dropping the sender does the same, without reporting a failure.
    ///
    /// # Errors
    ///
    /// [`TlsError::Io`] or [`TlsError::Tls`] when `close_notify` could not be
    /// written.
    pub fn close(mut self) -> Result<(), TlsError> {
        self.close_notify()
    }

    fn close_notify(&mut self) -> Result<(), TlsError> {
        if std::mem::replace(&mut self.closed, true) {
            return Ok(());
        }
        self.stream.conn.send_close_notify();
        while self.stream.conn.wants_write() {
            self.stream
                .conn
                .write_tls(&mut self.stream.sock)
                .map_err(from_io)?;
        }
        self.stream.sock.flush().map_err(TlsError::Io)
    }
}

impl Drop for TlsSender {
    fn drop(&mut self) {
        // A receiver that has stopped reading must not hold the drop.
        let _ = self.stream.sock.set_write_timeout(Some(CLOSE_DEADLINE));
        let _ = self.close_notify();
    }
}

/// The receiving half of a tunnelled connection: a listener that completes a
/// TLS 1.3 handshake with each sender that connects.
pub struct TlsReceiver {
    listener: TcpListener,
    config: Arc<ServerConfig>,
    handshake_timeout: Duration,
}

impl TlsReceiver {
    /// Listen at `bind` (`host:port`; port 0 picks one, which
    /// [`TlsReceiver::local_addr`] reports) with the certificate chain and
    /// key `config` gives, for the handshake `config.mode` names.
    ///
    /// # Errors
    ///
    /// [`TlsError::IntegrationDeferred`] for [`Mode::Dtls`] and
    /// [`Mode::QuicTunnel`]; [`TlsError::Pem`] for a `cert_chain` or
    /// `private_key` that is missing or not PEM; [`TlsError::Tls`] when the
    /// key is not one rustls can sign with or is not the certificate's;
    /// [`TlsError::Io`] when the address cannot be bound.
    pub fn bind(bind: &str, config: &TlsConfig) -> Result<Self, TlsError> {
        if config.mode != Mode::TlsTcpStream {
            return Err(TlsError::IntegrationDeferred);
        }
        let server = Arc::new(server_config(config)?);
        let listener = TcpListener::bind(bind).map_err(TlsError::Io)?;
        Ok(Self {
            listener,
            config: server,
            handshake_timeout: config.handshake_timeout,
        })
    }

    /// The address the receiver listens on.
    ///
    /// # Errors
    ///
    /// [`TlsError::Io`] when the socket cannot say.
    pub fn local_addr(&self) -> Result<SocketAddr, TlsError> {
        self.listener.local_addr().map_err(TlsError::Io)
    }

    /// Wait for the next sender, and complete the TLS handshake with it
    /// within [`TlsConfig::handshake_timeout`].
    ///
    /// # Errors
    ///
    /// [`TlsError::Io`] when the accept fails or the handshake times out;
    /// [`TlsError::Tls`] when the handshake fails, as it does for a sender
    /// that does not trust the certificate. Either is that one connection's:
    /// the receiver can accept the next.
    pub fn accept(&self) -> Result<TlsConnection, TlsError> {
        let (tcp, peer) = self.listener.accept().map_err(TlsError::Io)?;
        tcp.set_nodelay(true).map_err(TlsError::Io)?;
        let conn = ServerConnection::new(Arc::clone(&self.config)).map_err(TlsError::Tls)?;
        let stream = handshake(conn, tcp, self.handshake_timeout)?;
        Ok(TlsConnection {
            stream,
            peer,
            frame: vec![0; MAX_FRAME_BYTES],
        })
    }
}

/// One sender's connection to a [`TlsReceiver`], from which its frames are
/// read in the order they were sent.
pub struct TlsConnection {
    stream: StreamOwned<ServerConnection, TcpStream>,
    peer: SocketAddr,
    /// The frame being read, which [`TlsConnection::recv`]'s [`Frame`]
    /// borrows its envelope from.
    frame: Vec<u8>,
}

impl TlsConnection {
    /// The sender's address.
    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer
    }

    /// Wait at most `timeout` for each read [`TlsConnection::recv`] makes
    /// (`None`, the default, waits as long as it takes); a read past it
    /// fails with a [`TlsError::Io`] of [`io::ErrorKind::WouldBlock`] or
    /// [`io::ErrorKind::TimedOut`], after which the connection is not usable
    /// if the read had begun a frame.
    ///
    /// # Errors
    ///
    /// [`TlsError::Io`] for a zero `timeout`, which the socket refuses.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), TlsError> {
        self.stream
            .sock
            .set_read_timeout(timeout)
            .map_err(TlsError::Io)
    }

    /// The next frame, decoded and checked as [`decode`] checks a datagram,
    /// borrowing its envelope from the connection; `None` once the sender
    /// has ended the stream with `close_notify`.
    ///
    /// # Errors
    ///
    /// [`TlsError::FrameLength`] for a length prefix no frame can have;
    /// [`TlsError::Wire`] for a frame that does not decode;
    /// [`TlsError::LengthMismatch`] for one shorter than its prefix;
    /// [`TlsError::Io`] when the connection ends without `close_notify`
    /// ([`io::ErrorKind::UnexpectedEof`]) or a read fails;
    /// [`TlsError::Tls`] when TLS refuses a record. The connection is not
    /// usable after any of them.
    pub fn recv(&mut self) -> Result<Option<Frame<'_>>, TlsError> {
        let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
        if !read_unless_closed(&mut self.stream, &mut prefix)? {
            return Ok(None);
        }
        let len = u32::from_le_bytes(prefix) as usize;
        if !(MIN_FRAME_BYTES..=MAX_FRAME_BYTES).contains(&len) {
            return Err(TlsError::FrameLength {
                len,
                min: MIN_FRAME_BYTES,
                max: MAX_FRAME_BYTES,
            });
        }
        self.stream
            .read_exact(&mut self.frame[..len])
            .map_err(from_io)?;
        let frame = decode(&self.frame[..len])?;
        let decoded = datagram_size(frame.envelope.len());
        if decoded != len {
            return Err(TlsError::LengthMismatch {
                prefix: len,
                frame: decoded,
            });
        }
        Ok(Some(frame))
    }
}

/// Fill `buf` from `stream`, or return `false` when the stream ends cleanly
/// (`close_notify`) before its first byte.
fn read_unless_closed(stream: &mut impl Read, buf: &mut [u8]) -> Result<bool, TlsError> {
    let mut filled = 0;
    while filled < buf.len() {
        match stream.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => {
                return Err(TlsError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the stream ended inside a frame's length",
                )))
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(from_io(e)),
        }
    }
    Ok(true)
}

/// The error a socket operation on a TLS stream failed with: TLS's own
/// refusal, which rustls passes through `io::Error`, as [`TlsError::Tls`].
fn from_io(e: io::Error) -> TlsError {
    match e
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
    {
        Some(refused) => TlsError::Tls(refused.clone()),
        None => TlsError::Io(e),
    }
}

/// Run `conn`'s handshake on `tcp` to its end, each read and write waiting at
/// most `timeout` (zero: as long as it takes), and hand back the stream for
/// the frames, with no read or write timeout.
fn handshake<C, S>(
    mut conn: C,
    mut tcp: TcpStream,
    timeout: Duration,
) -> Result<StreamOwned<C, TcpStream>, TlsError>
where
    C: Deref<Target = ConnectionCommon<S>> + DerefMut,
    S: SideData,
{
    let deadline = (!timeout.is_zero()).then_some(timeout);
    set_timeouts(&tcp, deadline)?;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp).map_err(|e| match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => TlsError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("the TLS handshake took longer than {timeout:?}"),
            )),
            _ => from_io(e),
        })?;
    }
    set_timeouts(&tcp, None)?;
    Ok(StreamOwned::new(conn, tcp))
}

fn set_timeouts(tcp: &TcpStream, timeout: Option<Duration>) -> Result<(), TlsError> {
    tcp.set_read_timeout(timeout).map_err(TlsError::Io)?;
    tcp.set_write_timeout(timeout).map_err(TlsError::Io)
}

/// Connect to the first of `target`'s addresses that answers, each attempt
/// waiting at most `timeout` (zero: as long as the system does).
fn connect_tcp(target: &str, timeout: Duration) -> Result<TcpStream, TlsError> {
    let mut last = None;
    for addr in target.to_socket_addrs().map_err(TlsError::Io)? {
        let attempt = if timeout.is_zero() {
            TcpStream::connect(addr)
        } else {
            TcpStream::connect_timeout(&addr, timeout)
        };
        match attempt {
            Ok(tcp) => return Ok(tcp),
            Err(e) => last = Some(e),
        }
    }
    Err(TlsError::Io(last.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{target} resolves to no address"),
        )
    })))
}

/// The host of `target`, `host:port`: without the port, and without the
/// brackets around an IPv6 address.
fn host_of(target: &str) -> &str {
    let host = target.rsplit_once(':').map_or(target, |(host, _port)| host);
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
}

fn server_name(name: &str) -> Result<ServerName<'static>, TlsError> {
    ServerName::try_from(name.to_string()).map_err(|_| TlsError::InvalidServerName(name.into()))
}

/// The one crypto provider the stage uses, whatever another crate in the
/// process installs as rustls's default.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Every certificate in `pem`, which must hold at least one; `field` names
/// the configuration field in an error.
fn certificates(pem: &[u8], field: &'static str) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let certs = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| TlsError::Pem {
            field,
            reason: e.to_string(),
        })?;
    if certs.is_empty() {
        return Err(TlsError::Pem {
            field,
            reason: "no CERTIFICATE section".into(),
        });
    }
    Ok(certs)
}

/// The roots a sender trusts: `trust_roots`, or webpki-roots' set when it is
/// empty.
fn trust_roots(pem: &[u8]) -> Result<RootCertStore, TlsError> {
    if pem.is_empty() {
        return Ok(RootCertStore::from_iter(
            webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
        ));
    }
    let mut roots = RootCertStore::empty();
    for cert in certificates(pem, "trust_roots")? {
        roots.add(cert).map_err(|e| TlsError::Pem {
            field: "trust_roots",
            reason: format!("a certificate is not usable as a root: {e}"),
        })?;
    }
    Ok(roots)
}

fn client_config(config: &TlsConfig) -> Result<ClientConfig, TlsError> {
    let provider = provider();
    let roots = Arc::new(trust_roots(&config.trust_roots)?);
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(TlsError::Tls)?;
    let builder = if config.accept_self_signed {
        let roots = verifier(roots, &provider)?;
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SelfSignedToo { roots, provider }))
    } else {
        builder.with_root_certificates(roots)
    };
    Ok(builder.with_no_client_auth())
}

fn server_config(config: &TlsConfig) -> Result<ServerConfig, TlsError> {
    let chain = certificates(&config.cert_chain, "cert_chain")?;
    let key = PrivateKeyDer::from_pem_slice(&config.private_key).map_err(|e| TlsError::Pem {
        field: "private_key",
        reason: match e {
            rustls::pki_types::pem::Error::NoItemsFound => "no PRIVATE KEY section".into(),
            e => e.to_string(),
        },
    })?;
    let mut server = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(TlsError::Tls)?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(TlsError::Tls)?;
    // Nothing goes from the receiver to the sender after the handshake, not
    // even session tickets: a sender that closes with tickets unread in its
    // socket resets the connection, which can destroy its close_notify before
    // the receiver reads it.
    server.send_tls13_tickets = 0;
    Ok(server)
}

/// rustls's own certificate verifier over `roots`.
fn verifier(
    roots: Arc<RootCertStore>,
    provider: &Arc<CryptoProvider>,
) -> Result<Arc<WebPkiServerVerifier>, TlsError> {
    WebPkiServerVerifier::builder_with_provider(roots, Arc::clone(provider))
        .build()
        .map_err(|e| TlsError::Tls(rustls::Error::General(e.to_string())))
}

/// The verifier [`TlsConfig::accept_self_signed`] asks for: a certificate
/// that chains to the roots, or one that verifies as its own root.
///
/// The second check is rustls's verifier with the certificate itself as the
/// only root, so a certificate passes it only if it is signed by its own key,
/// and the server name, the validity period and the handshake's signature
/// are checked as for any other. Nothing here is cryptography of the tree's
/// own.
#[derive(Debug)]
struct SelfSignedToo {
    roots: Arc<WebPkiServerVerifier>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for SelfSignedToo {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let by_roots = self.roots.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        if by_roots.is_ok() {
            return by_roots;
        }
        let mut own = RootCertStore::empty();
        if own.add(end_entity.clone()).is_err() {
            return by_roots;
        }
        let own = verifier(Arc::new(own), &self.provider)
            .map_err(|_| rustls::Error::General("the certificate as its own root".into()))?;
        own.verify_server_cert(end_entity, &[], server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.roots.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.roots.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.roots.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn modes_are_distinct() {
        assert_ne!(Mode::Dtls, Mode::QuicTunnel);
        assert_ne!(Mode::Dtls, Mode::TlsTcpStream);
        assert_ne!(Mode::QuicTunnel, Mode::TlsTcpStream);
    }

    /// The host comes off a target with its port, and an IPv6 address
    /// without its brackets.
    #[test]
    fn the_host_of_a_target() {
        assert_eq!(host_of("mesh.example:443"), "mesh.example");
        assert_eq!(host_of("127.0.0.1:4433"), "127.0.0.1");
        assert_eq!(host_of("[::1]:4433"), "::1");
        assert_eq!(host_of("mesh.example"), "mesh.example");
    }

    /// The default trusts webpki-roots and nothing self-signed, and the
    /// private key never reaches `Debug`.
    #[test]
    fn the_default_and_its_debug() {
        let mut config = TlsConfig::default();
        assert_eq!(config.mode, Mode::TlsTcpStream);
        assert!(!config.accept_self_signed);
        assert_eq!(config.handshake_timeout, DEFAULT_HANDSHAKE_TIMEOUT);
        assert!(!trust_roots(&config.trust_roots).unwrap().is_empty());
        config.private_key = b"-----BEGIN PRIVATE KEY-----\nsecret\n".to_vec();
        let shown = format!("{config:?}");
        assert!(shown.contains("private_key: elided"), "{shown}");
        assert!(!shown.contains("secret"), "{shown}");
    }

    /// A frame is the length bounds' worth of bytes: the shortest has no
    /// envelope and the longest the largest envelope a datagram holds.
    #[test]
    fn the_frame_bounds_are_the_datagrams() {
        assert_eq!(MIN_FRAME_BYTES, datagram_size(0));
        assert_eq!(MAX_FRAME_BYTES, datagram_size(MAX_ENVELOPE));
        assert_eq!(MAX_FRAME_BYTES, pulse_wire_udp::MTU_BYTES);
    }
}
