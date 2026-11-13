---
id: ADR-0084
title: Rerun's reverse-Z depth goes to QAROS unchanged, declared with an infinite far plane
status: accepted
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0084: Rerun's reverse-Z depth goes to QAROS unchanged, declared with an infinite far plane

> Amended by ADR-0092: the app scale the declaration is divided by is the user's zoom, starting at 1, not the data scale; at zoom 0.1 to 10 the marker is 1e31 to 1e29.

## Context

ADR-0015 chose forward normalized depth with a finite far plane because QAROS's documentation did not say what a depth pixel holds or whether an infinite far plane is allowed. The header still says only that near > far declares reverse-Z ([HDR] L715-717). QAROS's source says more, and what it says makes our conversion redundant:

- **QAROS converts our depth anyway.** Only unscaled standard-Z depth over a finite far plane passes through; everything else is linearized with the declared planes, multiplied by the depth scale and re-encoded into a fixed room range of 0.1 to 20 m ([PSH] L364-369, L418-432, L486-492; [DNF] L83-92). The viewer streams in app units with a depth scale folded in by the sender, so its forward depth was converted on every frame (`depthConversion=true`, `effectiveDepthScale=0.001` in the Legion log of 2026-11-02). The viewer's own conversion was the first of two, and its clamp at `--far` dropped everything past 20 m in the room to background before QAROS saw it.
- **QAROS reads Rerun's depth directly.** Under reverse-Z a declared `near_plane` at most 0 or above 1e20 is an infinite far plane ([PSH] L397-404, `IsInfiniteFar`), and such depth is linearized as `far_plane / depth` ([DNF] L15-20), Rerun's own `near / distance` read back ([DEP]). A reverse-Z 0 stays "no depth" ([DNF] L75-82). The sender names this encoding in its log ([RSS] L312-333).
- **Nothing rejects the declaration.** `qar_render_sender_show_frame` takes every sender kind, CPU included, through `show_frame_common`, which hands `rendered_near_far` to `SetNearFar` without a check ([CRS] L2096-2122, L2483-2502); `SetNearFar` stores the planes in the frame's metadata as they are ([RSS] L164-176). The Rust binding and the stub copy the planes ([QR] `OwnedFrame::show`).
- **The CPU render sender takes the same route.** Its ingress is `MemToVulkanStagingConversion` with the depth plausibility check on ([VSS] L84-113); unless the layouts are identical it runs `UniversalVulkanImageCopyV2` with depth conversion enabled ([MVS] L217-220, L295-320), whose depth route needs an `R32Float` destination ([UV2] L883-894, L1197-1202). With identical layouts it uploads directly, and a depth scale over an infinite far plane is dropped with a warning ([UV2] L643-654, L956-966); which of the two the CPU sender's distribution layout selects was not traced.
- **The plausibility check accepts it.** Declared reverse-Z is judged consistent unless the values are nearly all 1 or sit in the upper half with a background of 1 ([DPL] L91-105); Rerun's background is 0 and its geometry `near / distance`.

## Decision

- **Default `--depth reverse-infinite`.** The output pass (`eye_output.wgsl`) and the CPU path (`DepthRange::convert_buffer_from_reverse_z`) write Rerun's reverse-Z value as it is. The frame stamp's depth (`near / 1 m`) and the smaller depth target's nearest depth per footprint (largest reverse-Z) are already in that encoding. Coverage still reads reverse-Z 0 as nothing drawn.
- **Declared as reverse-Z with an infinite far plane:** `near_plane` 1e30 room meters (`INFINITE_FAR_PLANE_MARKER`), `far_plane` the real near plane, `depth_scale` 1.0. The sink converts both to app units as before. 1e30 rather than `f32::MAX`: the sink multiplies by `1 / app_scale`, which takes `f32::MAX` to infinity at any app scale below 1; 1e30 stays finite down to an app scale of about 3e-9 and above QAROS's 1e20 threshold up to an app scale of 1e10, so no shader or log downstream meets an infinity.
- **Fallback for this round:** `--depth forward`, `reverse` and `linear` stay, unchanged. They go once a headset run confirms the default and QuaternAR documents the infinite far plane in `qar_streaming.h`, so the default rests on the contract rather than on the implementation.
- ADR-0015's depth encoding is superseded; its color conventions stand.

