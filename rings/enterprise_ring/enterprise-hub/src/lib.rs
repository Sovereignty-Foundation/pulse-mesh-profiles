// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! enterprise_ring: the `hub` layer, the ring's one import point.
//!
//! Re-exports the ring's `core`, `functions` and `stages` layers and
//! registers them with `mesh_stage_registry::register_stages!`, which emits
//! the ring's manifest. Downstream code depends on this crate, never on the
//! inner layers.
//!
//! In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/enterprise_ring.html>.
#![forbid(unsafe_code)]

pub use enterprise_core as core;
pub use enterprise_functions as functions;
pub use enterprise_stages as stages;

// `RING_MANIFEST`, which `tests/manifest.rs` checks.
mesh_stage_registry::register_stages! {
    functions: [],
    stages: [
        enterprise_stages::tls,
    ],
}
