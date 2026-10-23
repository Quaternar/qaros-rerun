---
id: ADR-0014
title: Off-axis eye projection as Rerun's symmetric perspective plus a viewport transformation
status: accepted
date: 2026-10-05
repo: qaros-rerun
---

# ADR-0014: Off-axis eye projection as Rerun's symmetric perspective plus a viewport transformation

## Context

QAROS hands each eye an OpenXR-style pose and a four-angle, asymmetric field of view. `re_renderer` supports symmetric perspective only, and some shaders assume a centered principal point.

## Decision

`xr-math`, a pure crate with no Rerun or QAROS dependency, decomposes the off-axis projection into `re_renderer`'s symmetric perspective plus a translating viewport transformation, proven equal to the explicit off-axis matrix. The same crate holds `Pose`, `Fov`, `NearFar` mirroring the QAROS types, view coordinates to OpenXR room axes (left-handed view coordinates rejected), and app-volume placement with scale. A test in `qaros-rerun` checks the decomposition against `re_renderer`'s own `Projection` and `RectTransform` matrices.

## Alternatives considered

- Patch `re_renderer` with a frustum projection (`Projection::Frustum`, proposed upstream): not done, to keep the projection out of the vendored Rerun patches.

## Consequences

- Positive: a 1 m cube measured within a pixel of the expected geometry in both eyes; spheres look elongated near the edge, which is the correct rectilinear projection, not a defect.
- Negative: shaders that assume a centered principal point (Gaussian splats) are untested; screen-space point sizes assume a symmetric field of view.
- Negative: a change in how `ViewBuilder` combines the two matrices would not fail the test.

## Evidence and sources

- [XRM] `crates/xr-math/src/projection.rs`, `eye.rs`, `pose.rs`, `coordinates.rs`, `placement.rs`.
- [README] `README.md`, crate table, "Evidence from a Mac", "Tests".

[XRM]: ../../crates/xr-math/src/projection.rs "Off-axis projection"
[README]: ../../README.md "qaros-rerun README"