## Alternatives considered

- **Keep forward depth (ADR-0015).** Two conversions, two clamps (ours at `--near`..`--far`, QAROS's at 0.1 to 20 m) and a dependency on `--far` that the header never asks for.
- **Remove the finite-far encodings now.** Leaves no fallback if a QAROS build reads the declaration differently before the header documents it.
- **`near_plane` of 0 or below.** `IsInfiniteFar` accepts it too, but a near plane at most 0 cannot declare reverse-Z (near > far), and a negative near plane declares metric depth ([PSH] L371-388).

## Consequences

- Positive: one conversion of the depth, inside QAROS; nothing drawn is clamped by the viewer, so content past 20 m in the room stays a surface until QAROS's own range clamps it. `--far` no longer affects the default encoding (it still bounds the stamp's 1 m).
- Positive: a few lines of shader math out of the default path; the output pass otherwise stays (color compositing with premultiplied alpha and coverage, frame stamp, copy into QAROS's images, the smaller depth target).
- Negative: the default relies on qar-streaming behaviour the public header does not state. QuaternAR should document in `qar_streaming.h` that a reverse-Z `near_plane` above 1e20 declares an infinite far plane.
- To verify in the glasses: the sender's log line at frame 10, `RenderingStreamSender: stream ... produces depth [view ... reverse-Z infinite-far near=... far=...]`, with `near` above 1e20 and `far` the near plane in app units; no `contradicts its declared near/far` or `holds no rendered depth` warning for a scene with content; occlusion against the room and other volumes, and steady reprojection, both up close and for content far away. On the CPU path also no `over an infinite far plane ... the scale is dropped` warning.

## Evidence and sources

- [HDR] QAROS header at `e0c0195`, `qar-streaming-c/include/qar_streaming.h` L715-717 (`QarNearFar`), L2680-2684 (`QarRenderFrameShow`).
- [PSH] qar-streaming at `e487a31`, `libs/qar-streaming/include/qar_streaming/vulkan/PixelShaderHelper.hpp` (`DefaultNearFar`, `MetricDepthNearFar`, `IsInfiniteFar`, `NormalizedNearFar`, `ShouldPreserveMetadata`).
- [DNF] Same, `libs/qar-streaming/shaders/lib/DepthNearFarConvertShared.slang` (`LinearizeDepth`, `ConvertDepthNearFar`).
- [RSS] Same, `libs/qar-streaming/src/RenderingStreamSenderImpl.hpp` (`SetNearFar`, `DepthEncodingName`, `DescribeDepthViews`; the report at frame 10, L1305-1316).
- [CRS] Same, `libs/qar-streaming-c/src/v0/rendering_stream_sender.cpp` (`show_frame_common`, `qar_impl_render_sender_show_frame`).
- [VSS] Same, `libs/qar-streaming/src/backends/VideoStreamSenderImpl.hpp` (`CreateVulkanIngressConversion` for CPU frames).
- [MVS] Same, `libs/qar-streaming/src/backends/conversions/MemToVulkanStagingConversion.hpp`.
- [UV2] Same, `libs/qar-streaming/src/backends/conversions/UniversalVulkanImageCopyV2.hpp`.
- [DPL] Same, `libs/qar-streaming/src/backends/conversions/common/DepthPlausibility.hpp`.
- [QR] The `qaros` crate at `e0c0195`, `qar-streaming-rust/crates/qaros/src/render.rs` L950-962.
- [DEP] `crates/xr-math/src/depth.rs`; the sink's unit conversion in `crates/qaros-sink/src/sink.rs` (`PoseMapping::show_params`).

[HDR]: https://github.com/Quaternar/qaros/blob/e0c0195/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[QR]: https://github.com/Quaternar/qaros/blob/e0c0195/qar-streaming-rust/crates/qaros/src/render.rs "qaros render sender"
[DEP]: ../../crates/xr-math/src/depth.rs "Depth encodings"
