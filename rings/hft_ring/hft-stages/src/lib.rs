// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Sovereignty Foundation
//! hft_ring: the `stages` layer, stateful ring primitives on the hot path.
//!
//! - `numa`: hugepage shared-memory backings and the NUMA placement of
//!   memory and threads, `core_region::numa` re-exported here, where the
//!   ring that places its lanes with it lives.
//! - `multilane`: N rings, one writer each, read round-robin by one reader,
//!   placed on NUMA nodes through `numa`.
//! - `spmc`: one producer and many consumers, each with its own cursor, so
//!   a publish reaches every reader without a copy per reader.
//!
//! `multilane` and `spmc` share one overrun rule, stated in the private
//! `overrun` module: a reader the writer has lapped counts what it lost and
//! never receives a newer lap under an older position.
//!
//! Imports come through the hub: `hft_hub::stages::multilane` and its siblings.
//!
//! In the book: <https://pulse-mesh.net/docs/profiles/architecture/rings/hft_ring.html>.

pub mod multilane;
pub mod numa;
mod overrun;
pub mod spmc;
