---
id: ADR-0026
title: QAROS's native Vulkan render sender as the GPU frame path
status: accepted
date: 2026-10-06
repo: qaros-rerun
---

# ADR-0026: QAROS's native Vulkan render sender as the GPU frame path

## Context

At headset resolution the CPU path is too slow for two users (ADR-0013). QAROS publishes a Vulkan frame path (commit `9492c51`, the Vulkan cube, 2026-10-05) with a cube example as its main documentation.

## Decision

The GPU path is QAROS's own Vulkan render sender. The `qaros` crate (feature `vulkan`, `ash` types) exposes adapter choice, `Session::create_vulkan_render_sender` with `QarStreamParamsVulkan`, and safe access to the Vulkan frame (views, image records, semaphore, write-back of layout, queue family and fence). The viewer's `--qaros-graphics vulkan` renders each eye straight into the sender's images on its own device (ADR-0025), separated textures, one sender per user. Depth stays on the GPU (ADR-0031). The CPU path stays available.

## Alternatives considered

- A D3D11 bridge from Vulkan into the D3D11 render sender: an extra copy, a second API and a keyed-mutex protocol the header does not document.
- Wait for a QAROS release with the Vulkan path: rejected; building on the published source accepts coupling to an API that may still change.

## Consequences

- Positive: on a Windows laptop the Vulkan sender path on its own, through the `qaros` crate without the viewer's rendering, reaches 218 fps (960x1000) and 177 fps (1832x1920) GPU to GPU, pixel-exact, validation layer silent; the viewer shows a frame about 3 ms after beginning it, so its rate is the viewer's own frame time.
- Negative: the contract is documented mostly by the cube; image format, usage, initial state and the fence rule are inferred from it (the first frame reports `TRANSFER_SRC_OPTIMAL` on the external family, not `UNDEFINED`).
- Negative: two targets at 1832x1920 reach 18.7 fps each headless; per-user render cost is the limit (ADR-0020).

## Evidence and sources

- [VP] `docs/vulkan-path.md`; `crates/qaros-sink/src/vulkan/`.
- [EV] `docs/evidence/vulkan-path/` (GPU path frames, `runs.log`).
- [QC] The `qaros` crate in `Quaternar/qaros` at the revision pinned in `Cargo.toml`.
- [CUBE] QAROS Vulkan cube example at `b1338df`.

[VP]: ../vulkan-path.md "The QAROS Vulkan path"
[EV]: ../evidence/vulkan-path/runs.log "Vulkan path runs"
[QC]: https://github.com/Quaternar/qaros "QAROS"
[CUBE]: https://github.com/Quaternar/qaros/tree/b1338df/qar-streaming-c/examples/vulkan_cube "QAROS Vulkan cube example"
