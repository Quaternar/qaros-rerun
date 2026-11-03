//! The viewer window hidden (minimised or occluded) while a headset is attached (ADR-0097).
//!
//! eframe 0.37.0 runs only the app's `logic` for a hidden window (`if !show_ui` in
//! `wgpu_integration.rs`), and Rerun advances time, builds the scene the eyes are drawn from and
//! draws the 2D views the panels capture in `ui`. The app asks for the `ui` and the paint with
//! `egui::ViewportCommand::RequestPaintWhileHidden` in each hidden pass. This state is what the
//! surface status handler and the event loop read: a hidden window's surface is not
//! reconfigured (a minimised Windows window has no area), and while the headset streams the
//! event loop runs the passes at headset rate instead of eframe's 100 ms for a hidden window
//! (`INVISIBLE_WINDOW_REPAINT_INTERVAL` in `run.rs`).
//!
//! A minimised window also lays out at its last shown size (`MinimisedLayout`, ADR-0099), so
//! Rerun keeps drawing the views the scene and the panels come from.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use egui_wgpu::SurfaceErrorAction;

/// egui-wgpu's `WgpuConfiguration::on_surface_status`.
pub type SurfaceStatusHandler =
    Arc<dyn Fn(&wgpu::CurrentSurfaceTexture) -> SurfaceErrorAction + Send + Sync>;

#[derive(Clone, Debug, Default)]
pub struct HiddenWindow {
    is_hidden: Arc<AtomicBool>,
    wants_headset_frames: Arc<AtomicBool>,
}

impl HiddenWindow {
    /// Set by the app's `logic` at the start of every pass, before that pass paints.
    pub fn update(&self, is_hidden: bool, headset_is_streaming: bool) {
        self.is_hidden.store(is_hidden, Ordering::Release);
        self.wants_headset_frames
            .store(is_hidden && headset_is_streaming, Ordering::Release);
    }

    pub fn is_hidden(&self) -> bool {
        self.is_hidden.load(Ordering::Acquire)
    }

    /// The window is hidden and the headset streams, so the event loop drives the passes.
    pub fn wants_headset_frames(&self) -> bool {
        self.wants_headset_frames.load(Ordering::Acquire)
    }

    /// egui-wgpu's surface status handler, except that a hidden window skips the frame for any
    /// status: reconfiguring the surface of a minimised window would size it for a window of no
    /// area, and wgpu's reconfigure waits for the device, which times out while the render
    /// thread submits (ADR-0081). The `Resized` of the restore reconfigures it.
    pub fn surface_status_handler(&self, when_shown: SurfaceStatusHandler) -> SurfaceStatusHandler {
        let hidden_window = self.clone();
        Arc::new(move |status| {
            if hidden_window.is_hidden() {
                SurfaceErrorAction::SkipFrame
            } else {
                when_shown(status)
            }
        })
    }
}

/// Whether the window of this pass is minimised, as eframe reads it from the OS each pass
/// (`egui_winit::update_viewport_info`; not refreshed at runtime on macOS).
pub fn is_minimised(viewport: &egui::ViewportInfo) -> bool {
    viewport.minimized == Some(true)
}

/// The screen a minimised window lays out on (ADR-0099). Rerun draws no view in a viewport
/// of no area (`re_viewport` `viewport_ui.rs`, `is_zero_sized_viewport`), and a view it does
/// not draw publishes no scene and no panel rect. A minimised Windows window has no client
/// area, so while minimised every pass is given the screen of the last pass it was shown,
/// and the views keep their size and place.
#[derive(Debug, Default)]
pub struct MinimisedLayout {
    last_shown_screen: Option<egui::Rect>,
    minimised_screen_logged: bool,
}

impl MinimisedLayout {
    /// eframe's `App::raw_input_hook`, called before every egui pass with the window's state
    /// of that pass.
    pub fn lay_out_minimised_at_last_shown_size(&mut self, raw_input: &mut egui::RawInput) {
        let is_minimised_now = raw_input
            .viewports
            .get(&raw_input.viewport_id)
            .is_some_and(is_minimised);
        if !is_minimised_now {
            self.minimised_screen_logged = false;
            if let Some(screen) = raw_input.screen_rect.filter(has_area) {
                self.last_shown_screen = Some(screen);
            }
            return;
        }
        let Some(last_shown_screen) = self.last_shown_screen else {
            return;
        };
        if !self.minimised_screen_logged {
            self.minimised_screen_logged = true;
            re_log::info!(
                "headset: the window is minimised (its screen {}); the viewer lays out at its \
                 last shown size {:.0}x{:.0} points",
                raw_input.screen_rect.map_or_else(
                    || "not reported".to_owned(),
                    |screen| format!("{:.0}x{:.0} points", screen.width(), screen.height())
                ),
                last_shown_screen.width(),
                last_shown_screen.height()
            );
        }
        raw_input.screen_rect = Some(last_shown_screen);
    }
}

