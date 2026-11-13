---
id: ADR-0061
title: A process-wide queue submission lock
status: proposed
date: 2026-10-19
repo: qaros-rerun
---

# ADR-0061: A process-wide queue submission lock

## Context

wgpu's own submissions are thread-safe. The QAROS Vulkan path (ADR-0029) is not only wgpu: it submits raw acquire and release batches to wgpu's `VkQueue`, stages wait and signal semaphores on wgpu's queue for the next submit, signals discarded frames with an empty raw batch and waits for the queue to be idle before a sender goes. All of that assumed one submitting thread. With a render thread (ADR-0058) the UI thread keeps submitting (Rerun's `before_submit`, the headless panel capture), so a raw submit could race a wgpu submit on the same `VkQueue` (Vulkan requires external synchronization), and a staged semaphore could ride on the UI thread's submit.

## Decision

The vendored `re_renderer` exports `queue_submission_lock()`, a process-wide mutex, and holds it in `RenderContext::before_submit` around its submit. The viewer holds it around the eye submit (`draw_and_submit`), the headless panel capture's two queue submits (not its tessellation or its waits for the GPU), and on the render thread around the sink calls (begin, show, abandon, user changes, drop) of a GPU sink that submits outside wgpu (`GpuFrameSink::submits_outside_wgpu`, today the QAROS Vulkan sink), which covers every raw submit and every staged semaphore. A sink that submits through wgpu alone takes no lock: wgpu synchronizes its submits. Preparing and drawing the eye views (the view builders and queuing the scene's draw data) run outside it, apart from the eye submit itself, so the scene's encoding cost does not hold the UI thread's submits. Nobody holds it across a call that takes it again. egui's window presentation in eframe does not take it, so with the QAROS Vulkan sink `--decoupled-render` is refused unless the viewer is headless (the launched-by-QAROS default).

## Alternatives considered

- A second queue for the render thread: wgpu 30 exposes one queue per device.
- Refusing the Vulkan sink in decoupled mode: the Vulkan path is the default on Windows and the one QAROS streams fastest with.
- Patching eframe's presentation as well: a third vendored crate for a mode QAROS does not launch.

## Consequences

- Positive: every submit in the process that we know of is serialized; the Vulkan path keeps its single-submitter assumption.
- Negative: with the Vulkan sink, the UI thread's `before_submit` can wait for a sink call on the render thread. The eyes wait for the UI thread at most for one of its submits.
- Negative: the lock does not cover QAROS's own threads. "QAROS may submit on its own queues" ([VKA] L54); whether those can include wgpu's queue is not documented.
- Negative: with the Vulkan sink, a write of the unanswered-targets file (ADR-0040) during a sink call happens under the lock; rare and small.

## Evidence and sources

- [CODE] `patches/re_renderer/src/context.rs` (`queue_submission_lock`), `crates/qaros-rerun/src/decoupled.rs` (`sink_submission_lock`), `crates/qaros-rerun/src/eye_render.rs`, `crates/qaros-rerun/src/panel_render.rs` (`PanelRenderer::submit`), `crates/frame-sink/src/gpu.rs` (`submits_outside_wgpu`).
- [VK] `crates/vulkan-device/src/handover.rs` (`submit_handover`, `submit_raw`), `crates/vulkan-device/src/semaphores.rs` (`wait_queue_idle`, `submit_sync_batch`).
- [VKA] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/docs/vulkan-api.md` L54.

[CODE]: ../../patches/re_renderer/src/context.rs "Queue submission lock"
[VK]: ../../crates/vulkan-device/src/handover.rs "Raw Vulkan hand-off"
[VKA]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/docs/vulkan-api.md "qaros Vulkan API"
