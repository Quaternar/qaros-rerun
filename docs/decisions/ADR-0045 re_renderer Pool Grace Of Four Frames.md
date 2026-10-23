---
id: ADR-0045
title: Vendored re_renderer keeps unused pooled resources for four frames
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0045: Vendored re_renderer keeps unused pooled resources for four frames

## Context

At 1832x1920 `ViewBuilder::new` took 5 ms on average and up to 13 ms (under 1 ms at 960x1000). re_renderer's published `DynamicResourcePool` (unchanged from 0.38.1 to 0.40.0) keeps a released texture reusable for one frame only, and users begin a frame every two to three viewer frames, so the eye views' full-resolution targets were destroyed and created again for almost every eye frame. The vendored re_renderer (ADR-0031) already carried a patch.

## Decision

Patch `DynamicResourcePool` so unused resources stay reusable for `REUSE_GRACE_FRAMES = 4` frames. Four is chosen from data: a creation counter (`POOL_RESOURCES_CREATED`) and the override `QAROS_RERUN_POOL_GRACE` measured grace 1, 2, 3, 4 and 8 at 1832x1920 with two targets and Vulkan receivers, three runs each, and 4 is the smallest value at the floor of pool creations. The override stays for measuring; it is not a user setting.

## Alternatives considered

- Grace 8: no fewer creations than 4.
- Grace 2 or 3: about 9 and 7 to 9 creations per shown frame against 5.7 to 6 at 4.
- Keep the eye targets outside the pool in the headset renderer: re_renderer allocates the view's targets inside `ViewBuilder::new`, so that would be a larger patch.

## Consequences

- Positive: pool creations per shown frame fell from 25 to 30 (grace 1) to 5.7 to 6; `ViewBuilder::new` from 1.6 to 2.5 ms to 0.15 to 0.2 ms; with the headless loop of ADR-0044 the Vulkan path reached 45.9 / 58.4 frames/s at 1832x1920 one target (CPU / Vulkan receiver).
- Positive: no measurable VRAM increase on an 8 GB GPU (median 3.2 to 3.4 GB at grace 2 to 8 against 3.3 to 3.4 GB at 1).
- Negative: the vendored re_renderer patch widens (`dynamic_resource_pool.rs`, plus the two counters in `context.rs` and their exports), one more place to re-apply on every Rerun upgrade.
- Negative: applies to every pooled resource of the viewer, not only the eye targets.

## Evidence and sources

- [CODE] `patches/re_renderer/src/wgpu_resources/dynamic_resource_pool.rs` (`REUSE_GRACE_FRAMES`, `POOL_RESOURCES_CREATED`, `QAROS_RERUN_POOL_GRACE`).

[CODE]: ../../patches/re_renderer/src/wgpu_resources/dynamic_resource_pool.rs "Dynamic resource pool"
