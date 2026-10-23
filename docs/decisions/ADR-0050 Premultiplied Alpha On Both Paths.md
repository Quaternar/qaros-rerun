---
id: ADR-0050
title: Color goes to QAROS with premultiplied alpha on both paths
status: accepted
date: 2026-10-14
repo: qaros-rerun
---

# ADR-0050: Color goes to QAROS with premultiplied alpha on both paths

## Context

ADR-0015 sets sRGB-encoded bytes for color. The two paths produce alpha differently:

- The Vulkan path hands QAROS Rerun's composited eye as it is. Rerun's compositor writes `srgb(unmultiplied) * alpha` ([CMP] L94-98), premultiplied alpha over sRGB-encoded color, and the output pass only zeroes uncovered pixels ([OUT] `covered_color`).
- The CPU path reads back Rerun's screenshot, which is opaque, and sets alpha from depth coverage: a covered pixel keeps its color with alpha 255, an uncovered one becomes `[0, 0, 0, 0]` ([HS] `clear_uncovered_pixels`).

The public QAROS header and docs at `b1338df` do not say which convention the mixer expects. The header's pixel formats carry no alpha convention or transfer function ([HDR] L394-400, L734-742); the mixer page says only that "per-source alpha drives transparency during compositing, and empty regions stay fully transparent" ([MIX], "Depth and alpha"); every sample writes opaque color ([CPU] L127-134, [CUBE] `cube.frag`, `renderer.cpp` L721-723). OpenXR, whose room space and depth conventions QAROS and ADR-0015 follow, treats layer color as premultiplied unless a layer sets `XR_COMPOSITION_LAYER_UNPREMULTIPLIED_ALPHA_BIT`, which "indicates the texture color channels have not been premultiplied by the texture alpha channel" ([OXR]).

## Decision

Color goes to QAROS as sRGB-encoded bytes with premultiplied alpha, OpenXR's default, on both paths:

- Vulkan path: Rerun's premultiplied composite passes through unchanged; translucent content and anti-aliased edges keep fractional alpha with `rgb <= alpha`.
- CPU path: alpha is coverage, 0 or 1, so opaque covered pixels and all-zero uncovered pixels are premultiplied as they stand.

The documentation leaves the convention open; if the mixer expects straight alpha, the Vulkan output pass unmultiplies (`rgb / alpha` where alpha is above 0) in one place.

## Alternatives considered

- Straight alpha: the output pass would unmultiply every pixel, against OpenXR's default, and a mixer expecting premultiplied input would add a bright fringe at every translucent edge; with premultiplied output and a mixer expecting straight alpha, the error is a darker fringe on those edges only.
- Fractional alpha on the CPU path: Rerun's screenshot carries no alpha, as its compositing writes opaque alpha; reading the composited eye back instead would move the CPU path onto the viewer's own device, a larger change than the CPU path's role (fallback, Mac) justifies.

## Consequences

- Positive: both paths satisfy one stated convention; tests pin it (`translucent_content_goes_out_premultiplied` in `eye_output.rs`, `uncovered_pixels_become_transparent_and_premultiplied` in `headset.rs`).
- Negative: unconfirmed against the mixer; translucent meshes are opaque on the CPU path and translucent on the Vulkan path.

## Evidence and sources

- [CMP] `patches/re_renderer/shader/composite.wgsl` L85-98.
- [OUT] `crates/qaros-rerun/src/eye_output.wgsl`, `covered_color`.
- [HS] `crates/qaros-rerun/src/headset.rs`, `clear_uncovered_pixels`.
- [HDR] QAROS header at `b1338df`, `qar-streaming-c/include/qar_streaming.h` L394-400, L734-742.
- [MIX] QAROS docs at `b1338df`, `docs/website/docs/operator-guide/the-mixer.md`, "Depth and alpha" (L53-56).
- [CPU] QAROS sample at `b1338df`, `qar-streaming-c/examples/cpu_rendering_visualizer.c` L127-134.
- [CUBE] QAROS sample at `b1338df`, `qar-streaming-c/examples/vulkan_cube/`.
- [OXR] Khronos, OpenXR 1.0 reference, `XrCompositionLayerFlagBits`.

[CMP]: ../../patches/re_renderer/shader/composite.wgsl "Rerun compositor"
[OUT]: ../../crates/qaros-rerun/src/eye_output.wgsl "Eye output pass"
[HS]: ../../crates/qaros-rerun/src/headset.rs "Headset renderer"
[HDR]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[MIX]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/operator-guide/the-mixer.md "The mixer"
[CPU]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/examples/cpu_rendering_visualizer.c "C CPU sample"
[CUBE]: https://github.com/Quaternar/qaros/tree/b1338df/qar-streaming-c/examples/vulkan_cube "Vulkan cube sample"
[OXR]: https://registry.khronos.org/OpenXR/specs/1.0/man/html/XrCompositionLayerFlagBits.html "XrCompositionLayerFlagBits"
