---
id: ADR-0038
title: App content pose space by default, near and far handed back in app units
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0038: App content pose space by default, near and far handed back in app units

## Context

The `b1338df` header (L150-158, citing QuaternAR's ADR-0253) states that a render sender bound to an app volume hands out view poses, hands and the rendered pose in that volume's app content space, in app units; `QarNearFar` and `rendered_near_far` are in the units of the poses (L458-460, L1996).

## Decision

`--pose-space app` is the default. `PoseMapping` (shared by the CPU and Vulkan sinks) maps each pose into the room with the volume's placement (volume pose times `app_pose`, position times `app_scale`), so the renderer keeps working in room meters; near and far go back divided by the app scale, and so does linear depth's `depth_scale`. The volume the sink creates and the renderer's placement share one set of values (`--origin`, `--yaw`, `--scale`). `--pose-space room` remains as an option.

## Alternatives considered

- Room space by default: contradicts the header.
- Render in app space directly: rationale for keeping the renderer in room meters is not recorded; the effect is that depth encoding and placement code stay unchanged.

## Consequences

- Positive: matches the documented contract; stub tests check the mapping and app-unit near and far at app scale 2.
- Negative: unverified on real head poses; whether linear `depth_scale` should carry the room-to-app factor is ambiguous in the header (the Vulkan path refuses linear depth anyway).

## Evidence and sources

- [HDR] QAROS header at `b1338df`, `qar-streaming-c/include/qar_streaming.h` L150-158, L458-460, L1996.
- [OPT] `crates/qaros-rerun/src/options.rs` (`PoseSpaceKind::App`).
- [MAP] `crates/qaros-sink/src/sink.rs` (`PoseMapping`).

[HDR]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[OPT]: ../../crates/qaros-rerun/src/options.rs "Viewer options"
[MAP]: ../../crates/qaros-sink/src/sink.rs "QAROS sink"
