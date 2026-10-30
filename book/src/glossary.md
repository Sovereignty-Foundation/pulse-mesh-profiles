# Glossary

<p class="pm-kind"><b>Reference</b> · for every reader; the book, the website, the command line and the error messages use these words in these senses</p>

Terms are listed alphabetically. Where a word has more than one meaning in this tree, each meaning has its own entry and the book always says which one it means. The Sovereignty stack's names are included because Pulse Mesh sits beside them; where a component is not part of this repository the entry says so.

**ACK** — A 12-byte feedback datagram (magic plus a `u64` cursor) the UDP receiver sends to the sender to report its cursor, one past the highest sequence it has seen. The sender's backpressure stall is released by it. Defined in `pulse-wire-udp`.

**Aegis™** — The bounded, typed execution layer of the Sovereignty stack (Aegis / Construct). Not part of this repository.

**Attach** — The first operation on a port-protocol connection: the client names itself, announces the protocol version and the capabilities it wants, and receives its node id.

**Backpressure** — The feedback loop that stops a UDP sender from running more than `target_queue_depth` frames ahead of the receiver's ACK cursor, for at most `stall_timeout` before the send fails. Amber in every figure.

**Basket** — The SQLite capture store of envelopes that the daemon writes for structured queries (`mesh-basket`; `pms` and the MCP server query it). Distinct from the envelope store, which is shared memory.

**BLAKE3-16** — The first 16 bytes of a BLAKE3 hash computed under a domain tag. The envelope id is one.

**Busy claim** — A store writer's claim that found its slot held by another writer, mid-store or crashed, or already taken by a newer lap, and gave up its cursor for a fresh one. Counted in the store's header and reported by `busy_claims()`; readers skip the cursors given up.

**Canon™** — The canonical codec, `pulse-canon`: the crate that turns a Pulse and a PulseEnvelope into bytes and back, exactly the same way in every implementation. Also the Sovereignty stack's name for that semantic layer.

**Capability handshake** — The 48-byte Hello exchange in `mesh-capability` by which two nodes on plain UDP agree on a transport upgrade (NACK retransmission, FEC, and the higher-tier backends) from the intersection of their capability bits.

**Channel** — The routing key an envelope is published on through the daemon (`pms push --channel`, `Mesh::send`). Subscriptions filter on it, by exact name or glob.

**Content id** — See envelope id.

**CRC trailer** — A CRC32C over bytes 8 to 124 of a Pulse, stored in bytes 124 to 128 of its slot by `publish_with_crc` and checked by `read_slot_verify_crc`. `pms-crc32c` owns the computation.

**Cursor** — The ring's monotonic write counter. The slot a write lands in is the cursor modulo the capacity.

**Daemon** — `mesh-hub`: the local process that terminates the port protocol over a Unix socket and a WebSocket and bridges every connection to the runtime hub. One of the three meanings of hub.

**Detach** — The port-protocol operation that releases a client's name and closes its subscriptions cleanly.

**Developer profile** — A tag, one of `hft`, `ai`, `robotics`, `telemetry` and `dev`, that `pms modules ls --for` and `pms doctor --for` take to select optimization modules. It installs and enables nothing. Not the profiles repository. Each profile's modules are listed in [Modules and developer profiles](/docs/daemon/reference/modules.html).

**Domain tag** — A fixed string mixed into a hash so that identifiers from different contexts can never collide. The envelope id uses `pulse-envelope-id/v2`.

**Envelope** — See PulseEnvelope.

**Envelope id** — The 16-byte BLAKE3 hash of an envelope's canonical bytes under its domain tag. Identical content produces the identical id in Rust, Python, Node, C and WebAssembly; `verify()` recomputes it.

**Envelope reference** — The sixteen-byte `envelope_ref` field of a Pulse: the content id of the PulseEnvelope it names, byte for byte, or all zeros when the Pulse carries none. It has been part of the Pulse since the first commit and chapter 2 of the specification defines it; `Pulse::envelope_id()` reads it as a `PulseEnvelopeId` and `set_envelope_id` writes it. The daemon's runtime fills it on the signal Pulse it writes for each stored envelope, and a second process attached to the hub fetches the envelope by it.

**Envelope store** — The content-addressed shared-memory store that sits beside a ring (`core-store`), where a Pulse's `envelope_ref` resolves to the envelope it names. Layout v2 carries a magic, a version, a claim counter and per-slot sequence words, and holds each envelope as its canonical bytes, verified when read. Emerald in every figure.

