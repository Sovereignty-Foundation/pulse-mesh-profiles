// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! `cfg(xdp)`: the target the datapath builds for, 64-bit little-endian
//! Linux. Elsewhere every constructor returns `PlatformUnsupported`: the
//! hand-assembled program's encoding assumes a little-endian host, and the
//! rings' mapping offsets reach past a 32-bit `off_t`.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(xdp)");
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    if var("CARGO_CFG_TARGET_OS") == "linux"
        && var("CARGO_CFG_TARGET_POINTER_WIDTH") == "64"
        && var("CARGO_CFG_TARGET_ENDIAN") == "little"
    {
        println!("cargo::rustc-cfg=xdp");
    }
}
