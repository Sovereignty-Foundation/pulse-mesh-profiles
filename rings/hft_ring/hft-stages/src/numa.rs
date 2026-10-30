// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! `hft_stages::numa`: hugepage shared-memory backings and the NUMA
//! placement of memory and threads (module M01), as the HFT ring reaches
//! them. The code is `core_region::numa`, in the core layer, since any ring
//! or store can be placed with it; this module re-exports it whole.

pub use core_region::numa::*;
