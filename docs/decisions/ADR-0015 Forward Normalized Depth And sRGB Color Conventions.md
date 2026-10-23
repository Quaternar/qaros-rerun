---
id: ADR-0015
title: Forward normalized depth with a finite far plane, sRGB-encoded color
status: accepted
date: 2026-10-05
repo: qaros-rerun
---

# ADR-0015: Forward normalized depth with a finite far plane, sRGB-encoded color

## Context

QAROS reprojects with the depth it receives, but its documentation does not define what a depth pixel holds, what `depth_scale` scales, or whether 8-bit color is sRGB or linear. The header at `b1338df` states the units of near and far and how reverse-Z is signalled. Rerun renders infinite reverse-Z.

## Decision

- Depth goes out as D3D/OpenXR-style forward normalized depth with a finite far plane (0 at near, 1 at far, Rerun's background at far), `depth_scale` 1.0, mirroring `XrCompositionLayerDepthInfoKHR`. Reverse normalized and linear meters are one flag away (`--depth`); linear is CPU-only, since a depth image holds normalized values.
- Color is sRGB-encoded bytes with premultiplied alpha (ADR-0050), BGRA8 requested by default (`--color-format`), D32 depth.
- Uncovered pixels are transparent from depth coverage, so only Rerun's content reaches the room.

## Alternatives considered

- Reverse-Z or linear meters as the default: kept behind the flag; the public Vulkan cube example uses forward `[0, 1]` with an OpenXR-style projection, which matches this default.

## Consequences

- Positive: the convention switches in one place (`--depth`, `crates/xr-math/src/depth.rs`); the `b1338df` header's near and far units and reverse-Z signalling are consistent with this choice.
- Positive: `B8G8R8A8_UNORM` texels arrive bit-exact on the Windows QAROS package.
- Negative: not yet seen in the glasses; the documentation does not define whether depth is normalized or linear, or whether an infinite far plane is allowed.
- Negative: depth edges are not antialiased while color edges are; about a 1 px dark edge on silhouettes.

## Evidence and sources

- [README] `README.md`, "Coordinates and depth".
- [DEP] `crates/xr-math/src/depth.rs` (reasoning with QAROS header line references).
- [CUBE] QAROS Vulkan cube example at `b1338df`.

[README]: ../../README.md "qaros-rerun README"
[DEP]: ../../crates/xr-math/src/depth.rs "Depth encodings"
[CUBE]: https://github.com/Quaternar/qaros/tree/b1338df/qar-streaming-c/examples/vulkan_cube "QAROS Vulkan cube example"
