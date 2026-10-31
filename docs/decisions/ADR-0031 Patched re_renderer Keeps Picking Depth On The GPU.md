---
id: ADR-0031
title: Vendored re_renderer keeps the picking layer's depth on the GPU
status: accepted
date: 2026-10-08
repo: qaros-rerun
---

# ADR-0031: Vendored re_renderer keeps the picking layer's depth on the GPU

## Context

QAROS reprojects each frame with its depth [RS], so depth goes out with every frame. On the Vulkan path color stays on the GPU, but depth would still make a CPU round trip, because `re_renderer` 0.40.0 keeps its depth private: the main depth buffer is multisampled and stored past the pass only while a volume is queued, and its single-sampled resolve (`DepthResolveProcessor`) runs only then and is not exposed; the picking layer's depth is private and its readback (20 bytes per pixel) always scheduled.

## Decision

Vendor `re_renderer` 0.40.0 under `patches/`. The depth part of the patch is +56/-18 lines; the whole vendored crate differs from crates.io 0.40.0 by +248/-63, with the pool grace and counters of ADR-0045 and the decoupled render patches of ADR-0058, ADR-0059 and ADR-0061. `ViewBuilder::new_with_picking_depth_on_gpu` draws the picking layer without scheduling its readback and makes its depth target sampleable; `ViewBuilder::picking_depth_target` hands it out. The Vulkan output pass samples the reverse-Z depth and converts it in the shader. `ViewBuilder::new` behaves as published, so the CPU path is unchanged. One extra line lets embedded shaders resolve from `patches/re_renderer`.

## Alternatives considered

- Use the main depth buffer: needs sampleable usage, a stored result and a sample choice per pixel, and its silhouette coverage would no longer match the CPU path.
- Read back and re-upload: the round trip this removes.
- Upstream a per-view render hook with sampleable depth: proposed; not available.

## Consequences

- Positive: on a Mac per-frame time falls from 28.8 to 16.6 ms at 960x1000 and from 61.6 to 45.2 ms at 1832x1920, and a frame is shown in the viewer frame its eyes were drawn in.
- Negative: a second vendored Rerun crate to re-apply per release; the picking pass still draws the scene a second time per eye.
- The vendored crates keep Rerun's licence, MIT OR Apache-2.0, with its licence files in each crate's directory.

## Evidence and sources

- [VP] `docs/vulkan-path.md`, "Why the extra pass", Mac evidence.
- [PATCH] `patches/re_renderer/src/view_builder.rs` (`new_with_picking_depth_on_gpu`, `picking_depth_target`); `crates/qaros-rerun/src/eye_output.rs`, `eye_output.wgsl`.
- [PC] `patches/re_renderer` diffed against the `re_renderer` 0.40.0 crate from crates.io.
- [RS] QAROS late-stage reprojection (warping) with the frame's depth: the mixer page and the glossary at `b1338df`.
- [LIC] `README.md`, "License".

[VP]: ../vulkan-path.md "The QAROS Vulkan path"
[PATCH]: ../../patches/re_renderer/src/view_builder.rs "Patched ViewBuilder"
[PC]: https://crates.io/crates/re_renderer/0.40.0 "re_renderer 0.40.0"
[RS]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/operator-guide/the-mixer.md "QAROS mixer: late-stage reprojection"
[LIC]: ../../README.md "qaros-rerun README"
