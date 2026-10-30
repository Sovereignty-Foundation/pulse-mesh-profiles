# `hft_ring/`

The latency ring, with all four crates populated: `hft-core` holds the prefetch hints and the `wire_v2` layout constants, `hft-functions` the wait strategies behind `WaitStrategy` with CPU-feature detection, `hft-stages` the NUMA placement, the multilane multiplexer and the SPMC ring, and `hft-hub` composes them. Depend on `hft-hub`. The book's page is [hft ring](https://pulse-mesh.net/docs/profiles/architecture/rings/hft_ring.html); the crates are in the generated [crate index](https://pulse-mesh.net/docs/profiles/reference/crates.html).
