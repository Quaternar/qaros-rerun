---
id: ADR-0013
title: CPU readback frame path first, GPU path as follow-on work
status: superseded
superseded_by: ADR-0026
date: 2026-10-05
repo: qaros-rerun
---

# ADR-0013: CPU readback frame path first, GPU path as follow-on work

## Context

On Windows Rerun renders through Vulkan (wgpu with DX12 disabled). The public QAROS header at `c5b72b0` accepts frames from the CPU or D3D11 only.

## Decision

Send frames over the CPU render sender: per QAROS frame, render both eyes through `re_renderer`, read color back through Rerun's screenshot path and depth through a full-eye picking layer (Rerun's main depth buffer is multisampled, private and discarded), make uncovered pixels transparent from depth coverage, write them into the frame's textures (four declared views: both eyes' color side by side in texture 0, depth in texture 1, BGRA8 and D32) and show with the near and far planes used.

## Alternatives considered

- D3D11 interop from the start: cross-API work, an extra copy and a keyed-mutex protocol the header does not document.
- Wait for a Vulkan frame path: the public header at `c5b72b0` has none.

## Consequences

- Positive: no cross-API interop; works on a Mac and is bit-exact on the Windows QAROS package.
- Negative: two eyes of color and depth read back per frame, plus 16 bytes per pixel of unused picking ids. Two users reached 15 to 20 fps each at 1832x1920 on a Mac and 5.9 each on a Windows laptop against the QAROS package, too slow for headset resolution.
- Superseded as the performance path by ADR-0026. The CPU path stays in the code as `--qaros-graphics cpu`, the default off Windows; on Windows the default is `vulkan` (`QarosGraphics::PLATFORM_DEFAULT`).

## Evidence and sources

- [SINK] `crates/qaros-sink/src/sink.rs` (CPU render sender per user), `crates/qaros-sink/src/pixels.rs`.
- [OPT] `crates/qaros-rerun/src/options.rs` (`--qaros-graphics`, `QarosGraphics::PLATFORM_DEFAULT`).
- [README] `README.md`, "The hook" (per-frame steps for a CPU sink).

[SINK]: ../../crates/qaros-sink/src/sink.rs "CPU QAROS sink"
[OPT]: ../../crates/qaros-rerun/src/options.rs "Viewer options"
[README]: ../../README.md "qaros-rerun README"
