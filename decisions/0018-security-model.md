# ADR-0018 — The security model: what is protected, by what, and what is not

**Status:** Accepted
**Date:** 2026-10-17
**Accepted:** 2026-10-17, by the Architect. The cryptographic-engineering review of item 9 is no longer a condition of acceptance; it remains a condition of recommending sealed mode for production, and of any release that does.
**Amended:** 2026-10-17: sealing wraps the whole datagram (data frames, FEC parity and ACK and NACK feedback alike) and the inner datagram keeps its CRC, so a sealed datagram costs 36 bytes, not 17, and its nonce is a per-session seal counter rather than the inner frame's `seq`; item 3 and the "Cost per datagram" consequence are restated to say so.
**Amended:** 2026-10-17: a client proves an Ed25519 identity key at attach, separate from a node's X25519 key for sealed sessions (an X25519 key cannot sign); item 6 is restated to say which key does what.
**Amended:** 2026-10-19: the replay window and the rekey limit run on item 3's per-session seal counter, which every sealed datagram carries, not on the inner frame's `seq`, as the first amendment's change of nonce implied and the code is built; items 4 and 5 are restated to say so.

## Context

Nothing in this release encrypts or authenticates traffic. The shared-memory ring, the envelope store and the daemon's Unix socket are created owner-only (`0o600`), so on one host the operating system's user boundary is the only protection, and it is a real one. Everything else is open:

- **UDP (`pulse-wire-udp`).** Frames are plaintext. Their CRC32C catches corruption, not tampering, and the receiver's source filters compare IP addresses, which an attacker can forge (the receiver's own docs say so). Anyone on the path can read every Pulse and envelope, change them, inject frames, and replay old ones.
- **The daemon's WebSocket.** Any local user can connect to `127.0.0.1:9876` without credentials, which steps around the Unix socket's owner-only mode. Since #69 it cannot be bound off loopback without `--ws-allow-remote`, but with that flag it serves the hub in plaintext to the network.
- **Identity.** A client's node id is a hash of the name it chooses at attach, so `mesh-schema`'s producer and consumer lists trust whatever name a client claims.
- **The Pulse's replay fields.** `chain_step` and `preimage` are reserved for a replay defence that is not implemented; nothing reads or checks them (#70 corrected the docs that said otherwise).
- **The capability Hello (`mesh-capability`).** It carries a `TLS_443` bit, but nothing sends the Hello, nothing upgrades, and the Hello itself is unauthenticated, so a path attacker could strip capabilities.

Every use of Pulse beyond one host waits on this: the remote workers and the multi-region runs (2026-10-16 forges) ran over plaintext UDP.

## Threat model

**Defended against:** an attacker on the network path between two Pulse nodes, or between a remote client and a daemon, who can read, drop, change, reorder, inject and replay datagrams and connections; and another local user connecting to a daemon's WebSocket.

**Not defended against:** a process running as the same user as the node (it can map the shared memory and read the key file), a compromised host or kernel, volumetric denial of service, and traffic analysis of datagram sizes and timing. These are stated so that nobody reads the design as covering them.

## Decision

