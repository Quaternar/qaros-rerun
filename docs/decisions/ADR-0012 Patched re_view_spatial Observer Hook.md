---
id: ADR-0012
title: Vendored re_view_spatial with a 3D view observer hook
status: accepted
date: 2026-10-05
repo: qaros-rerun
---

# ADR-0012: Vendored re_view_spatial with a 3D view observer hook

## Context

The headset renderer needs the 3D view's draw data every frame. Rerun 0.40.0 offers no public way to get it: the view class registry refuses a duplicate `3D` identifier and has no replace; spatial visualizers branch on that identifier; a visualizer added with `extend_view_class` is skipped unless an entity carries an instruction for it and cannot see other visualizers' output.

## Decision

Vendor `re_view_spatial` 0.40.0 under `patches/`, applied with `[patch.crates-io]`. The patch is small (+59/-1 lines against the crates.io crate): right before the 3D view queues its draw data into its own `ViewBuilder`, it hands the data, view coordinates, scene bounds and desktop camera to an optional process-wide observer (`view_3d_observer.rs`, call site in `ui_3d.rs`). Draw data is copied per eye by downcasting to each known type, since `QueueableDrawData` is not `Clone`.

While an observer is registered, `ExtendedEyeFrustum::last_frame` (`eye.rs`) returns `None`. Rerun skips encoded images, encoded depth images and video frames whose bounds lie fully outside the desktop camera's last frustum (no draw data, only a bounding box); the eyes look elsewhere and would lose them without a warning.

## Alternatives considered

- A custom view or visualizer from Rerun's examples: blocked by the identifier checks above.
- Upstream a hook first (an official per-view render hook, or `App::replace_view_class`): not available in 0.40.0.

## Consequences

- Positive: the least invasive hook found; diffing `patches/re_view_spatial` against the crates.io crate shows the whole change per release.
- Negative: re-applied on every Rerun release until upstream offers a hook.
- Negative: the observer slot is `Send + Sync`, so thread-bound QAROS handles cannot sit behind it (ADR-0017).
- Negative: a new draw data type in a later Rerun release silently disappears from the headset until the copy list is updated (logged once).
- Negative: the viewer always registers the observer, so the desktop view also decodes encoded images and video frames it does not show.

## Evidence and sources

- [HOOK] `patches/re_view_spatial/src/view_3d_observer.rs`, `src/ui_3d.rs`, `src/eye.rs`; the per-eye copy in `crates/qaros-rerun/src/draw_data_copy.rs`.
- [README] `README.md`, "The hook".
- [CULL] `log-test-scene 300 <url> 0 8` puts a magenta PNG-encoded image under a pinhole 8 m behind the default desktop camera. Headless image sink, circle walk, 60 written color images per run, two runs each: without the `last_frame` change none shows magenta, with it 31 do.

[HOOK]: ../../patches/re_view_spatial/src/view_3d_observer.rs "Observer hook"
[README]: ../../README.md "qaros-rerun README"