fn has_area(screen: &egui::Rect) -> bool {
    screen.width() > 0.0 && screen.height() > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_input(screen: Option<egui::Rect>, minimised: bool) -> egui::RawInput {
        let mut raw_input = egui::RawInput {
            screen_rect: screen,
            ..Default::default()
        };
        raw_input.viewports.insert(
            raw_input.viewport_id,
            egui::ViewportInfo {
                minimized: Some(minimised),
                ..Default::default()
            },
        );
        raw_input
    }

    fn screen(width: f32, height: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height))
    }

    #[test]
    fn a_minimised_window_lays_out_at_the_last_shown_screen_until_restored() {
        let mut layout = MinimisedLayout::default();

        let mut shown = pass_input(Some(screen(1600.0, 900.0)), false);
        layout.lay_out_minimised_at_last_shown_size(&mut shown);
        assert_eq!(shown.screen_rect, Some(screen(1600.0, 900.0)));

        for minimised_screen in [None, Some(screen(136.0, 0.0)), Some(screen(160.0, 28.0))] {
            let mut minimised = pass_input(minimised_screen, true);
            layout.lay_out_minimised_at_last_shown_size(&mut minimised);
            assert_eq!(minimised.screen_rect, Some(screen(1600.0, 900.0)));
        }

        let mut restored = pass_input(Some(screen(1280.0, 720.0)), false);
        layout.lay_out_minimised_at_last_shown_size(&mut restored);
        assert_eq!(restored.screen_rect, Some(screen(1280.0, 720.0)));

        let mut minimised_again = pass_input(None, true);
        layout.lay_out_minimised_at_last_shown_size(&mut minimised_again);
        assert_eq!(minimised_again.screen_rect, Some(screen(1280.0, 720.0)));
    }

    #[test]
    fn a_covered_window_keeps_its_own_screen() {
        let mut layout = MinimisedLayout::default();
        let mut shown = pass_input(Some(screen(1600.0, 900.0)), false);
        layout.lay_out_minimised_at_last_shown_size(&mut shown);

        let mut covered = pass_input(Some(screen(1500.0, 800.0)), false);
        if let Some(viewport) = covered.viewports.get_mut(&covered.viewport_id) {
            viewport.occluded = Some(true);
        }
        layout.lay_out_minimised_at_last_shown_size(&mut covered);
        assert_eq!(covered.screen_rect, Some(screen(1500.0, 800.0)));
    }

    #[test]
    fn a_window_minimised_before_it_was_ever_shown_is_left_as_reported() {
        let mut layout = MinimisedLayout::default();
        let mut minimised = pass_input(None, true);
        layout.lay_out_minimised_at_last_shown_size(&mut minimised);
        assert_eq!(minimised.screen_rect, None);
    }

    #[test]
    fn a_shown_pass_without_area_keeps_the_last_shown_screen() {
        let mut layout = MinimisedLayout::default();
        let mut shown = pass_input(Some(screen(1600.0, 900.0)), false);
        layout.lay_out_minimised_at_last_shown_size(&mut shown);
        let mut empty = pass_input(Some(screen(0.0, 0.0)), false);
        layout.lay_out_minimised_at_last_shown_size(&mut empty);

        let mut minimised = pass_input(None, true);
        layout.lay_out_minimised_at_last_shown_size(&mut minimised);
        assert_eq!(minimised.screen_rect, Some(screen(1600.0, 900.0)));
    }

    fn reconfigure_always() -> SurfaceStatusHandler {
        Arc::new(|_| SurfaceErrorAction::Reconfigure)
    }

    #[test]
    fn a_hidden_window_skips_the_frame_and_a_shown_one_keeps_the_default() {
        let hidden_window = HiddenWindow::default();
        let handler = hidden_window.surface_status_handler(reconfigure_always());

        hidden_window.update(true, false);
        assert!(matches!(
            handler(&wgpu::CurrentSurfaceTexture::Outdated),
            SurfaceErrorAction::SkipFrame
        ));

        hidden_window.update(false, false);
        assert!(matches!(
            handler(&wgpu::CurrentSurfaceTexture::Outdated),
            SurfaceErrorAction::Reconfigure
        ));
    }

    #[test]
    fn the_event_loop_drives_frames_only_while_hidden_and_streaming() {
        let hidden_window = HiddenWindow::default();
        hidden_window.update(true, true);
        assert!(hidden_window.wants_headset_frames());
        hidden_window.update(true, false);
        assert!(!hidden_window.wants_headset_frames());
        assert!(hidden_window.is_hidden());
        hidden_window.update(false, true);
        assert!(!hidden_window.wants_headset_frames());
        assert!(!hidden_window.is_hidden());
    }
}
