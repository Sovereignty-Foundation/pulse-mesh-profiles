# Enterprise: Pulses between services over TLS 1.3

<p class="pm-kind"><b>Workflow</b> · for a developer carrying Pulse traffic between services across a network that admits TLS on TCP; about fifteen minutes, in Rust, with the profiles repository beside the standard</p>

<figure class="pm-art"><svg class="uc-diagram" viewBox="0 0 380 240" role="img" aria-label="Frames between services, over a verified tunnel"><circle cx="50" cy="120" r="18" class="uc-node-strong"/><text x="50" y="160" class="uc-label">service a</text><circle cx="330" cy="120" r="18" class="uc-ring"/><circle cx="330" cy="120" r="9" class="uc-ring-dash"/><text x="330" y="160" class="uc-label">service b</text><rect x="100" y="96" width="180" height="48" rx="24" class="uc-ring"/><rect x="112" y="108" width="156" height="24" rx="12" class="uc-ring-dash"/><line x1="68" y1="120" x2="100" y2="120" class="uc-edge"/><line x1="280" y1="120" x2="312" y2="120" class="uc-edge"/><text x="190" y="80" class="uc-label">tls 1.3 on tcp</text><text x="190" y="176" class="uc-label">verified server, frames behind a length</text><circle class="uc-pulse" r="3"><animateMotion dur="2.1s" begin="0s" repeatCount="indefinite" path="M68,120 L312,120"/></circle><circle class="uc-pulse" r="3"><animateMotion dur="2.1s" begin="0.7s" repeatCount="indefinite" path="M68,120 L312,120"/></circle></svg><figcaption>Frames between services, over a verified tunnel</figcaption></figure>

**You will have:** a receiver presenting a certificate on a loopback port, a sender that verifies it and sends one frame across a TLS 1.3 handshake, the frame read on the other side, and the stage's own statement of which of its three modes works in this release and which two are deferred.

**You need:** `pulse-mesh` and `pulse-mesh-profiles` side by side, as [Start here](../start/onboard-your-agent.md) describes, and the Rust toolchain. For two machines, a certificate for the receiving host's name and TCP between the hosts on the port you choose.

## Hand this to your agent

```text
Run the workflow "Enterprise: Pulses between services over TLS 1.3" from book/src/workflows/enterprise-tls.md in the pulse-mesh-profiles repository:
1. Run cargo run --release -p example-enterprise-01-tls-stage-rust and show me its output. Expect an "enterprise ring manifest" line naming enterprise_stages::tls, "receiver: listening on 127.0.0.1:<port>", "receiver: got seq 1, envelope \"hello over TLS 1.3\"", and a last line beginning "dtls sender: the DTLS and QUIC-tunnel modes are not integrated in this release".
2. Read examples/enterprise-01-tls-stage-rust/src/main.rs and explain in plain words how the sender came to trust the receiver's certificate, and why that way is for loopback tests only.
3. In the example, change `sending.mode = Mode::Dtls;` (line 73) to `Mode::QuicTunnel` and run it again; the last line still begins "dtls sender:" with the same deferred-mode message, and the exit status is still 0, since the TLS frame crossed before that. Put the mode back. (Do not change the TLS sender's mode on line 62: that connect would fail and the run would exit non-zero.)
Stop and tell me if anything fails, and say which step.
```

## Or do it by hand

### 1. Run the example

From the repository root:

```bash
cargo run --release -p example-enterprise-01-tls-stage-rust
```

```text
enterprise ring manifest: [("functions", []), ("stages", ["enterprise_stages::tls"])]
receiver: listening on 127.0.0.1:…
receiver: got seq 1, envelope "hello over TLS 1.3"
dtls sender: the DTLS and QUIC-tunnel modes are not integrated in this release; Mode::TlsTcpStream is
```

The port is the kernel's choice. The last line is the stage's own `TlsError::IntegrationDeferred`, printed as the program asked for the DTLS mode after the frame had crossed over TLS on TCP.

### 2. Read the program

```rust
{{#include ../../../examples/enterprise-01-tls-stage-rust/src/main.rs:34:}}
```

The receiver's `TlsConfig` carries a certificate chain and its private key, here a self-signed certificate for `localhost` made on the spot; `TlsReceiver::bind` listens, `accept` takes one connection, and `recv` returns the next frame or `None` when the sender has closed. The sender's `TlsConfig` names the server it expects (`sni`) and the roots it trusts (`trust_roots`): here the same self-signed certificate, so the sender trusts exactly one certificate and nothing else. `TlsSender::connect` performs the handshake and verifies the certificate and name; `send(seq, send_ns, &pulse, envelope)` carries one wire frame whole, behind a 4-byte length prefix; `close` ends the stream. Then the program asks for `Mode::Dtls` and prints what the stage says.

