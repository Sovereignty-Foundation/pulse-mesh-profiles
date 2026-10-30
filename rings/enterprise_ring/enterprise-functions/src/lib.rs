// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! enterprise_ring: the `functions` layer, empty by design.
//!
//! The ring's one primitive, the TLS transport, carries session state and
//! lives in `stages`; nothing is a stateless function. The crate stays so that
//! the four-crate shape reads the same in every ring, and it is where such
//! functions land when the ring gains some. In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/enterprise_ring.html>.
#![forbid(unsafe_code)]
#![no_std]
