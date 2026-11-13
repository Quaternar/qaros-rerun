---
id: ADR-0029
title: Vulkan frame hand-off through raw acquire and release batches chained to wgpu
status: accepted
date: 2026-10-08
repo: qaros-rerun
---

# ADR-0029: Vulkan frame hand-off through raw acquire and release batches chained to wgpu

## Context

The cube example transitions each sender image from its reported layout and queue family, renders, transitions to `TRANSFER_SRC_OPTIMAL`, releases ownership to `VK_QUEUE_FAMILY_EXTERNAL`, signals the frame's semaphore, writes the new state back, then shows. wgpu cannot record a chosen final layout or a queue-family release, assumes wrapped images start `UNDEFINED`, has no pNext on submits, and attaches staged semaphores to whichever submit comes next.

## Decision

`HandoverQueue::submit_handover` submits three batches on wgpu's queue, chained by binary semaphores of our own: a raw acquire batch (reported state to attachment layout on wgpu's family), wgpu's render submission, a raw release batch (to `TRANSFER_SRC_OPTIMAL`, released to the external family, the frame's semaphore signalled). Images are wrapped again every frame with an empty drop callback. The write-back records `TRANSFER_SRC_OPTIMAL` on the external family with a null fence, then `show_frame` is called right after submission, without waiting for a fence. A frame that is begun but not shown gets its semaphore signalled by an empty batch. All raw submits run on the UI thread; a user's sender is dropped only after the queue is idle.

## Alternatives considered

- Stage semaphores on wgpu's own submit: they attach to the next submit, possibly egui's or Rerun's.
- Wait for the GPU before `show_frame`, as the C cube does: costs a loop turn per frame and is not required. The Rust Vulkan sample "waits on a slot's fence only when it reuses the slot, never for the frame just submitted: the frame's semaphore hands that work to QAROS, which waits for it on the GPU" ([VKT] L790), and the header asks only that layouts, queue ownership and synchronization be updated on the frame before `qar_render_sender_show_frame` ([HDR] L3975-3983).
- Wrap once and track layouts: wgpu would assume `UNDEFINED` and lose the reported state.

## Consequences

- Positive: same barriers and order as the cube; pixel-exact on the Windows QAROS package with the Khronos validation layer silent.
- Negative: "QAROS may submit on its own queues" ([VKA] L54); whether those can include wgpu's queue, which our raw batches share, is not documented. A frame whose record cannot be read is dropped and its user's sender released (ADR-0055).

## Evidence and sources

- [VP] `docs/vulkan-path.md`, "One frame".
- [HO] `crates/vulkan-device/src/handover.rs`, `crates/vulkan-device/tests/handover.rs`; `crates/qaros-sink/src/vulkan/sink.rs`.
- [CUBE] QAROS Vulkan cube example at `b1338df`.
- [VKT] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/tutorials/rust/vulkan-rendering.mdx` L790.
- [VKA] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/docs/vulkan-api.md` L54.
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L3975-3983 (`qar_render_sender_frame_vulkan`).

[VP]: ../vulkan-path.md "The QAROS Vulkan path"
[HO]: ../../crates/vulkan-device/src/handover.rs "Hand-over queue"
[CUBE]: https://github.com/Quaternar/qaros/tree/b1338df/qar-streaming-c/examples/vulkan_cube "QAROS Vulkan cube example"
[VKT]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/tutorials/rust/vulkan-rendering.mdx "Rust Vulkan rendering tutorial"
[VKA]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/docs/vulkan-api.md "qaros Vulkan API"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