**FEC** — Forward error correction in `pulse-wire-udp`: parity datagrams over a window of frames, so a receiver can rebuild a lost frame without asking for it.

**Fixture** — A byte-exact test vector under `pulse/pulse-spec/test-vectors`. Every implementation of the codec must reproduce the fixtures byte for byte; the conformance pages and the website's scrubber are built on them.

**Four-crate ring** — The shape every ring under `rings/` has: `<ring>-core` (shared types), `<ring>-functions` (stateless primitives), `<ring>-stages` (stateful processing) and `<ring>-hub` (the composed surface a consumer depends on). A layer with nothing to hold stays as an empty crate so the shape is uniform.

**Four-question test** — The rule in `CONTRIBUTING.md` for where new code goes: whether it touches the canonical Pulse atom, touches the canonical envelope, crosses a ring boundary or sits on the hot path decides whether it belongs in the inner stack; anything else is peripheral.

**Heartbeat overlay** — The `HeartbeatPayload` that occupies the last 32 bytes of a Pulse whose instruction is `Heartbeat`, read and written through `heartbeat_payload` and `set_heartbeat_payload`.

**Hermes** — The orchestration surface of Sov OS. Not part of this repository.

**Hub** — Three things, and the book says which: the daemon (`mesh-hub`), the runtime event bus it wraps (`PulseHub` in `mesh-hub-runtime`, combining a ring, an envelope store, the schema and broadcast registries and the health registry), and a ring's hub crate (`<ring>-hub`).

**Instance identifier** — Sixteen random bytes a hub draws when it is created or opened, so every handle on a hub name has its own. The hub writes it into an `origin_id` left unset on the Pulses it publishes and skips the envelope signals that carry it, since its own listeners already had those envelopes.

**Instruction** — The 16-bit code in a Pulse that says what it signals: `Null`, `Heartbeat` or `Data` in this tree.

**Knox™** — Cryptographic assertion in the Sovereignty stack (Knox / Musashi). Not part of this repository.

**Kronos™** — The Sovereignty stack's temporal authority: time as a load-bearing coordinate. The monotonic timestamp every Pulse carries and the hub's temporal validation mode are the parts of that discipline present here.

**Lane** — One ring of a MultiLane set (`hft-stages`), named `<prefix>-l<index>`, so several writers can run in parallel without sharing a slot.

**Lap** — One pass of a ring's or a store's write cursor around its slots: `cursor / capacity`. The store's sequence words encode the lap (`2·lap + 1` in progress, `2·lap + 2` published), so a reader can tell that a slot holds the cursor it asked for and not the same slot from an earlier pass. The ring's slot counter serves a poller the same way: each completed write adds two, so a copy whose counter shows more writes than its position's lap needs may hold the next lap and is counted lost.

**Layout version** — The number a shared-memory header carries for its own layout: `PMES` version 2 for the envelope store, `PMSR` version 1 for the ring header. An `open` refuses a region whose version is not its own (`LayoutMismatch`). The ring's header version numbers the header, not the slot format, which is version 1 at 128 bytes or version 2 at 192.

**Lexicon™** — The conformance registry and the Merkle root over its public entries (`mesh-lex`), historically written R-LEX. The lex root anchors the published catalog; the daemon logs it at start.

**Mesh** — The set of processes attached to one hub, and by extension the substrate they share.

**Monotonic nanoseconds** — The `monotonic_nanos` field of a Pulse: a clock reading that must not go backwards within a hub, which the temporal validation mode enforces on the Pulses an application publishes. A hub stamps its own signals with the host's monotonic clock (CLOCK_MONOTONIC on Linux, CLOCK_UPTIME_RAW on macOS) unless the application drives its clock.

**MultiLane** — N rings striped for parallel writers in the HFT ring, with NUMA placement and per-lane writer pinning.

**Musashi™** — Proof-gated execution in the Sovereignty stack: authorization as witnessed mathematics. Not part of this repository.

**NACK** — A 12-byte feedback datagram asking the sender to retransmit one sequence number from its send cache. The receiver stages each missing sequence once, emits at most sixteen per cycle, repeats a request after four cycles and gives up after three; the sender bounds its queue and its retransmits per send.

**Node id** — The 16-byte identity the daemon derives from a client's attach name (`NodeId::from_name`), rendered as `name@hex`.

**Optimization module** — One of twelve capabilities, M01 to M12, each a crate of its own or a module inside one layer of a ring, tagged with the developer profiles it suits. `pms modules ls` lists them, from a static table in the dev ring.

