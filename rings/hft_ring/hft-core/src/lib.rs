// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! hft_ring: the `core` layer, hardware primitives with no state and no I/O.
//!
//! - `prefetch`: adaptive cache-line prefetch hints, inline CPU instructions.
//! - `wire_v2`: the version-2 slot constants, defined from `core-ring`'s so
//!   the two cannot drift, with the per-slot cache-line separation they imply.
//!
//! Imports come through the hub: `hft_hub::core::prefetch` and
//! `hft_hub::core::wire_v2`.
//!
//! In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/hft_ring.html>.

pub mod prefetch;
pub mod wire_v2;
