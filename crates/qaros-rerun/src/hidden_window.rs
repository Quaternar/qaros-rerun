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

#[cfg(test)]
mod tests {
    use super::*;

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
