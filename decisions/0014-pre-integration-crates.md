# ADR-0014 — Pre-integration crates say so

**Status:** Accepted
**Date:** 2026-09-25

## Context

Three tier-2 crates are vendored from sovOS™ and used by nothing in the tree: `mesh-capability` (the 48-byte Hello and its negotiation outcome), `mesh-context` (the context registry for the high half of a Pulse's scope) and `mesh-module-hub` (a node's private hub beside the shared one). Each is complete in itself and tested, and each describes a mechanism the rest of the tree does not exercise: no transport sends the Hello (`pulse-wire-udp` speaks the data frames alone), nothing resolves a context id through the registry, and the daemon runs one shared hub and no module hubs. `mesh-capability`'s documentation read as if the handshake were in force. A reader of the crate index could not tell a wired-in crate from one waiting for its integration, and the architecture check (ARC-12) needs to know which crates are allowed to have no dependent.

## Decision

The three crates ship, and say what they are. Each carries `[package.metadata.pulse-mesh] status = "pre-integration"` in its manifest, opens its crate documentation with the status, the fact that nothing uses it yet, and what would integrate it, and is listed with that status in the generated crate index (a status column read from `cargo metadata`). The architecture check accepts a crate with no dependent when it is a binary, an example, a tool, or declares this status; every other library crate must be depended on. The crates are not moved, renamed or feature-gated: their tier is where they will integrate, and their tests keep running in the gate.

## Consequences

The tree states its own integration state in one place per crate, readable by people (the docs) and by tools (the manifest). Integrating one of the three is the removal of its status line, the sentence in its docs, and the dependency edge that does the work; nothing else changes shape. A fourth crate vendored ahead of its use takes the same status. `mesh-capability`'s documentation describes the Hello as what two peers would exchange, and names the transport that does not send it yet.
