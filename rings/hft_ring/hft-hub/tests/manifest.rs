// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! The hub's manifest matches the composition it declares and re-exports.

#[test]
fn manifest_lists_the_declared_composition() {
    let m = hft_hub::RING_MANIFEST;
    assert_eq!(m[0], ("functions", &["hft_functions::wait"][..]));
    assert_eq!(
        m[1],
        ("stages", &["hft_stages::multilane", "hft_stages::spmc"][..])
    );
}
