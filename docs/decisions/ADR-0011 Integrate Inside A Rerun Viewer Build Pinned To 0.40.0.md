---
id: ADR-0011
title: Integrate inside a Rerun viewer build pinned to 0.40.0
status: accepted
date: 2026-10-05
repo: qaros-rerun
---

# ADR-0011: Integrate inside a Rerun viewer build pinned to 0.40.0

## Context

A Rerun recording logged the usual way (Python, Rust, C++) must appear in the glasses as head-tracked stereo with depth, while the desktop viewer keeps working and users change nothing in their code. Rerun has no runtime plugin mechanism; its extensibility tracking issue rates customizing the spatial view "level 0: fork and modify code" [rerun#3087].

## Decision

Build a separate viewer binary, `qaros-rerun`, from the Rerun 0.40.0 crates, with a headset renderer that runs inside the viewer's own frame and reuses the viewer's data store, time cursor and 3D view. Any Rerun SDK connects to it on the standard port 9876. Each release pins one Rerun version exactly (`rerun` and `re_*` =0.40.0, egui =0.37.0, wgpu =30.0.0); the workspace pins Rust 1.96, which Rerun 0.40.0 requires.

## Alternatives considered

- A separate process rendering Rerun data: would have to rebuild the data store, time cursor and scene visualizers the viewer already holds.
- Ship inside the stock viewer: not possible without an upstream hook.
- A custom view or visualizer from Rerun's extension examples: a wrapper view cannot reuse the `3D` identifier and visualizers cannot see each other's output (ADR-0012).
- Why 0.40.0: the newest release on crates.io; every vendored patch applies to it without conflicts, and wgpu stays 30.0.0.

## Consequences

- Positive: one source of truth for time; the desktop viewer is unchanged with no stream.
- Negative: coupling to Rerun internals; every Rerun release needs the vendored patches re-applied (ADR-0012, ADR-0031).
- Negative: Rerun's device setup is crate-private, so the viewer copies Rerun's setup and headless harness to own the wgpu device (ADR-0025).
- Negative: a release build from clean takes about 25 minutes on a laptop.

## Evidence and sources

- [rerun#3087] Rerun extensibility tracking issue.
- [TC] `rust-toolchain.toml` (channel 1.96), `Cargo.toml` (`rust-version = "1.96"`, the exact Rerun, egui and wgpu pins).
- [CRATES] The `rerun` crate's release list on crates.io.
- [README] `README.md`, "How it works".

[rerun#3087]: https://github.com/rerun-io/rerun/issues/3087 "Rerun extensibility tracking issue"
[TC]: ../../rust-toolchain.toml "Toolchain pin"
[CRATES]: https://crates.io/crates/rerun/versions "rerun on crates.io"
[README]: ../../README.md "qaros-rerun README"
