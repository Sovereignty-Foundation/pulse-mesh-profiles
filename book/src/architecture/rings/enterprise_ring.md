# `enterprise_ring`

<p class="pm-kind"><b>Reference</b> · the enterprise ring, crate by crate</p>

The TLS transport, for carrying Pulse traffic between hosts over TCP with TLS 1.3, and the deferred DTLS and QUIC-tunnel modes beside it. Downstream code should depend on `enterprise-hub`, not the inner crates directly.

| Crate | Layer | Contents |
|---|---|---|
| `enterprise-core` | core | **Empty by design.** The ring's one primitive, the TLS transport, is stateful and lives in `stages`; the crate keeps the four-crate shape uniform and is where shared core types land. |
| `enterprise-functions` | functions | **Empty by design.** The ring has no stateless primitive yet; the crate keeps the four-crate shape uniform. |
| `enterprise-stages` | stages | **Status: partial.** `tls` — the wire transport's frames over TLS 1.3 on TCP, on `rustls`: `TlsSender::connect` verifies the server's certificate and name, `TlsReceiver::bind` presents a configured certificate chain and key and `accept`s connections, and each frame is carried whole behind its length (`Mode::TlsTcpStream`). The receiver authenticates no client. The other two modes, `Mode::Dtls` (DTLS 1.3 over UDP) and `Mode::QuicTunnel`, are deferred: `connect` and `bind` return `TlsError::IntegrationDeferred` for them (ADR-0017). |
| `enterprise-hub` | hub | The composition surface. Since `functions` is empty, it registers only the `tls` stage. `#![no_std]`. |

Both `core` and `functions` being empty here is expected, not a gap — see [Rings: domain surfaces](overview.md) for why the four-crate shape stays uniform even when a ring's primitives are all stages.