**Origin** — The producer's node id as carried on an envelope event and matched by subscription filters.

**Owl™** — Observation and telemetry in the Sovereignty stack. Not part of this repository.

**Port protocol** — See Universal Port Protocol.

**Profiles (repository)** — `pulse-mesh-profiles`, the repository of the enterprise, HFT and robotics rings, composed on the standard. Not a developer profile.

**Pulse** — The 128-byte atom: eleven fields in two cache lines, the first 8 bytes being the SeqLock word of the slot it sits in. Cyan in every figure.

**Pulse Mesh™** — The wire format and the shared-memory transport this book documents, and the substrate the Sovereignty stack moves through.

**PulseEnvelope™** — The variable-size, content-addressed envelope: a header, a payload that is raw bytes or a nested list of envelopes, and the envelope id computed over the canonical bytes. Violet in every figure.

**PulseRegion** — A shared-memory region holding exactly one Pulse under the SeqLock protocol (`core-region`).

**PulseRing** — The SeqLock ring in `core-ring`: a header and N slots in POSIX shared memory, one writer, whose publishes take its handle's `&mut`, and any number of readers, each through a handle of its own in the writer's process. The header records the ring's magic, header version, capacity and slot size, and `open` refuses a mismatch. Version 1 slots are 128 bytes; version 2 slots are 192 bytes and add a ready line.

**Ready line** — The 8-byte word at bytes 128 to 136 of a version-2 slot that the writer bumps after a publish, so a hardware-wait receiver can monitor it instead of the SeqLock word. A reader may also poll it in place of the counter (`PulseRing::read_position_by_ready`).

**Recovery window** — The range of sequence numbers a UDP receiver may still ask the sender to retransmit: the sender's send-cache capacity, 1,024 frames by default. A gap wider than the window keeps only its newest window of sequences staged; the rest are given up and counted.

**Ring (buffer)** — A PulseRing. When the book means the data structure it says ring buffer or PulseRing.

**Ring (domain surface)** — One of the four tier-3 surfaces: the dev ring, in the daemon repository, and the enterprise, HFT and robotics rings, in the profiles repository. When the book means these it says the dev ring, the HFT ring, and so on.

**RITA™** — Modular role inference in the Sovereignty stack: governance mathematics as a replaceable part. Not part of this repository.

**Sage™** — Deterministic governance in the Sovereignty stack. Not part of this repository.

**Satellite** — An external repository that implements a binding or an adapter against the conformance fixtures. `modules.toml`, in the profiles repository, defines the fields such a repository registers with; it lists none today.

**SeqLock** — The sequence-counter protocol that protects a slot: the writer marks the counter odd, copies the payload, and marks it even; a reader that sees an odd counter, or a counter that changed across its copy, retries for a bounded number of spins and then gives up.

**Slot** — One Pulse-sized cell of a ring, addressed by cursor modulo capacity.

**Sov OS™** — The integrated experimental substrate of the Sovereignty stack, also written sovOS. The hub runtime and the module hub in this tree were vendored from its mesh core.

**Stage** — See four-crate ring.

**Stall deadline** — How long a UDP send may wait for the receiver's cursor under backpressure before it fails with `SenderError::Stalled`: `stall_timeout`, five seconds by default, `None` for no deadline.

**Subscription** — A server-pushed stream of events matching a filter, opened by `subscribe` and identified by the id the ack carries. A tap is a subscription opened from the command line or `Mesh::tap`.

**Thin client** — The Python and Node clients in the daemon repository (`clients/`) that speak the port protocol directly, without the Rust client.

**Tier** — One of the four dependency layers: pulse (0), core (1), mesh (2), rings (3). A tier depends only on the tiers below it; the peripheral surface sits outside them and depends on all of them.

**Type id, type name** — The numeric and the named envelope type. A name-routed envelope carries its name, and its type id is 0 unless the sender gave one; a schema registered for a type id other than 0 checks it, named or not. Filters may match either.

**Universal Port Protocol** — Version 0 of the JSON protocol between the daemon and its clients: the operations attach, detach, send, subscribe, unsubscribe and health, and the replies ack, error, event and health, over a WebSocket (subprotocol `pms-universal-port-v0`) or a length-prefixed Unix socket. The types live in `mesh-port-protocol`.

**WISP™** — The Sovereignty stack's identity primitive. Not part of this repository; the registry at sovfound.org/registry records the mark and its status.

