// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! enterprise_ring: the `stages` layer, stateful primitives on the hot path.
//!
//! **Status: partial.** The `tls` stage's `Mode::TlsTcpStream` works: TLS 1.3
//! over TCP on rustls, the sender verifying the server's certificate and name,
//! the receiver presenting a configured certificate chain and key, and each
//! wire frame carried whole behind its length. Its other two modes,
//! `Mode::Dtls` (DTLS 1.3 over UDP) and `Mode::QuicTunnel`, are deferred:
//! `TlsSender::connect` and `TlsReceiver::bind` return
//! `TlsError::IntegrationDeferred` for them. The receiver authenticates no
//! client.
//!
//! - `tls`: TLS and DTLS encapsulation of the wire transport on port 443; a
//!   stage because it carries session state on the per-datagram path.
//!
//! Imports come through the hub: `enterprise_hub::stages::tls`.
//!
//! In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/enterprise_ring.html>.
#![forbid(unsafe_code)]

pub mod tls;
