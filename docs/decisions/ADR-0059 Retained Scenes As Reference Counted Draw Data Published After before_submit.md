---
id: ADR-0059
title: Retained scenes are reference-counted draw data copies published after before_submit
status: proposed
date: 2026-10-19
repo: qaros-rerun
---

# ADR-0059: Retained scenes are reference-counted draw data copies published after before_submit

## Context

The render thread (ADR-0058) draws a scene that the UI thread built in an earlier `re_renderer` frame of another context. `re_renderer` documents draw data as built for one frame ("Expected to be recreated every frame"), and its resources come from per-frame pools and a per-frame CPU write belt.

Reading the 0.40.0 sources: pools recycle by reference count, not by time (`DynamicResourcePool::begin_frame` reclaims a resource only once the pool holds its last `Arc`); every draw data type holds its GPU resources through `GpuBindGroup`, which keeps strong handles of the buffers and textures it binds, or through `GpuBuffer`, `GpuTexture` and `Arc<GpuMesh>` fields; the belt is only staging, copied into pool resources in the frame's "before view builder" encoder, which `before_submit` submits. The volume draw data of 0.40.0 holds its 3D texture through its `GpuBindGroup`. Two draw data types also share a per-frame `SortedDrawables` list between all their clones (`Arc<Mutex<_>>`), cleared whenever a view of another frame index collects into it, and indexed by the draw.

## Decision

- A retained scene is a copy of the streamed view's draw data (`draw_data_copy::copy_all`, `Arc` clones), with the view's placement and seed, taken by the observer in the UI pass and published after the UI pass, once Rerun's `before_submit` has submitted the uploads it depends on. A viewer frame that did not draw the view withdraws the scene.
- The hand-off is a mutex over `Option<Arc<RetainedScene>>` with a condvar. The render thread takes its own `Arc` once per eye frame and drops it after that frame's submissions, so a replaced scene's resources return to Rerun's pools only after the last eye frame that drew them was submitted; any later write to them is a later submission on the same queue.
- `PointCloudDrawData` and `GaussianSplatDrawData` get a hand-written `Clone` in the vendored `re_renderer`: a copy starts its own `SortedDrawables`. Without it, Rerun's desktop view (drawn in the paint callback after the hand-off, frame index N) and the eye views (frame index M) clear and refill one list from two threads, which ends in a wrong batch or an index panic.

## Alternatives considered

- Deep copies of the GPU data per scene: doubles memory and upload bandwidth for every scene and needs access to private draw data fields.
- Persistent ("retained") variants of the draw data types or of the belt: a larger patch across every renderer, not needed while pools recycle by reference count.
- Publishing inside the observer: the scene's uploads would not be submitted yet, and an eye frame could draw before them.

## Consequences

- Positive: no copy of GPU data; a scene costs `Arc` clones (0.1 ms for a 5000-entity scene on the Mac).
- Positive: a GPU test retains a scene of an opaque point, sorted transparent points, sorted gaussian splats and a volume, runs the first context 12 more frames that recycle its pools, then draws copies of the retained scene from a second context on another thread while the first context, on a third thread, draws copies of it and new scenes in turns; every image must be byte-identical to the first context's reference [TEST]. The concurrent part catches the shared `SortedDrawables` only by chance: with the hand-written `Clone` reverted it failed 2 of 10 runs on the Mac.
- Negative: rests on undocumented properties of `re_renderer` 0.40.0; an upgrade must re-check them (the test covers point clouds, splats and volumes).
- Negative: resources Rerun rewrites in place (video frame textures) can change under a retained scene; the eye then shows newer content than the rest of the scene.

## Evidence and sources

- [DOC] `docs/decoupled-render.md`, "Retained draw data and resource lifetimes".
- [TEST] `crates/qaros-rerun/src/decoupled.rs`, `retained_draw_data_draws_unchanged_from_a_second_context_on_another_thread`.
- [RR] `patches/re_renderer/src/wgpu_resources/dynamic_resource_pool.rs`, `bind_group_pool.rs`, `allocator/uniform_buffer_fill.rs`, `transparent_sort.rs`, `renderer/point_cloud.rs`.

[DOC]: ../decoupled-render.md "Decoupled headset render loop"
[TEST]: ../../crates/qaros-rerun/src/decoupled.rs "Retained draw data test"
[RR]: ../../patches/re_renderer "Vendored re_renderer 0.40.0"
