---
id: ADR-0057
title: The GPU eye path keeps its output uniforms and composite targets across frames
status: accepted
date: 2026-10-19
repo: qaros-rerun
---

# ADR-0057: The GPU eye path keeps its output uniforms and composite targets across frames

## Context

On a Windows laptop with an RTX-class GPU some runs stream smoothly and others stay choppy for their whole length. A Windows performance trace of one choppy second, with the allocation call stacks, showed our UI thread creating a 128 MiB host-visible allocation every 143 to 162 ms and freeing it about 130 ms later, all from `EyeOutput::encode` → `create_buffer_init` for the output pass's 32-byte uniform → wgpu-core's staging buffer → a new gpu-allocator block, on every eye frame. wgpu-core stages `mapped_at_creation` through a host-visible buffer; under `MemoryHints::Performance` the first host block is 64 MiB and the next 128 MiB, and gpu-allocator frees a block as soon as it is empty unless it is the last one. When re_renderer's upload belt fills the first host block, every eye frame gets a new block, and WDDM commits and tears down its 64 page tables each time; the frame that staged into it waits for the commit (117 to 125 ms per frame in the trace). The eye path also created a new composite target per eye and frame, device-local and suballocated: not the observed source, but the same churn whenever a device block sits at its edge.

## Decision

The GPU eye path keeps its per-frame resources:

- The output pass's uniform buffers by content, up to 8, oldest dropped first. A kept buffer is never written again, so frames in flight read what they were given; the uniform (near, far, encoding, sizes) repeats from frame to frame, so every frame after the first stages nothing.
- One composite target per user and eye, reused while its format and resolution match. Reuse is safe because a user's next frame begins only after its last one left the renderer, and wgpu orders the texture's use across submissions on the one queue.

A kept uniform buffer or composite target unused for 10 s (`UNUSED_EYE_RESOURCE_LIFETIME`) is released, checked every viewer frame. The lifetime is in time, not viewer frames, because an idle viewer repaints only about twice a second; once nobody streams, the kept resources are gone within about 10 s. A released user's targets go by the same rule: the renderer does not learn of releases, which happen inside the sink.

`QAROS_RERUN_EYE_REUSE=0` creates both per eye frame again, as a diagnostic opt-out to compare against; it is not a user setting. `QAROS_RERUN_MEMORY_BLOCK_MIB` (gpu-allocator's device block size, unset by default) stays as a diagnostic knob. The memory line (Vulkan heaps from `VK_EXT_memory_budget` and gpu-allocator's blocks) is logged once at start at INFO and every 5 s at DEBUG, since with reuse on the extra block is no longer expected and the periodic line is for diagnosis. While DEBUG is off for `qaros_rerun::memory_budget_log`, the periodic budget query and allocator report are not taken.

## Alternatives considered

- Larger gpu-allocator blocks (`QAROS_RERUN_MEMORY_BLOCK_MIB`): moves the edge where a block fills but still stages per frame, and raises resident memory for every run.
- Writing one uniform buffer with `queue.write_buffer`: also stages through wgpu-core's host-visible staging, so it can hit the same full block.
- Residency hints (`VK_EXT_memory_priority`, `VK_EXT_pageable_device_local_memory`): cannot stop a block from being created and destroyed; the trace shows allocation churn, not a budget overrun.
- Keep it behind `QAROS_RERUN_EYE_REUSE=1`: every run without the variable stays exposed to the choppy state.

## Consequences

- Positive: in an A/B of reuse against the baseline on a Windows laptop with an RTX-class GPU (structure from motion demo), runs went from 1 smooth and 4 choppy to 5 smooth and 0 choppy, and the extra empty 128 MiB host block (`128/0.00`), present in nearly every baseline memory line, never appeared.
- Positive: GPU engine utilisation sampled in the same A/B: in a choppy baseline run the kernel copy engine was at 71 % and the viewer's own 3D at 1.4 %; smooth reuse runs showed about 35 % and 7 to 8 %.
- Negative: up to 8 small uniform buffers and two composite targets per streaming user stay resident while frames flow; a user that leaves keeps its targets for up to 10 s.
- Negative: `--frame-stamp` still uploads its stamp per eye frame, so stamped runs can still stage through a full host block.
- Open: why a run lands in the full-block state (re_renderer's belt chunk count following a decaying upload peak) is likely, not verified.

## Evidence and sources

- [CODE] `crates/qaros-rerun/src/eye_output.rs` (`KeptUniformBuffers`, `KeptCompositeTargets`), `crates/qaros-rerun/src/memory_budget_log.rs`, `crates/qaros-rerun/src/memory_knobs.rs`.

[CODE]: ../../crates/qaros-rerun/src/eye_output.rs "Eye output"
