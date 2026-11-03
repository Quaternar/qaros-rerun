---
id: ADR-0103
title: A minimised window's geometry is never saved, and a window restored too small opens at Rerun's size
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0103: A minimised window's geometry is never saved, and a window restored too small opens at Rerun's size

> Amends ADR-0097 and ADR-0099 (what a minimised window's full passes do to the saved window).

## Context

In the 1103b test on Legion (2026-11-03), a run ended with the viewer window minimised. The next start, of the drone lidar demo at 20:44Z, never showed a window: the viewer laid out at 158x64 points, published no scene and opened no panel. Rerun's saved state, `%APPDATA%\rerun\data\app.ron`, held `"window": "(normal_geometry:(outer_position_pixels:Some((x:-32000.0,y:-32000.0)),scale_factor:1.5,inner_size_points:(x:158.0,y:26.0)),fullscreen:false,maximized:false)"`.

- **eframe saves the window as it is at the autosave.** `EpiIntegration::save` writes `WindowSettings::from_window` under the `window` key (`eframe` 0.37.0 `native/epi_integration.rs` L470-L489). `WindowGeometry::from_window` reads the window's outer position and inner size at that moment and keeps an earlier geometry only while the window is maximised or fullscreen (`egui-winit` 0.37.0 `window_settings.rs` L32-L76). Windows parks a minimised window at -32000,-32000 with a client area of a title strip.
- **A minimised window's full pass now reaches the autosave.** `maybe_autosave` runs at the end of a full pass (`wgpu_integration.rs` L908). A hidden window without a paint request runs `logic` alone and returns before it (L715-L767); since ADR-0097 every hidden pass asks for paint (`RequestPaintWhileHidden`, `egui-winit` 0.37.0 `lib.rs` L1922-L1924), so a minimised window is saved every autosave interval.
- **The exit save does not rescue it.** The viewer exits through `process_exit` without eframe's teardown, so the last autosave is what the next start reads.
- **The restore clamps the place but not into a usable size.** `clamp_size_to_sane_values` raises each side to at least 64 points, and on Windows `clamp_position_to_monitors` moves the window onto a monitor (`window_settings.rs` L178-L224, called from `epi_integration.rs` L37-L42). The window came back at 158x64 points in a corner, which the owner did not find.

## Decision

- **A minimised window's geometry is never saved.** The viewer's `App::save` runs after eframe wrote the window settings and before the flush (`epi_integration.rs` L479-L499). While the last pass was minimised, it puts back the window settings of the last save while the window was shown, or removes the key when there was none, so the next start uses Rerun's own window (`WindowPersistence::keep_shown_window_settings`, `window_persistence.rs`). Everything else Rerun saves stays as Rerun wrote it.
- **A window restored too small opens at Rerun's size.** On the first shown pass of a run, a screen narrower than 320 or lower than 450 points, Rerun's least window size (`re_viewer` 0.40.0 `native.rs` L46), sends the window to 40,40 at Rerun's first size of 1600x1200 points (L45), kept within nine tenths of the monitor, and logs `headset: the window was restored at ...`. It acts once a run, so a window someone shrinks later is left alone.

## Alternatives considered

- **Turn off window persistence.** Every start would forget where the owner put the window. Rejected.
- **Stop autosaving.** The autosave is the only save, since the viewer exits without eframe's teardown, so the blueprint and settings would not be kept either. Rejected.
- **Only repair the restored window.** A minimised save would still be written every autosave and repaired at every start, with a jump each time. Kept as the second guard, not the only one.

## Consequences

- **What to check on Legion.** End a run with the window minimised, then start again: the window opens where it was last shown, at that size, and `app.ron`'s `window` has no -32000 position.
- **A geometry already saved minimised.** The first start opens the window at 40,40 at Rerun's size and logs why; the next autosave saves that shown window.
- **macOS.** egui-winit reads `minimized` only at the window's creation on macOS (`egui-winit` 0.37.0 `lib.rs` L1503-L1508), so the save guard does not act there; macOS does not park a minimised window off-screen.
