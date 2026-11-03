---
id: ADR-0087
title: Each view panel streams one fixed frame per run, and a resized view is scaled into it
status: proposed
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0087: Each view panel streams one fixed frame per run, and a resized view is scaled into it

## Context

A view's stream panel shows a CPU video sender whose texture has one size for its life (ADR-0076). The crop of a view follows the desktop layout, so resizing the desktop window changes every view's pixel size at every capture (250 ms). The sink answered a size that held for two publishes with a new sender, a `set_stream`, a `set_size`, and kept the old sender until the new one's first ready begin, at most 10 s.

Seen in the glasses on QAROS 0.9.0-dev.3184 (viewer log of 2026-11-02, from 12:55:32 UTC on): one window drag gave the camera view 441x238, 572x301, 666x317, 783x450 and more within seconds, and each size that held made a new stream (`camera now streams ... at 704x320 px instead of ... (the image's size changed)`). No successor ever had a ready begin: each old stream was destroyed after the 10 s retire limit (`its successor had no ready begin within the retire limit`), and no successor showed a frame for the rest of the run (no "first frame" line after 12:52:58). A replaced stream is also the case in which the 3177 mixer lost its Vulkan device ([KI] "A stream panel's mixer loses its Vulkan device on a replaced stream", Quaternar#343).

## Decision

- **One frame per view and run.** Panel capture fixes each view's stream frame from the first crop size that holds for `STREAM_FRAME_SETTLE` (1 s): the settled size, halved until neither side exceeds 1280 px, is the content box, and the content box padded to the mixer multiples (width 64, height 8) is the frame. A view is not published before its frame is fixed, so the sink claims the panel, sizes its sender and its metres from the frame, and every later image has that size. The frame is kept for the rest of the run, also while the view is off screen, because its panel keeps its sender (ADR-0082).
- **Scaled into the frame on the CPU.** Every capture is scaled, aspect preserved, to the largest size that fits the content box, and centred in the frame. A crop of the settled size is copied unscaled. A larger crop is first halved with a 2x2 box filter while it is at least twice the target, then sampled bilinearly; a smaller one is sampled up bilinearly.
- **Letterbox, never a new stream.** A view whose aspect changes, however much, is letterboxed within the run. The bars and the mixer padding take the crop's bottom-right pixel, the view's background in a Rerun view. They cannot be transparent: `PanelImages` stores every image with alpha 255.
- **The panel's size stays.** The panel keeps the metres it was claimed with: 0.45 m wide, its height from the frame's aspect (ADR-0076). The next run fixes a new frame from the layout it finds, and its claim sets the panel's size from it.
- **The timeline keeps its own path.** The timeline panel is drawn at a fixed 960x256 and padded for the mixer as before (ADR-0078).
- **The sink's replacement stays as a safety net.** It replaces a sender whose image keeps a new size, or that failed; with fixed frames, a view's size never changes within a run.

| Candidate | Fits | Why |
| --- | --- | --- |
| Fixed frame from the first settled size, letterboxed | yes, chosen | No stream is replaced in a run; the settled layout streams at its native pixels with no scaling cost. |
| Recreate the stream once a new size has held for a few seconds | no | Every replacement on 3184 blanked the panel for good in the log above; a replaced stream is also the #343 case. A longer hold makes swaps rarer, not safe. |
| One fixed frame per panel kind (for example 1024 px wide at the panel's pixels per metre) | no | Upscales every small view on every capture and streams more pixels than the view has; the panel's aspect still has to come from the view. |
| Scale in the sink on the QAROS thread | no | That thread renders the eyes (the render thread with `--decoupled-render`); the scaling belongs on the UI thread, which already crops and swizzles. |

## Consequences

- Positive: resizing the desktop window, moving it to a display of another scale, or changing the layout's split never blanks a panel; the panel shows the view scaled, at most 250 ms behind the desktop.
- Positive: no stream is replaced in a run on a resize, so the replaced-stream path of #343 is not taken.
- Negative: a view that settles small (a small window at start) streams at that low resolution for the whole run, upscaled when the window grows; the next run picks the new size.
- Negative: a view whose aspect changes a lot shows bars until the next run.
- Negative: a view gets its panel 1 s later than before (the frame settle, then the 3 s panel settle).
- Cost on the UI thread, per view and capture, measured on an Apple M5 Pro with the functions built at `opt-level=3`: a crop of the settled size 0.05 ms (a copy); 1016x665 into a 572x301 box 1.3 ms; 899x470 into 572x301 0.9 ms; 441x238 up into 572x301 0.5 ms; 899x470 up into 1016x665 1.9 ms; 1900x1000 into the 1280x720 cap 5 ms. At 4 captures a second, two resized views cost under 2 % of a core; four views at the cap about 8 %. The dev profile (`opt-level=1`) is up to twice as slow.

## Evidence and sources

- [PC] `crates/qaros-rerun/src/panel_capture.rs` (`StreamFrame`, `StreamFrames`, `letterbox_into_frame`; tests `a_desktop_resize_keeps_the_published_size_and_scales_the_view_into_it`, `a_frame_is_fixed_once_the_crop_size_holds_for_the_settle_time_and_kept_after`, `a_crop_of_another_aspect_is_letterboxed_at_the_largest_size_that_fits`, `a_crop_of_the_settled_size_is_copied_unscaled_into_the_frame_centre`, `a_larger_crop_is_box_halved_and_a_smaller_one_sampled_up_bilinearly`).
- [SP] `crates/qaros-sink/src/stream_panels.rs` (test `a_view_scaled_into_its_fixed_frame_keeps_its_one_sender_through_a_resize`).
- Viewer log `viewer-20261102T125057Z-32564.log` of the `structure_from_motion` demo on QAROS 0.9.0-dev.3184, lines 723-1352.
- [KI] [Known issues](../known-issues.md).

[PC]: ../../crates/qaros-rerun/src/panel_capture.rs "Panel capture"
[SP]: ../../crates/qaros-sink/src/stream_panels.rs "Stream panels"
[KI]: ../known-issues.md "Known issues"