1. **Node identity is a key, not a name.** Each node holds one long-term X25519 static key pair, generated on first start and stored in a file created `0o600`. A node's public key, not its name, is its identity on the network. Peers are admitted by an explicit allowlist of public keys in configuration; there is no certificate authority in this design.
2. **UDP sessions use a Noise handshake.** Two nodes run `Noise_IK_25519_ChaChaPoly_BLAKE2s` (the initiator knows the responder's static key from its allowlist; the responder learns and checks the initiator's) through the `snow` crate. The handshake yields one key per direction and a 32-bit session index each side picks for the other.
3. **Each datagram is sealed with an AEAD.** Sealing wraps whole datagrams: whatever the transport would put in a datagram (a data frame, an FEC parity datagram, an ACK or NACK, a retransmission) becomes, unchanged and with its CRC, the plaintext of one sealed datagram of a new wire version. Its cleartext header carries the magic, the version, a kind, the receiver's session index and the session's own 64-bit seal counter; the inner datagram follows as ChaCha20-Poly1305 ciphertext with the header as associated data, then the 16-byte tag. The nonce is the seal counter, explicit, so datagrams may arrive out of order. The 128-byte Pulse is encrypted with everything else in the inner datagram.
4. **Replay is refused at the transport.** The receiver keeps, per session, a sliding window over the 2,048 seal counters up to the highest it has accepted; a datagram whose counter is older than the window, or already seen inside it, is dropped and counted, and the window moves only once a datagram's tag has verified, so a forgery cannot move it. *(Amended 2026-10-19: it said the window runs on `seq`.)* This is the replay defence for network traffic in this release. `chain_step` and `preimage` stay reserved; giving them end-to-end meaning across several hops is left to the multi-host design.
5. **Keys turn over.** A session rekeys with a fresh handshake every two minutes and before its seal counter could wrap; a session idle past three rekey periods is dropped. A peer removed from the allowlist can no longer complete a handshake, so its access ends at its session's next rekey at the latest. *(Amended 2026-10-19: it said before `seq` could wrap.)*
6. **The daemon's remote surfaces use TLS 1.3.** The WebSocket gains `wss://` through `rustls`, and the enterprise ring's TLS stage is implemented on the same crate. A client attaching over the network proves possession of an Ed25519 identity key by signing a challenge from the daemon, bound to the TLS session by an exporter value; the daemon derives the node id from the key, and `mesh-schema`'s lists then name keys rather than claimed names. This key is separate from a node's X25519 key of item 1, which the sealed sessions use: Noise's static keys agree secrets and do not sign.
7. **Plaintext stays the default where the host already protects it.** Shared memory, the Unix socket and loopback UDP stay as they are. A node configured for sealed UDP refuses plaintext frames from any address that is not loopback.
8. **The capability Hello rides inside the authenticated session.** Once sealed sessions exist, the Hello is exchanged as the first sealed message, so a path attacker cannot strip or forge its bits, and `upgrade_recommended` switches a link to the sealed transport.
9. **Only vetted implementations.** `snow`, `chacha20poly1305` and `rustls`, pinned in the workspace table. No primitive, construction or handshake is written in this tree. The sealed decode gets a fuzz target. Someone who does cryptographic engineering reviews this ADR and the sealed-frame code before sealed mode is recommended for production; until then the crate docs say the review is pending.

## Consequences

- **Cost per datagram.** Sealing adds a ChaCha20-Poly1305 pass and a replay-window check per datagram, and 36 bytes per datagram: a 20-byte cleartext header (the magic, the version, a kind byte, two reserved bytes, the receiver's 4-byte session index and the sender's 8-byte seal counter) and the 16-byte tag. Sealing wraps the whole datagram, FEC parity and ACK and NACK feedback included, and the inner datagram keeps its CRC, which the tag does not replace; so a sealed data frame holds at most 1,279 envelope bytes, 36 fewer than `MAX_ENVELOPE`, or 1,261 with FEC on, 36 fewer than `FEC_MAX_ENVELOPE`. The cost is measured as a reading when the code exists; no figure is promised here. *(Amended 2026-10-17: it said 17 bytes.)*
- **FEC parity frames are sealed too**, so the receiver can no longer admit unauthenticated parity (`fec.rs` admits it today because nothing authenticates it).
- **Multicast does not fit pairwise sessions.** A group key scheme is a separate decision, needed only if multicast lands.
- **Key management becomes an operator task:** generating, distributing and revoking allowlisted public keys. The first version keeps it to a file and a command (`pms keys`); anything more is a later decision.
- **Fragmentation (P1-6) seals each fragment as its own datagram**, so a fragment is authenticated before it is buffered and an attacker cannot fill reassembly memory with forgeries.
- **What still is not protected:** everything in "Not defended against" above, and any deployment that leaves sealed mode off. The book's trust-boundaries page should say so in as many words.

## Order of work

L2-6 (sealed UDP, items 2 to 5 and 7), then L2-7 (TLS 1.3 and `wss://`, item 6's transport half), then L2-8 (key-proof attach and keyed schema lists), then L2-9 (the Hello inside the session, item 8).
