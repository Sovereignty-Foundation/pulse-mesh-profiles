// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! robotics_ring: the `stages` layer, stateful delivery primitives on the hot path.
//!
//! - `large_payload`: an out-of-band shared-memory segment pool for
//!   multi-megabyte payloads such as LIDAR scans and video frames, which
//!   processes map by name; a Pulse carries a 16-byte reference to the
//!   payload.
//! - `net_uring`: a UDP transport over io_uring, Linux only, whose batches
//!   go out on one submission, and a pool of persistent TCP flows, one per
//!   peer, both carrying the bytes their caller hands them; for AF_XDP, see
//!   the `pulse-wire-xdp` plugin.
//! - `autoscale`: ring sizing driven by observed receiver lag, applied by
//!   replacing the ring under its name, which readers follow.
//!
//! Imports come through the hub: `robotics_hub::stages`.
//!
//! In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/robotics_ring.html>.

pub mod autoscale;
pub mod large_payload;
pub mod net_uring;
