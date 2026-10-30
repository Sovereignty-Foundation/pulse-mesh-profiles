// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! hft_ring: the `hub` layer, the ring's one import point.
//!
//! Re-exports the ring's `core`, `functions` and `stages` layers and emits
//! the ring's manifest. Downstream code depends on this crate, never on the
//! inner layers.
//!
//! - functions: `functions::wait`, the pluggable wait strategies. The
//!   hardware ones are behind this crate's `umwait`, `mwaitx` and `wfe`
//!   features, which turn on the same features of `hft-functions`.
//! - stages: `stages::multilane` (striped rings with a multiplexer, placed
//!   by `stages::numa`) and `stages::spmc` (one producer, many consumers).
//! - core: `core::prefetch` and `core::wire_v2`, the hardware primitives the
//!   stages build on.
//!
//! In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/hft_ring.html>.
#![forbid(unsafe_code)]

pub use hft_core as core;
pub use hft_functions as functions;
pub use hft_stages as stages;

// `stages::numa` is stage-layer support for multilane, not a stage of its
// own, so it is not registered.
mesh_stage_registry::register_stages! {
    functions: [
        hft_functions::wait,
    ],
    stages: [
        hft_stages::multilane,
        hft_stages::spmc,
    ],
}
