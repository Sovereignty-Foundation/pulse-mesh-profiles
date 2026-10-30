// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! robotics_ring: the `functions` layer, empty by design.
//!
//! The ring's three primitives all carry state and live in `stages`; none is
//! a stateless function. The crate stays so that the four-crate shape reads
//! the same in every ring, and it is where such functions land when the ring
//! gains some. In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/robotics_ring.html>.
#![forbid(unsafe_code)]
#![no_std]
