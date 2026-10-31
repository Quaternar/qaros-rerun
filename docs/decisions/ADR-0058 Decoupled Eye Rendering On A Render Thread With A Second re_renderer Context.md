---
id: ADR-0058
title: Decoupled eye rendering on a render thread with a second re_renderer context
status: proposed
date: 2026-10-19
repo: qaros-rerun
---

# ADR-0058: Decoupled eye rendering on a render thread with a second re_renderer context

## Context

The eyes are rendered inside the viewer frame (ADR-0017): the 3D view builds its draw data in Rerun's UI pass, the observer copies it per eye, and the eyes are drawn after the pass. Every eye frame therefore waits for a whole viewer frame, and a scene whose update takes tens of milliseconds (queries and visualizers over many entities, long time ranges) sets the headset rate. On the Mac a 45 ms scene update gives 11 eye frames per second on the CPU path [DOC].

`re_renderer` brackets its work in frames (`RenderContext::begin_frame`, `before_submit`), and every `ViewBuilder` takes its uniforms from the context's per-frame CPU write belt and its targets from per-frame pools. Rerun's context is busy for the whole UI pass, so a second thread cannot use it while the UI pass runs.

## Decision

With `--decoupled-render` (off by default), the eyes are drawn on a thread of their own:

- The render thread owns a second `RenderContext`, created with `RenderContext::new_sharing_device` on the viewer's `wgpu::Device` and `wgpu::Queue`, with Rerun's output format and render config. It brackets one frame of its own context per eye frame.
- The UI thread runs Rerun as before. The 3D view observer only copies the streamed view's draw data; after the UI pass the copy is published as a retained scene (ADR-0059).
- Every eye frame takes the newest scene, begins the next frame of every user that has none in flight (fresh poses), builds the two eye views per user from per-eye copies of the scene's draw data, draws, submits and shows. It never waits for the UI thread.
- The renderer behind it is the same `HeadsetRenderer`, driven through `render_frames_from_scene` and the three parts of `end_viewer_frame`; the default path runs unchanged.

## Alternatives considered

- One context shared under a lock: the UI pass holds the context from `begin_frame` to `before_submit`, so the render thread would wait for the whole scene update; nothing is decoupled.
- Eye-only frames between viewer frames on Rerun's context (K eye frames per scene): reuses one scene for several eye frames, but they still run on the UI thread, before or after the UI pass, never during it.
- An own minimal draw path for retained scenes (our own pipelines over Rerun's draw data internals): `DrawData` contents are private to `re_renderer`; it would mean reimplementing every renderer.
- A second device: Rerun's draw data (bind groups, buffers, textures) belongs to one device and cannot be drawn on another.

## Consequences

- Positive: the eye rate no longer depends on the scene update time. On the Mac, with a synthetic scene update of 50, 100 and 200 ms the eyes held 248 frames per second at a 4.0 ms median interval, against 8.1, 4.5 and 2.3 with the flag off; a 5000-entity scene (46 ms updates) went from 11.1 to 62.7 [DOC].
- Positive: content updates at the scene rate while the eyes follow the head at the eye rate.
- Negative: shaders and pipelines are compiled twice, and the render thread has its own pools and CPU write belt (its own host-visible chunks).
- Negative: the eye rate still shares the GPU with the UI, and per-eye CPU work (collecting and encoding the scene's draw calls) grows with the scene's size.
- Negative: two more patches to the vendored `re_renderer` (ADR-0059, ADR-0061).

## Evidence and sources

- [DOC] `docs/decoupled-render.md`, measurements and the design as built.
- [CODE] `crates/qaros-rerun/src/decoupled.rs`, `patches/re_renderer/src/context.rs`.

[DOC]: ../decoupled-render.md "Decoupled headset render loop"
[CODE]: ../../crates/qaros-rerun/src/decoupled.rs "Render thread"
