// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! The TLS stage on loopback: frames cross a real TLS 1.3 handshake whole and
//! in order, a server the sender does not trust is refused, a self-signed one
//! passes only when `accept_self_signed` says so and only for its own name,
//! and the deferred modes say they are deferred.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use enterprise_stages::tls::{
    Mode, TlsConfig, TlsError, TlsReceiver, TlsSender, LENGTH_PREFIX_BYTES,
};
use pulse_canon::Pulse;
use pulse_wire_udp::MAX_ENVELOPE;
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::io::Write as _;
use std::net::TcpStream;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// A certificate for `names` and its key, as PEM.
struct Identity {
    cert: String,
    key: String,
}

/// A certificate authority that issues [`Identity`]s.
struct Ca {
    issuer: CertifiedIssuer<'static, KeyPair>,
}

impl Ca {
    fn new() -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "pulse-mesh test CA");
        Self {
            issuer: CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap(),
        }
    }

    fn pem(&self) -> String {
        self.issuer.pem()
    }

    fn issue(&self, names: &[&str]) -> Identity {
        let key = KeyPair::generate().unwrap();
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let cert = CertificateParams::new(names)
            .unwrap()
            .signed_by(&key, &self.issuer)
            .unwrap();
        Identity {
            cert: cert.pem(),
            key: key.serialize_pem(),
        }
    }
}

/// A self-signed certificate for `names`.
fn self_signed(names: &[&str]) -> Identity {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    let made = rcgen::generate_simple_self_signed(names).unwrap();
    Identity {
        cert: made.cert.pem(),
        key: made.signing_key.serialize_pem(),
    }
}

/// A receiver's configuration presenting `identity`.
fn serving(identity: &Identity) -> TlsConfig {
    let mut config = TlsConfig::default();
    config.cert_chain = identity.cert.clone().into_bytes();
    config.private_key = identity.key.clone().into_bytes();
    config.handshake_timeout = Duration::from_secs(5);
    config
}

/// A sender's configuration trusting `roots` (PEM; empty for webpki-roots).
fn trusting(roots: &str) -> TlsConfig {
    let mut config = TlsConfig::default();
    config.trust_roots = roots.as_bytes().to_vec();
    config.handshake_timeout = Duration::from_secs(5);
    config
}

/// A receiver on a loopback port of its own, presenting `identity`.
fn listening(identity: &Identity) -> (TlsReceiver, String) {
    let receiver = TlsReceiver::bind("127.0.0.1:0", &serving(identity)).unwrap();
    let port = receiver.local_addr().unwrap().port();
    (receiver, format!("127.0.0.1:{port}"))
}

/// The frames a receiver read from its first connection, as `(seq, send_ns,
/// pulse, envelope)`, until the sender closed the stream; or the error it
/// stopped at.
type Received = Result<Vec<(u64, u64, Vec<u8>, Vec<u8>)>, TlsError>;

fn read_all(receiver: TlsReceiver) -> JoinHandle<Received> {
    std::thread::spawn(move || {
        let mut conn = receiver.accept()?;
        conn.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut frames = Vec::new();
        while let Some(frame) = conn.recv()? {
            frames.push((
                frame.seq,
                frame.send_ns,
                frame.pulse.to_vec(),
                frame.envelope.to_vec(),
            ));
        }
        Ok(frames)
    })
}

/// Several frames, from an empty envelope to the largest one, arrive whole
/// and in order, and closing the sender ends the receiver's stream cleanly.
#[test]
fn frames_cross_a_tls_handshake_whole_and_in_order() {
    let ca = Ca::new();
    let (receiver, target) = listening(&ca.issue(&["localhost"]));
    let reading = read_all(receiver);

    let mut config = trusting(&ca.pem());
    config.sni = Some("localhost".into());
    let mut sender = TlsSender::connect(&target, &config).unwrap();
    let mut pulse_bytes = [0u8; 128];
    pulse_bytes[0] = 0x5a;
    pulse_bytes[127] = 0xa5;
    let pulse = Pulse::from_array(&pulse_bytes);
    let envelopes: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"one".to_vec(),
        (0..=255u8).collect(),
        vec![0x77; MAX_ENVELOPE],
    ];
    for (seq, envelope) in envelopes.iter().enumerate() {
        sender
            .send(seq as u64, 1_000 + seq as u64, &pulse, envelope)
            .unwrap();
    }
    sender.close().unwrap();

    let frames = reading.join().unwrap().unwrap();
    assert_eq!(frames.len(), envelopes.len());
    for (seq, ((got_seq, send_ns, got_pulse, got_envelope), envelope)) in
        frames.iter().zip(&envelopes).enumerate()
    {
        assert_eq!(*got_seq, seq as u64);
        assert_eq!(*send_ns, 1_000 + seq as u64);
        assert_eq!(got_pulse.as_slice(), pulse.as_bytes().as_slice());
        assert_eq!(got_envelope, envelope);
    }
}

