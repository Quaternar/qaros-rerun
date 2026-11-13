---
id: ADR-0078
title: The viewer draws the timeline panel itself and takes its input as panel gestures
status: accepted
date: 2026-10-29
repo: qaros-rerun
---

# ADR-0078: The viewer draws the timeline panel itself and takes its input as panel gestures

## Context

ADR-0072 sets the timeline panel's behaviour: seeks, Play, a thumb that follows the time cursor and holds after release. Panels are video streams of the viewer (ADR-0076), so the viewer draws this panel too and has no page or server to take input through. Gesture events reach the application for stream panels as for any other: kind (hover, tap, long press, drag), phase, position and start position in the sender's pixels, and a lifecycle key of source peer and gesture id ([HDR] L4301-4482, L1180-1197). A gesture id is unique per peer only, taps carry id 0 ([HDR] L4338-4350, L4419-4426), every drag update carries its start position ([HDR] L4357-4358), and a live lifecycle is republished every 250 ms ([HDR] L4351-4356).

Rerun's own time control cannot be reused: `re_time_panel` 0.40.0 keeps it private and it needs a full `ViewerContext`. `re_viewer::App` keeps its time controls private too, but a view context system sees `ctx.time_ctrl` on every view it runs for ([RV]).

## Decision

- **Drawing.** A private `egui::Context` with Rerun's style (`re_ui::apply_style_and_install_loaders`) draws one row: a Play/Pause button with Rerun's icons, a track with a thumb, and the cursor as text. The raster is 960 x 256 px at 2 pixels per point. It renders on the viewer's own wgpu device through a renderer of its own (`OffscreenEguiRenderer`), never through eframe's, so its textures cannot replace the desktop UI's font atlas. It is drawn in `HeadsetViewerApp::logic`, which eframe also runs while the window is minimised or hidden, and published into `PanelImages` under the timeline key; the panel hook claims the panel once that image exists.
- **When it redraws.** When the thumb moved by a track pixel, the play state changed, or a drag is active, at most 15 times a second. An unchanged image is not drawn again; the sink's once-a-second re-show serves a mixer that connects late. The draw and its readback are submitted together and collected on a later `logic` pass, so the UI thread, which also renders the eye frames, never waits for the GPU; one render is in flight at a time.
- **Input.** Gestures are hit-tested against the rectangles the last UI pass recorded, not replayed as pointer events. A tap on the track seeks and pauses; a tap on the button sends Pause while playing and Play otherwise; a drag that starts on the track takes the pointer for its (peer, gesture id), seeks at its start, follows its updates and commits its end. Hovers and long presses are ignored. While one lifecycle owns the pointer, other peers' drags and taps are ignored, and an owner silent for 750 ms is released. Only the timeline panel subscribes to gestures.
- **Requests.** The panel feeds `TimelineRequests`, which the viewer takes once per frame, so seeks reach Rerun as `SetPlayState(Paused)` then `SetTimeClamped` in the same `logic` pass, before Rerun's own; Pause maps to `SetPlayState(Paused)`.
- **Feedback.** The thumb follows the finger while owned, holds for 1 s after release, then follows the published cursor, as ADR-0072. The button reads the play state of the recording's time control, which a context system (`TimeControlProbe`, registered on the 3D and 2D view classes) notes once per viewer frame; following counts as playing.
- **Amends.** This supersedes ADR-0072's page and keeps its seek, Play, thumb and hold rules, its 0.45 m panel width and its pose in front of the head. Its height follows the content area, as the view panels' (ADR-0076).

## Alternatives considered

- **Replaying gestures as egui pointer events.** It depends on egui's interpretation of a synthetic pointer; hit-testing the recorded rectangles is deterministic and unit-testable.
- **Patching `re_time_panel` to expose its time control.** A third patched Rerun crate for one row of controls.
- **A web page for the timeline.** It would need a panel server and a website panel next to stream panels, which ADR-0076 exists to avoid.

## Consequences

- Positive: a tap or a drag reaches the time cursor as one typed event, with no HTTP and no polling, and two users never fight over one cursor.
- Positive: pure tests cover the gesture mapping and ownership; a viewer test pushes a tap into the panel and finds the pause and the seek on Rerun's command channel after one `logic` pass.
- Negative: the control is a copy of Rerun's look, not Rerun's widget, and follows Rerun's style only as far as `re_ui` does.
- Negative: as with the page, the mixer decides between tap and drag; a drag that does not move within about 150 ms becomes a tap.

## Evidence and sources

- [TP] `crates/qaros-rerun/src/timeline_panel.rs`, `panel_render.rs` (`OffscreenEguiRenderer`), `timeline_requests.rs`, `timeline_scrub.rs` (`TimeControlProbe`), `viewer_app.rs` (`logic`).
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L1180-1197, L4301-4482.
- [RV] `re_viewer` 0.40.0 `src/app/mod.rs` L975 (`App::extend_view_class`); `re_ui` 0.40.0 `src/lib.rs` L244.

[TP]: ../../crates/qaros-rerun/src/timeline_panel.rs "Timeline panel"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
