---
id: ADR-0099
title: A minimised viewer window lays out at its last shown size
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0099: A minimised viewer window lays out at its last shown size

> Amends ADR-0097 (a minimised window runs `ui`, and now also keeps its layout, its scene and its panels).

## Context

In run 4 of the 1103 test on METALENSE 2 (2026-11-03, the `structure_from_motion` run from 19:49Z), the viewer window was minimised from about 19:50:33Z to 19:50:53Z. The timeline panel kept advancing in the glasses, but the 3D content and both 2D view panels disappeared until the window was restored. Covering the window with a maximised one keeps the 3D content and the panels live; only a minimise breaks them.

- **`ui` ran while minimised.** ADR-0097's `RequestPaintWhileHidden` kept the passes full: the panel capture, which runs in `ui`, kept requesting screenshots, and the first one after the minimise withdrew both views (`panels: camera left the layout`, `reprojection_error left the layout` at 19:50:33Z).
- **Rerun drew no view.** A viewer frame that does not draw the streamed 3D view withdraws the scene (`DecoupledHeadset::end_viewer_frame`), so the glasses showed no content rather than the last frame. The run published 8031 scenes in about 154 s, against about 60 a second shown, consistent with none for 20 s. The views' rectangles come from Rerun's `ViewRectPublisher`, set only when a view's `ui` runs (`re_viewport` 0.40.0 `viewport_ui.rs` L484-L494), so the capture found no view on screen.
- **Rerun skips views in a viewport of no area.** `ViewportUi::viewport_ui` and the tiles' `pane_ui` return before drawing when `ui.available_size().min_elem() <= 0.0` (`re_viewport` 0.40.0 `viewport_ui.rs` L68-L70, L428-L430).
- **A minimised Windows window has no client area.** winit's `inner_size` is `GetClientRect` (`winit` 0.30.13 `platform_impl/windows/window.rs` L214-L223). egui-winit reports no screen when a side is zero (`egui-winit` 0.37.0 `lib.rs` L325-L334), and egui then keeps the last screen (`egui` 0.37.0 `input_state/mod.rs` L387). A minimised window that reports a small screen with area lays out in it, and Legion's does: build 1103b logged `the window is minimised (its screen 158x26 points)` (2026-11-03 20:40:23Z), a strip too small for any view.

## Decision

- **A minimised window lays out at its last shown size.** The viewer's `App::raw_input_hook` (eframe 0.37.0 `epi.rs` L282, called from `prepare_raw_input` before every egui pass) keeps the screen of the last pass whose window was not minimised and had area, and gives it to every pass while `ViewportInfo::minimized` is true (`MinimisedLayout`, `hidden_window.rs`). The first minimised pass logs once what screen the window reported and the size the viewer lays out at. The restore's first pass takes the window's own screen again.
- **A covered window is unchanged.** Only `minimized` counts; an occluded window keeps the screen it reports.
- **A minimised window that drew no view keeps the last scene.** `end_viewer_frame` keeps the scene in the handoff when the streamed view was not drawn and the window is minimised, so the glasses keep the last frame instead of going empty if the layout is lost anyway.
- **A minimised capture with no view keeps the panels.** The panel capture withdraws nothing when the window is minimised and no 2D view is on screen in the capture, so no panel is hidden for a minimise (`publish_on_screen_views`, `panel_capture.rs`).

## Alternatives considered

- **Keep the window from minimising.** A minimise is the owner's normal way to clear Legion's screen while the glasses are worn. Rejected.
- **Override the screen only when it has no area.** egui keeps the last screen when none is reported, so a check on area alone would miss a minimised window that reports a small screen. Rejected in favour of the `minimized` state.
- **Patch `re_viewport` to draw views in a viewport of no area.** A view drawn into no area has nothing to capture. Rejected.

## Consequences

- **Seen on Legion (build 1103b, METALENSE 2).** With the window minimised the 3D content and the timeline panel keep updating, the log has `headset: the window is minimised (its screen 158x26 points); the viewer lays out at its last shown size 1167x757 points` once per minimise, and no `left the layout` line.
- **The 2D view panels hold their last image while minimised.** They are crops of a window screenshot (`panel_capture.rs`), and a minimised window paints no frame, so no screenshot arrives until the restore; the panels stay shown, frozen. A covered window is painted and keeps them live. Live panels for a minimised window need the desktop UI drawn off-screen with eframe's own renderer, which is follow-up work (see `known-issues.md`).
- **macOS.** egui-winit reads `minimized` only at the window's creation on macOS (`egui-winit` 0.37.0 `lib.rs` L1503-L1508), so the hook does not act there; a minimised macOS window keeps its size.
- **The inline render (`--inline-render`).** The layout fix applies; the scene guard is on the decoupled path only.
