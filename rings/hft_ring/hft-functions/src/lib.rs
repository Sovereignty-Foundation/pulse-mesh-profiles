// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! hft_ring: the `functions` layer, pluggable wait strategies.
//!
//! - `wait`: the `WaitStrategy` trait, `BusySpin`, `Park`, and the hardware
//!   strategies behind the opt-in `umwait`, `mwaitx` and `wfe` features,
//!   with detection of the best one for the host.
//!
//! Imports come through the hub: `hft_hub::functions::wait`.
//!
//! In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/hft_ring.html>.

pub mod wait;
