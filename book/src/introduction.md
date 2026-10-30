<div class="pm-hero">
  <img class="pm-logo pm-logo-light" src="images/sov-mark-black.svg" alt="Sovereignty Foundation Inc.">
  <img class="pm-logo pm-logo-dark" src="images/sov-mark-white.svg" alt="">
  <span class="pm-eyebrow">Sovereignty Foundation Inc. · The Pulse Mesh™ Profiles Book</span>
  <h1>Pulse <span class="pm-accent">Mesh</span><sup class="pm-tm">™</sup> Profiles</h1>
  <p class="pm-sub">The rings on the standard: the enterprise, HFT and robotics profiles, each the same four crates composed on the substrate.</p>
  <p class="pm-ident">Pulse Mesh Profiles · working draft · reference implementation</p>
  <div class="pm-status">Nothing in this book is a release announcement. Licensing questions go to <a href="mailto:legal@sovfound.org">legal@sovfound.org</a>.</div>
  <div class="pm-cta-row">
    <a class="pm-cta pm-cta-primary" href="start/onboard-your-agent.html">Onboard your agent</a>
    <a class="pm-cta pm-cta-secondary" href="architecture/rings/overview.html">The rings</a>
    <a class="pm-cta pm-cta-secondary" href="/docs/daemon/guides/add-a-stage.html">Add a stage</a>
  </div>
</div>

Pulse Mesh™ is a transport substrate built on two byte layouts. The **Pulse** is a 128-byte control signal that occupies two cache lines and moves through a shared-memory ring without a lock. The **PulseEnvelope** carries anything larger, at any size, and is addressed by the 16-byte BLAKE3 hash of its canonical bytes, so the same content produces the same identifier in every language that implements the codec. The two layouts, their codec, the ring, the store and the wire are [the standard](/docs/introduction.html), a repository of its own; this one builds on it.

This repository is the profiles: three domain rings composed on the standard, each the same four crates. The enterprise ring holds the TLS transport stage: TLS 1.3 over TCP works, and its DTLS and QUIC-tunnel modes are deferred. The HFT ring holds the wait strategies, the MultiLane ring, the SPMC ring and NUMA placement. The robotics ring holds the large-payload pool, the io_uring transport and the autoscale stage. Beside them sit the AF_XDP transport plugin, the HFT telemetry tool, and one runnable example per ring that has a runnable stage. The fourth ring, the general-purpose dev ring, is [in the daemon's repository](/docs/daemon/architecture/rings/dev_ring.html) with the daemon it serves. This repository depends on the standard as a sibling checkout (`../pulse-mesh`) while developing and as a pinned tag at a release.

This book documents the rings in this repository. It is organised in four parts, each answering one kind of question, and every page states at its top which kind it is and whom it is for.

## The four parts

| Part | The question it answers | How to read it |
|---|---|---|
| [Examples](examples/README.md) | How do I see it working? | The three examples are in the standard's and the daemon's books; this opener says where. |
| [Workflows](workflows/README.md) | How do I do my domain's job with it? | An example's shape for one domain, saying what is implemented and what is deferred. |
| [How-to guides](guides/README.md) | How do I do this particular task? | Find the task, follow the steps. |
| [Reference](reference/README.md) | What exactly is true? | Look things up. Each ring, crate by crate, and the generated tables. |
| [Concepts](explanation/README.md) | Why is it built this way? | Read for understanding. The shape of the rings, and the decision log. |

A last part, [Contributing](contributing/README.md), is for people changing the tree: the workspace, the gates, the decision record, and the documentation's own rules.

## Where to start

- **Seeing a ring run:** each ring's page names its example and the command that runs it; [hft_ring](architecture/rings/hft_ring.md) is the one with the most in it.
- **Adding to a ring:** [Add a stage to a ring](/docs/daemon/guides/add-a-stage.html).
- **Understanding how the pieces fit:** [Rings: domain surfaces](architecture/rings/overview.md) for the four-crate shape, and the [Concepts](explanation/README.md) part for where the rings sit above the standard and the daemon.
- **Contributing:** [Developer Onboarding](contributing/onboarding.md), then the [documentation style](contributing/style.md) if you are writing.

## How this book relates to the rest of the repository

Two records are authoritative, and this book defers to both. The rustdoc (`cargo doc --workspace --no-deps --open`) is the API contract: every crate opens with an overview, every public item is documented, and a workspace lint keeps it that way. The decision log under `decisions/` records why the non-obvious calls were made, in order and without rewriting. The book is the guided path through that material. Where the book and the code disagree, one of them is wrong, and either way it is a defect to report.

The specification is the standard's, and this book cites it by chapter.

## What this book claims and what it does not

The book describes the tree at the version shown in the sidebar. It records no measured figures. Where it describes a measurement it gives the command that produces one. Where a behaviour is specified but not yet implemented, the page says so and names the test that will prove it. Refutations are welcome: the fastest way to improve a page is to show where it is wrong.
