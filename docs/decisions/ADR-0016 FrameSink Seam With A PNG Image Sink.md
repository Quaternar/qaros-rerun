---
id: ADR-0016
title: FrameSink seam between the headset renderer and QAROS, with a PNG image sink
status: accepted
date: 2026-10-05
repo: qaros-rerun
---

# ADR-0016: FrameSink seam between the headset renderer and QAROS, with a PNG image sink

## Context

The renderer must be verifiable without QAROS (projection, depth conversion, coordinate frames, on a Mac), and frames leave it either over a CPU copy or on the GPU.

## Decision

The `FrameSink` trait, shaped after the QAROS render sender loop (begin with per-eye pose and field of view, submit color and `f32` depth per eye, show with near, far and depth scale, abandon), is the only seam between the headset renderer and any output. `ImageSink` writes side-by-side color, a depth visualization and per-frame metadata, with `ScriptedHeadMotion` for runs without a headset. The contract: every begun frame ends in exactly one show or one abandon. `GpuFrameSink` (feature `gpu`) extends the seam for sinks whose textures the renderer draws into, and `GpuReadbackSink` runs the GPU path into PNGs.

## Alternatives considered

- Render straight into QAROS from the renderer: swapping the CPU copy for the Vulkan path would then change the renderer; behind one interface it stays a local change.

## Consequences

- Positive: geometry, stereo and depth are proven on a Mac with images; the Vulkan path reuses the renderer unchanged, and its PNGs are byte-identical to the CPU path's.
- Positive: multi-user (ADR-0020) and frame keys (ADR-0022) slot into the trait.
- Negative: two seams exist (CPU and GPU sinks) and each change touches both.

## Evidence and sources

- [FS] `crates/frame-sink/src/lib.rs`, `image_sink.rs`, `pose_source.rs`, `gpu.rs`, `gpu_readback.rs`.
- [README] `README.md`, "The seam".
- [EV] `docs/evidence/` (stereo frames from the image sink, GPU path frames).

[FS]: ../../crates/frame-sink/src/lib.rs "frame-sink"
[README]: ../../README.md "qaros-rerun README"
[EV]: ../evidence "Evidence"
