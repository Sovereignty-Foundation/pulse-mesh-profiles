// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! The hub's manifest matches the composition it declares and re-exports.

#[test]
fn manifest_lists_the_declared_composition() {
    let m = enterprise_hub::RING_MANIFEST;
    assert_eq!(m[0], ("functions", &[][..]));
    assert_eq!(m[1], ("stages", &["enterprise_stages::tls"][..]));
}
