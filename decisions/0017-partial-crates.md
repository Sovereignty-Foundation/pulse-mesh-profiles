# ADR-0017 — Partial crates say so, and the architecture check holds them to it

**Status:** Accepted
**Date:** 2026-10-17
**Amended:** 2026-10-17: the architecture check enforces the rule (`partial`, rule 7 of `scripts/arch-check.py`), now that every crate it names carries the status.

## Context

ADR-0014 gave crates the tree ships but does not use a status, `pre-integration`, in their manifest and their documentation. A second kind of crate had no such marker: one that is used, whose API is in place, but some of whose public entry points return an error saying the work behind them is deferred. In this release that is `enterprise-stages` (both TLS entry points return `TlsError::IntegrationDeferred`), `pulse-wire-xdp` (both constructors return `XdpError::IntegrationDeferred` on Linux) and `robotics-stages` (`net_uring`'s bind, send and receive and `FlowPool::acquire` return `NetUringError::NotYetImplemented`). Each said "skeleton" or "scaffold" somewhere in its prose, but a reader of the crate index, an evaluator running an example, or a tool could not tell such a crate from a finished one, and nothing kept a new deferred entry point from shipping unmarked. Examples built on these crates read as demonstrations of working features.

## Decision

A crate whose sources return a deferred error carries `[package.metadata.pulse-mesh] status = "partial"` in its manifest, or `pre-integration` when ADR-0014 applies, and opens its crate documentation with a line beginning `**Status: partial.**` that names what is deferred. A deferred error is a value of a variant named `IntegrationDeferred` or `NotYetImplemented` returned as an `Err`. The generated crate index shows the status in its status column. The architecture check gains a rule, `partial`: a crate whose `src/` returns a deferred error and declares neither status, or declares one without the opening status line, is refused. Only `pre-integration` excuses a library from the consumers rule; `partial` describes a crate that is used.

## Consequences

The tree says, in the manifest and at the top of the crate docs, which crates have entry points that do not work yet, and a deferred entry point cannot be added without saying so. Finishing the last deferred entry point of a crate is also the removal of its status line, which the check then requires: a crate whose sources no longer return a deferred error and still declares `partial` is refused, so the label cannot outlive the gap. A crate that reports a capability the host lacks (`PlatformUnsupported`, `NotSupported`) is not partial: that error is the finished behaviour on that host.
