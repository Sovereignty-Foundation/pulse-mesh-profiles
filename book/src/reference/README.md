<div class="pm-part">
  <img class="pm-logo pm-logo-light" src="../images/sov-mark-black.svg" alt="Sovereignty Foundation Inc.">
  <img class="pm-logo pm-logo-dark" src="../images/sov-mark-white.svg" alt="">
  <span class="pm-eyebrow">Part 03 · Reference</span>
  <h1>What is <span class="pm-accent">exactly</span> true</h1>
  <p class="pm-sub">Each ring crate by crate, and the tables generated from the code.</p>
</div>

<p class="pm-kind"><b>Part opener</b> · for implementers and integrators looking something up</p>

Reference pages state what is true and nothing else. They do not teach and they do not persuade. Where a table can be produced from the code, it is, and a check in continuous integration fails when the page and the code disagree.

The specification and the API pages are [the standard's](/docs/reference/README.html); this book cites the specification by chapter. The workspace pages here describe the rings, following the tree's own structure, and the generated pages are produced from the code by the gate.

- Workspace: [Layout](../architecture/overview.md), [Rings: domain surfaces](../architecture/rings/overview.md), [enterprise_ring](../architecture/rings/enterprise_ring.md), [hft_ring](../architecture/rings/hft_ring.md), [robotics_ring](../architecture/rings/robotics_ring.md). The dev ring's page is [in the daemon's book](/docs/daemon/architecture/rings/dev_ring.html).
- Generated from the code: the [crate index](crates.md) and the [error catalogue](errors.md); the gate regenerates them and fails on any difference.
- [Glossary](../glossary.md).