/// Without `sni`, the target's host is the name verified: here an IP
/// address, which the certificate must name.
#[test]
fn the_targets_ip_address_is_the_name_verified_without_sni() {
    let ca = Ca::new();
    let (receiver, target) = listening(&ca.issue(&["127.0.0.1"]));
    let reading = read_all(receiver);
    let mut sender = TlsSender::connect(&target, &trusting(&ca.pem())).unwrap();
    sender.send(9, 9, &Pulse::zeroed(), b"by address").unwrap();
    drop(sender);
    let frames = reading.join().unwrap().unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].3, b"by address");
}

/// The connect's error when the server presents `identity`, and the
/// receiver's handshake failed with it too.
fn refused(identity: &Identity, config: &TlsConfig) -> TlsError {
    let (receiver, target) = listening(identity);
    let accepting = std::thread::spawn(move || receiver.accept().map(|_| ()));
    let err = match TlsSender::connect(&target, config) {
        Ok(_) => panic!("the sender trusted the server"),
        Err(e) => e,
    };
    assert!(
        accepting.join().unwrap().is_err(),
        "the receiver completed a handshake the sender refused"
    );
    err
}

fn is_certificate_error(err: &TlsError) -> bool {
    matches!(err, TlsError::Tls(rustls::Error::InvalidCertificate(_)))
}

/// A server whose certificate chains to none of the roots is refused: a
/// self-signed one under the default roots, and one from another CA under
/// a CA of the sender's own.
#[test]
fn a_server_the_sender_does_not_trust_is_refused() {
    let mut config = trusting("");
    config.sni = Some("localhost".into());
    let err = refused(&self_signed(&["localhost"]), &config);
    assert!(is_certificate_error(&err), "{err:?}");
    assert!(err.to_string().contains("certificate"), "{err}");

    let mut config = trusting(&Ca::new().pem());
    config.sni = Some("localhost".into());
    let err = refused(&Ca::new().issue(&["localhost"]), &config);
    assert!(is_certificate_error(&err), "{err:?}");
}

/// A trusted certificate for another name is refused.
#[test]
fn a_certificate_for_another_name_is_refused() {
    let ca = Ca::new();
    let mut config = trusting(&ca.pem());
    config.sni = Some("mesh.example".into());
    let err = refused(&ca.issue(&["localhost"]), &config);
    assert!(
        matches!(
            err,
            TlsError::Tls(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForName
                    | rustls::CertificateError::NotValidForNameContext { .. }
            ))
        ),
        "{err:?}"
    );
}

/// `accept_self_signed` admits a self-signed server, and still checks the
/// name it gives and refuses a certificate from a CA it does not trust.
#[test]
fn accept_self_signed_admits_a_self_signed_server_for_its_own_name_only() {
    let identity = self_signed(&["localhost"]);
    let (receiver, target) = listening(&identity);
    let reading = read_all(receiver);
    let mut config = trusting("");
    config.sni = Some("localhost".into());
    config.accept_self_signed = true;
    let mut sender = TlsSender::connect(&target, &config).unwrap();
    sender.send(1, 2, &Pulse::zeroed(), b"self-signed").unwrap();
    sender.close().unwrap();
    assert_eq!(reading.join().unwrap().unwrap()[0].3, b"self-signed");

    config.sni = Some("mesh.example".into());
    let err = refused(&self_signed(&["localhost"]), &config);
    assert!(is_certificate_error(&err), "{err:?}");

    config.sni = Some("localhost".into());
    let err = refused(&Ca::new().issue(&["localhost"]), &config);
    assert!(is_certificate_error(&err), "{err:?}");
}

/// A raw rustls client of a receiver at `target`, trusting `ca`, for writing
/// what a [`TlsSender`] never would.
fn raw_client(ca: &Ca, target: &str) -> StreamOwned<ClientConnection, TcpStream> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(ca.pem().as_bytes()).unwrap())
        .unwrap();
    let client =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
    let conn = ClientConnection::new(Arc::new(client), "localhost".try_into().unwrap()).unwrap();
    StreamOwned::new(conn, TcpStream::connect(target).unwrap())
}

