---
id: ADR-0097
title: A hidden viewer window keeps running the viewer for the headset
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0097: A hidden viewer window keeps running the viewer for the headset

> Amends ADR-0078 (the timeline panel drawn in `logic` is no longer the only thing that runs while the window is hidden) and ADR-0081 (the event loop that parks the render thread on a resize also drives a hidden window's passes).

## Context

In run 4 of the 1102d test on METALENSE 2 (2026-11-03, the `structure_from_motion` run from 13:02Z), the desktop viewer window was minimised while the glasses were worn. The headset's 3D view and the 2D view panels froze: time did not advance, and the timeline panel's slider moved but no view followed it. The owner wears the glasses with Legion's screen ignored, so a minimised or covered window is the normal case.

- **eframe runs no `ui` for a hidden window.** In eframe 0.37.0 a viewport that is not visible (`ViewportInfo::visible`, false when minimised or occluded) gets only `App::logic`, through `Context::run_logic`, and no egui pass or paint (`native/wgpu_integration.rs`, `run_ui_and_paint`, `if !show_ui`).
- **The headset needs `ui`.** Rerun 0.40.0 advances the time control, builds the 3D view whose draw data the eyes are rendered from (ADR-0081 hands that scene to the render thread), and draws the 2D views the panels capture, all in `App::ui`. The slider's commands reached the time control in `logic` (ADR-0078), but nothing redrew the views.
- **eframe can run `ui` for a hidden window when asked.** `egui::ViewportCommand::RequestPaintWhileHidden` (egui 0.37.0 `viewport.rs`) makes the next pass of a hidden window run `ui` and paint; it holds for one pass. The command sent in a pass is processed after that pass (`handle_viewport_output`), so a command sent in each pass keeps the following ones full.
- **A hidden window is repainted at most every 100 ms.** eframe throttles repaints of a minimised or invisible window to `INVISIBLE_WINDOW_REPAINT_INTERVAL` and sleeps 10 ms after each of its passes (`native/run.rs`, `winit_integration.rs` `sleep_if_invisible_or_minimized`). A covered macOS window gets no `RedrawRequested` and runs `logic` only, every 250 ms (`MISSED_REDRAW_TIMEOUT`). The scene and the panels would follow the recording at about 10 frames per second.
- **A minimised window's surface must not be reconfigured.** On Windows a minimised window has no area; eframe ignores its zero `Resized` (`wgpu_integration.rs`, winit #208), so the surface keeps its last size. An acquire that fails while hidden takes egui-wgpu's default action, which reconfigures on `Outdated` (`egui-wgpu` `WgpuConfiguration::default`). That would size a swapchain for a window of no area, and wgpu's reconfigure waits for the device, which times out while the render thread submits (ADR-0081). A failed acquire still renders a pending screenshot into a texture of its own (`winit.rs`, `paint_and_update_textures`), which is what the panels capture.

## Decision

- **Each hidden pass asks for the next to run `ui`.** While a headset is attached (a sink on the UI thread, or the decoupled render thread), the app's `logic` sends `RequestPaintWhileHidden` whenever the window is not visible, and asks for a repaint while the headset streams (`HeadsetViewerApp::keep_ui_running_while_hidden`, `viewer_app.rs`). The first hidden pass is `logic` only; every pass after it runs `ui`, the scene and the panel captures as when the window is shown. Without a headset, eframe's own logic-only passes stay.
- **A hidden window's surface skips failed frames.** The viewer wraps egui-wgpu's surface status handler: while the window is hidden, any acquire status other than success skips the frame (`HiddenWindow::surface_status_handler`, `hidden_window.rs`). The `Resized` of the restore reconfigures the surface as before, with the render thread parked (ADR-0081).
- **The event loop drives the passes at headset rate.** With the render thread (the default), while the window is hidden and the headset streams, the event loop of our own (`window_resize.rs`) runs the window's pass every 11 ms, as a delivered `RedrawRequested` does, and wakes for the next one; eframe's 10 ms sleep after a hidden pass still applies. The inline render (`--inline-render`) runs on eframe's own loop and keeps eframe's 100 ms.
- **Only where the viewer owns the surface handler.** The state is installed in `run_windowed` when the viewer sets its wgpu setup or runs a render thread, which every QAROS run does. Rerun's own `run_native_app` path is unchanged.

## Alternatives considered

- **Run the Rerun app's `ui` from `logic` ourselves.** `logic` runs inside `Context::run_logic`, which forbids an egui pass, and a pass on a second context would not share the textures and memory the Rerun app keeps in the window's context. Rejected.
- **Keep the window from counting as hidden.** eframe reads the window's state from the OS (`Window::is_minimized`, winit's `Occluded`); there is no option to override it. Rejected.
- **Move the time advance and the scene build out of `ui`.** Rerun 0.40.0 builds the views' draw data inside the view classes' `ui`, and the panels capture the painted views; moving them is a larger patch of `re_viewer` than this. Rejected.
- **Only send `RequestPaintWhileHidden`.** Correct, but eframe's throttle leaves the scene at about 10 frames per second while hidden. Kept as the inline-render behaviour only.

## Consequences

- **What to check on Legion.** Minimise the viewer while the glasses show a playing recording: time keeps advancing in the 3D view and the panels, and the slider moves the views. Nothing new is logged; a frame the surface skips while hidden is silent.
- **GPU work while hidden.** The viewer draws its window every pass while hidden, as when it is shown, and presents nothing when the acquire fails.
- **Not verified on Windows yet.** What a minimised Windows window's acquire returns on Legion's NVIDIA driver (`Outdated` at once, or a wait up to wgpu's acquire timeout) is untested; a wait would slow the hidden passes, and the eyes, drawn by the render thread, would not be affected.