## Check it worked

| Check | Command | Pass |
|---|---|---|
| A frame crossed TLS 1.3 and was read | `cargo run --release -p example-enterprise-01-tls-stage-rust` | `receiver: got seq 1, envelope "hello over TLS 1.3"` |
| The deferred modes say so | Same run; or the example with `Mode::QuicTunnel` | The `dtls sender:` line, and exit status 0 because the frame had already crossed |
| The stage is declared by the hub | First line | The manifest names `enterprise_stages::tls` |

If the handshake fails with a certificate error, the sender's `sni` does not name what the certificate was made for, or `trust_roots` does not hold the chain's root; the default `trust_roots`, empty, trusts the Mozilla root program's roots as the `webpki-roots` crate carries them, which a self-signed certificate never chains to. If the handshake hangs and then fails with a timeout, `handshake_timeout` (10 s by default) passed without the other side answering.

## What happened

- **One frame, whole, behind a length.** The wire frame is the one `pulse-wire-udp` puts in a datagram, carried on a TCP stream under TLS 1.3 on `rustls`, each frame prefixed by its length as a 4-byte little-endian integer. The receiver reads one frame at a time and hands back its sequence number, the sender's clock, the Pulse and the envelope.
- **The sender authenticates the server; the server authenticates no client.** `connect` verifies the certificate chain against `trust_roots` and the name against `sni` (an IP address is checked against the certificate's addresses and not sent as SNI). The receiver presents `cert_chain` and `private_key` and accepts any client; a deployment that must know who connects puts that control in front of the port, or in what the frames carry.
- **Self-signed is for tests.** The example trusts its own certificate as a root, which is sound on loopback. `accept_self_signed` is a different and weaker thing: it lets the sender accept any self-signed certificate that names the server, which disables server authentication, so it stays `false` anywhere but a loopback test.
- **Status: partial.** `Mode::TlsTcpStream` works. `Mode::Dtls` (DTLS 1.3 over UDP, datagram semantics and the best latency, blocked by some firewalls on UDP/443) and `Mode::QuicTunnel` (QUIC carrying the traffic as a stream, passing where HTTP/3 does) are deferred: `TlsSender::connect` and `TlsReceiver::bind` return `TlsError::IntegrationDeferred` for them. The crate says so with `status = "partial"` (ADR-0017), and the ring's `core` and `functions` crates are empty by design.

What this stage does not do: it does not authenticate clients; it does not carry datagrams, so a TLS-on-TCP path has head-of-line blocking that the UDP transport does not; and it is not the daemon's link, which is sealed UDP between daemons ([Distributed compute](/docs/daemon/workflows/distributed-compute.html)).

## Make it yours

- **Two machines.** Make a certificate for the receiving host's name from a CA you control, set `cert_chain` and `private_key` on the receiver, bind it to the host's address, and on the sender set `sni` to that name and `trust_roots` to the CA's certificate. Open the port for TCP. Leave `accept_self_signed` at `false`.
- **Many frames.** `send` takes any sequence and clock you keep; the receiver's `recv` returns frames in order until `None`. `TlsConnection::set_read_timeout` bounds a wait.
- **The frames' content.** The envelope bytes you pass are what arrives; encode them with `pulse_canon` so a receiver can verify them by identifier, as [Across a network](/docs/examples/across-a-network.html) shows.

## Next

- [`enterprise_ring`](../architecture/rings/enterprise_ring.md): the ring crate by crate.
- [Distributed compute](/docs/daemon/workflows/distributed-compute.html): daemons linked across machines, the other way traffic crosses a network.
- [The UDP transport](/docs/reference/spec/06-udp-transport.html): the frame this stage carries.

## Spoken walkthrough

*A script for the video and audio version of this page, about ninety seconds.*

Two services, a network between them that allows TLS on TCP, and one Pulse to carry across.

The receiver makes a certificate for itself and listens. The sender is told two things: the name it expects, and the one certificate it trusts. It connects, the handshake runs, the certificate and the name check, and one frame goes across: a sequence number, a clock reading, a Pulse and a small envelope, behind a four-byte length. The receiver prints what it got.

Then the program asks the same stage for DTLS, and the stage answers plainly: not in this release; TLS over TCP is what works. That line is the stage's own, and the program still exits cleanly, because the frame had already crossed.

Two things to carry away. The sender authenticates the server; the server authenticates no client, so put that control in front of the port if you need it. And trusting your own self-signed certificate is fine on loopback; letting the sender accept any self-signed certificate is not, anywhere.
