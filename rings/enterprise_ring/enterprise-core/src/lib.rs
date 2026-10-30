// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! enterprise_ring: the `core` layer, empty by design.
//!
//! The ring's one primitive, the TLS transport, carries session state and
//! lives in `stages`, sharing no types. The crate stays so that the four-crate
//! shape reads the same in every ring, and it is where such types land when
//! the ring gains some. In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/enterprise_ring.html>.
#![forbid(unsafe_code)]
#![no_std]