/// A sender dropped without `close` still ends the stream cleanly, and one
/// that is gone without `close_notify` is reported as such.
#[test]
fn the_end_of_the_stream_is_told_from_a_cut() {
    let ca = Ca::new();
    let (receiver, target) = listening(&ca.issue(&["localhost"]));
    let reading = read_all(receiver);
    let mut config = trusting(&ca.pem());
    config.sni = Some("localhost".into());
    drop(TlsSender::connect(&target, &config).unwrap());
    assert!(reading.join().unwrap().unwrap().is_empty());

    // Half a length prefix, then the socket closed without close_notify.
    let (receiver, target) = listening(&ca.issue(&["localhost"]));
    let reading = read_all(receiver);
    let mut stream = raw_client(&ca, &target);
    stream.write_all(&[1, 0]).unwrap();
    stream.flush().unwrap();
    drop(stream);
    match reading.join().unwrap() {
        Err(TlsError::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof),
        other => panic!("a cut stream read as {other:?}"),
    }
}

/// A length prefix no frame can have is refused before anything more is
/// read, and so is a frame shorter than its prefix.
#[test]
fn a_length_no_frame_can_have_is_refused() {
    let ca = Ca::new();
    let (receiver, target) = listening(&ca.issue(&["localhost"]));
    let reading = read_all(receiver);
    let mut stream = raw_client(&ca, &target);
    stream.write_all(&u32::MAX.to_le_bytes()).unwrap();
    stream.flush().unwrap();
    match reading.join().unwrap() {
        Err(TlsError::FrameLength { len, .. }) => assert_eq!(len, u32::MAX as usize),
        other => panic!("an impossible length read as {other:?}"),
    }

    let (receiver, target) = listening(&ca.issue(&["localhost"]));
    let reading = read_all(receiver);
    let mut stream = raw_client(&ca, &target);
    let mut frame = Vec::new();
    pulse_wire_udp::encode_into(1, 1, &Pulse::zeroed(), b"short", &mut frame).unwrap();
    let padded = frame.len() + 3;
    stream
        .write_all(&(padded as u32).to_le_bytes()[..LENGTH_PREFIX_BYTES])
        .unwrap();
    stream.write_all(&frame).unwrap();
    stream.write_all(&[0; 3]).unwrap();
    stream.flush().unwrap();
    match reading.join().unwrap() {
        Err(TlsError::LengthMismatch { prefix, frame: got }) => {
            assert_eq!((prefix, got), (padded, frame.len()));
        }
        other => panic!("a padded frame read as {other:?}"),
    }
}

/// DTLS and the QUIC tunnel are deferred: both entry points say so, before
/// they touch the configuration or the network.
#[test]
fn the_deferred_modes_return_integration_deferred() {
    for mode in [Mode::Dtls, Mode::QuicTunnel] {
        let mut config = TlsConfig::default();
        config.mode = mode;
        assert!(matches!(
            TlsSender::connect("127.0.0.1:443", &config),
            Err(TlsError::IntegrationDeferred)
        ));
        assert!(matches!(
            TlsReceiver::bind("127.0.0.1:0", &config),
            Err(TlsError::IntegrationDeferred)
        ));
    }
}

/// A receiver without a certificate and key, or with a key that is not the
/// certificate's, is refused at bind.
#[test]
fn a_receiver_needs_its_certificate_and_its_key() {
    let err = TlsReceiver::bind("127.0.0.1:0", &TlsConfig::default())
        .err()
        .unwrap();
    assert!(
        matches!(
            err,
            TlsError::Pem {
                field: "cert_chain",
                ..
            }
        ),
        "{err:?}"
    );
    let mut config = serving(&self_signed(&["localhost"]));
    config.private_key.clear();
    let err = TlsReceiver::bind("127.0.0.1:0", &config).err().unwrap();
    assert!(
        matches!(
            err,
            TlsError::Pem {
                field: "private_key",
                ..
            }
        ),
        "{err:?}"
    );
    config.private_key = self_signed(&["localhost"]).key.into_bytes();
    let err = TlsReceiver::bind("127.0.0.1:0", &config).err().unwrap();
    assert!(matches!(err, TlsError::Tls(_)), "{err:?}");
}
