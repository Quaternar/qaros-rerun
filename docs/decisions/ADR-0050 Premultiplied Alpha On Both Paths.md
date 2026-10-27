---
id: ADR-0050
title: Color goes to QAROS with premultiplied alpha on both paths, declared on the stream
status: accepted
date: 2026-10-14
repo: qaros-rerun
---

# ADR-0050: Color goes to QAROS with premultiplied alpha on both paths, declared on the stream

## Context

ADR-0015 sets sRGB-encoded bytes for color. The two paths produce alpha differently:

- The Vulkan path draws each eye with `BlendWithBackground::No` ([ER]), so Rerun's compositor ignores the source alpha and writes alpha 1 for every pixel ([CMP] L32-39), sRGB-encoded ([CMP] L94-98). The output pass then multiplies the composited color by depth coverage, 0 or 1 ([OUT] `covered_color`): a covered pixel keeps its color with alpha 1, an uncovered one becomes `[0, 0, 0, 0]`.
- The CPU path reads back Rerun's screenshot, which is opaque, and sets alpha from depth coverage: a covered pixel keeps its color with alpha 255, an uncovered one becomes `[0, 0, 0, 0]` ([HS] `clear_uncovered_pixels`).

A render sender declares what its alpha means by chaining `QarRenderSenderAlphaModeExt`, and "the QAROS mixer draws a `QAR_COLOR_ALPHA_MODE_PREMULTIPLIED` stream with its alpha as coverage ... Without this extension the stream declares `QAR_COLOR_ALPHA_MODE_UNSPECIFIED` and the receiver keeps its own default" ([HDR] L845-868, L2262-2288). For the mixer that default is opaque: an undeclared stream "Draws the stream opaque, unless the operator turns on `mixerSourceAlpha`" ([RS] "Transparency"), and "a source that declares opaque, or nothing, covers fully" ([MIX] "Depth and alpha"). The `qaros` crate exposes the declaration as `RenderSenderConfig::alpha_mode`, `ColorAlphaMode::Unspecified` by default ([QR] L57-61, L137, L156). OpenXR, whose room space and depth conventions QAROS and ADR-0015 follow, also treats layer color as premultiplied unless a layer sets `XR_COMPOSITION_LAYER_UNPREMULTIPLIED_ALPHA_BIT` ([OXR]).

## Decision

Color goes to QAROS as sRGB-encoded bytes with premultiplied alpha on both paths, and both senders declare it: the CPU sink and the Vulkan stream set `alpha_mode: ColorAlphaMode::Premultiplied` in their `RenderSenderConfig` ([SINK] `stream_config`), so the mixer blends each pixel by its coverage.

On both paths alpha is coverage, 0 or 1: opaque covered pixels and all-zero uncovered pixels are valid premultiplied values as they stand. The output pass already multiplies color by its alpha, so the convention holds should fractional coverage ever reach it.

## Alternatives considered

- Declare nothing: the mixer draws the stream opaque, so the black of every uncovered pixel hides the room behind the app volume, unless the operator turns on `mixerSourceAlpha` for every undeclared stream.
- Straight alpha (`QAR_COLOR_ALPHA_MODE_STRAIGHT`): for alpha 0 or 1 it means the same pixels, and QAROS would premultiply each frame in an extra shader pass ([HDR] L864-868).
- Opaque (`QAR_COLOR_ALPHA_MODE_OPAQUE`): the mixer would draw the uncovered pixels as covered, black over the room.

## Consequences

- Positive: both paths satisfy one declared convention, which the mixer composites by without operator settings; tests pin the pixels (`translucent_content_goes_out_premultiplied` in `eye_output.rs`, `uncovered_pixels_become_transparent_and_premultiplied` in `headset.rs`).
- Negative: no translucency reaches the room: translucent meshes go out opaque, and silhouette edges are hard, since coverage comes from depth, which is not antialiased (ADR-0015).
- Negative: a receiver other than the mixer composites by its own reading of the declared mode.

## Evidence and sources

- [CMP] `patches/re_renderer/shader/composite.wgsl` L32-39 (`blend_with_background` 0 forces alpha 1), L85-98.
- [ER] `crates/qaros-rerun/src/eye_render.rs` (`blend_with_background: BlendWithBackground::No`).
- [OUT] `crates/qaros-rerun/src/eye_output.wgsl`, `covered_color`.
- [HS] `crates/qaros-rerun/src/headset.rs`, `clear_uncovered_pixels`.
- [SINK] `crates/qaros-sink/src/sink.rs` and `crates/qaros-sink/src/vulkan/qaros_stream.rs` (`stream_config`, `the_stream_declares_premultiplied_alpha`).
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L845-868 (`QarColorAlphaMode`), L2262-2288 (`QarRenderSenderAlphaModeExt`).
- [RS] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/rendering-streams.mdx`, "Transparency" (L196-206).
- [MIX] QAROS docs at `ebfe3bf`, `docs/website/docs/operator-guide/the-mixer.md`, "Depth and alpha" (L53-56).
- [QR] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/crates/qaros/src/render.rs` (`ColorAlphaMode`, `RenderSenderConfig::alpha_mode`).
- [OXR] Khronos, OpenXR 1.0 reference, `XrCompositionLayerFlagBits`.

[CMP]: ../../patches/re_renderer/shader/composite.wgsl "Rerun compositor"
[ER]: ../../crates/qaros-rerun/src/eye_render.rs "Eye rendering"
[OUT]: ../../crates/qaros-rerun/src/eye_output.wgsl "Eye output pass"
[HS]: ../../crates/qaros-rerun/src/headset.rs "Headset renderer"
[SINK]: ../../crates/qaros-sink/src/sink.rs "CPU sink"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[RS]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/rendering-streams.mdx "Rendering streams"
[MIX]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/operator-guide/the-mixer.md "The mixer"
[QR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/crates/qaros/src/render.rs "qaros render senders"
[OXR]: https://registry.khronos.org/OpenXR/specs/1.0/man/html/XrCompositionLayerFlagBits.html "XrCompositionLayerFlagBits"
