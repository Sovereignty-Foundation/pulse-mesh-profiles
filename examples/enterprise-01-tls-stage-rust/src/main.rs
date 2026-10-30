// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! Example — the enterprise ring's TLS stage, reached through its hub (Rust).
//!
//! The enterprise ring is imported through `enterprise_hub` alone, which
//! re-exports the ring's layers; nothing here names `enterprise_stages`.
//! The example makes a certificate for `localhost`, starts a receiver on a
//! loopback port that presents it, connects a sender that trusts it as its
//! one root, and sends one frame across the TLS 1.3 handshake. Then it asks
//! for the DTLS mode, which this release defers, and prints what the stage
//! says.
#![forbid(unsafe_code)]
// A demonstration: it prints, and exits 1 if the frame does not cross.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use enterprise_hub::stages::tls::{Mode, TlsConfig, TlsReceiver, TlsSender};
use pulse_canon::Pulse;

fn main() -> std::process::ExitCode {
    // The hub's manifest names the stage this example reaches.
    println!(
        "enterprise ring manifest: {:?}",
        enterprise_hub::RING_MANIFEST
    );
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tls-stage: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])?;
    let cert = made.cert.pem();

    // `TlsConfig` is `non_exhaustive`: it is built from its `Default` and
    // set field by field.
    let mut serving = TlsConfig::default();
    serving.cert_chain = cert.clone().into_bytes();
    serving.private_key = made.signing_key.serialize_pem().into_bytes();
    let receiver = TlsReceiver::bind("127.0.0.1:0", &serving)?;
    let target = receiver.local_addr()?.to_string();
    println!("receiver: listening on {target}");

    let reading = std::thread::spawn(move || -> Result<String, String> {
        let mut conn = receiver.accept().map_err(|e| e.to_string())?;
        match conn.recv().map_err(|e| e.to_string())? {
            Some(frame) => Ok(format!(
                "seq {}, envelope {:?}",
                frame.seq,
                String::from_utf8_lossy(frame.envelope)
            )),
            None => Err("the sender closed the stream before its frame".into()),
        }
    });

    // The certificate is self-signed, so the sender trusts it as its root;
    // `sni` names the host the certificate was made for.
    let mut sending = TlsConfig::default();
    sending.mode = Mode::TlsTcpStream;
    sending.sni = Some("localhost".to_string());
    sending.trust_roots = cert.into_bytes();
    let mut sender = TlsSender::connect(&target, &sending)?;
    sender.send(1, 0, &Pulse::zeroed(), b"hello over TLS 1.3")?;
    sender.close()?;
    let got = reading
        .join()
        .map_err(|_| "the receiver's thread panicked")??;
    println!("receiver: got {got}");

    sending.mode = Mode::Dtls;
    match TlsSender::connect(&target, &sending) {
        Ok(_) => println!("dtls sender: connected"),
        Err(e) => println!("dtls sender: {e}"),
    }
    Ok(())
}
