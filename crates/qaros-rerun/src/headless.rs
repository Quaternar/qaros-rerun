//! Runs the viewer without a window, driven by `egui_kittest` the way Rerun's own
//! `re_viewer::run_headless_app` does, but with the headset wrapper app.
//!
//! The headset path needs no window: eye views are submitted on the viewer's queue directly.
//! Panel capture needs the window's pixels, so here the runner renders the UI off screen with
//! the harness and crops the views from that image.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use frame_sink::perf;

use rerun::external::{eframe, re_viewer};

use crate::panel_capture::PanelCapture;
use crate::panel_render::PanelRenderer;
use crate::viewer_app::{HeadsetViewerApp, IDLE_REPAINT_INTERVAL};

/// When egui wants its next pass, as asked through its repaint callback from any thread (a
/// peer update, a render request, Stop, Rerun data arriving) since the last pass began.
#[derive(Default)]
struct RepaintSchedule {
    next_pass: Mutex<Option<Instant>>,
    requested: Condvar,
}

impl RepaintSchedule {
    fn request_after(&self, delay: Duration) {
        let due = Instant::now().checked_add(delay);
        let mut next_pass = self
            .next_pass
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(due) = due
            && next_pass.is_none_or(|scheduled| due < scheduled)
        {
            *next_pass = Some(due);
            self.requested.notify_all();
        }
    }

    /// The pass about to run covers every request made so far.
    fn begin_pass(&self) {
        *self
            .next_pass
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Blocks until a requested pass is due or `latest` passes, whichever comes first.
    fn wait_until_due(&self, latest: Instant) {
        let mut next_pass = self
            .next_pass
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = Instant::now();
            let due = next_pass.map_or(latest, |scheduled| scheduled.min(latest));
            if due <= now {
                return;
            }
            next_pass = self
                .requested
                .wait_timeout(next_pass, due - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

pub struct HeadlessOptions {
    pub size: egui::Vec2,
    pub quit_when_done: bool,
    pub step_interval: Duration,
    /// From `device_setup`: Rerun's own, a forced backend, or the viewer's own Vulkan device.
    pub wgpu_setup: egui_wgpu::WgpuSetup,
}

pub fn run_headless(
    options: HeadlessOptions,
    mut panel_capture: Option<PanelCapture>,
    create_app: impl FnOnce(&eframe::CreationContext<'_>) -> HeadsetViewerApp,
) -> Result<(), String> {
    let mut setup_result = Ok(());
    let setup_result_slot = &mut setup_result;
    let mut harness = egui_kittest::Harness::<HeadsetViewerApp>::builder()
        .with_size(options.size)
        // egui shortens every delayed repaint by the predicted pass time; the harness's default
        // (0.25 s) would turn the idle and panel capture repaints into immediate ones.
        .with_step_dt(options.step_interval.as_secs_f32())
        // The harness's test defaults panic on text no font can draw and on unnamed widgets; a
        // viewer showing users' data draws tofu instead, as the windowed viewer does.
        .allow_missing_glyphs()
        .with_accessibility_check(false)
        .renderer(PanelRenderer::new(options.wgpu_setup))
        .build_eframe(move |creation_context| {
            *setup_result_slot = re_viewer::customize_eframe_and_setup_renderer(creation_context)
                .map_err(|error| error.to_string());
            create_app(creation_context)
        });
    setup_result?;
    let repaint_schedule = Arc::new(RepaintSchedule::default());
    {
        let repaint_schedule = Arc::clone(&repaint_schedule);
        harness.ctx.set_request_repaint_callback(move |request| {
            repaint_schedule.request_after(request.delay);
        });
    }
    re_log::info!(
        "headless viewer running at {}x{} points",
        options.size.x,
        options.size.y
    );

    let started = std::time::Instant::now();
    loop {
        // Without a time, egui advances its clock by the harness's fixed step (0.25 s) every
        // pass, and Rerun's playback (at most 0.1 s times the speed per pass) follows the pass
        // count instead of the wall clock.
        harness.input_mut().time = Some(started.elapsed().as_secs_f64());
        repaint_schedule.begin_pass();
        {
            let _step = perf::Span::start("step");
            harness.step();
        }
        let pass_ended = Instant::now();

        if let Some(panel_capture) = &mut panel_capture
            && panel_capture.take_due()
        {
            // `PanelRenderer` locks its queue submits against a render thread's raw Vulkan ones.
            let rendered = harness.render();
            match rendered {
                Ok(image) => {
                    let size = [image.width() as usize, image.height() as usize];
                    let color_image =
                        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
                    panel_capture.publish_views(&harness.ctx, &color_image);
                }
                Err(error) => re_log::warn_once!("panels: headless render failed: {error}"),
            }
        }

        if harness.state().is_stop_requested() {
            harness.state_mut().exit_after_qaros_teardown();
        }
        if options.quit_when_done && harness.state().is_done() {
            harness.state_mut().print_summary_once();
            harness.state_mut().leave_qaros();
            perf::flush();
            return Ok(());
        }
        // While a headset streams, the next viewer frame starts at once: a user's next frame
        // can begin only in a later viewer frame than its last show, so a pause between
        // viewer frames lowers every user's rate. Otherwise the next pass waits for what egui
        // scheduled, at least `step_interval` and at most `IDLE_REPAINT_INTERVAL` away.
        let _sleep = perf::Span::start("sleep");
        if !harness.state().headset_is_streaming() {
            std::thread::sleep(options.step_interval);
            if let Some(panel_capture) = &panel_capture {
                repaint_schedule.request_after(panel_capture.until_due());
            }
            repaint_schedule.wait_until_due(pass_ended + IDLE_REPAINT_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LATEST: Duration = Duration::from_millis(400);

    fn waited_with(request: impl FnOnce(&Arc<RepaintSchedule>)) -> Duration {
        let schedule = Arc::new(RepaintSchedule::default());
        request(&schedule);
        let started = Instant::now();
        schedule.wait_until_due(started + LATEST);
        started.elapsed()
    }

    #[test]
    fn with_nothing_requested_the_wait_lasts_until_the_latest_pass() {
        let waited = waited_with(|_| {});
        assert!(waited >= LATEST, "waited {waited:?}");
    }

    #[test]
    fn an_immediate_request_ends_the_wait_at_once() {
        let waited = waited_with(|schedule| schedule.request_after(Duration::ZERO));
        assert!(waited < Duration::from_millis(50), "waited {waited:?}");
    }

    #[test]
    fn a_delayed_request_ends_the_wait_at_its_delay() {
        let waited = waited_with(|schedule| schedule.request_after(Duration::from_millis(100)));
        assert!(
            (Duration::from_millis(100)..Duration::from_millis(300)).contains(&waited),
            "waited {waited:?}"
        );
    }

    #[test]
    fn a_request_from_another_thread_wakes_the_wait() {
        let waited = waited_with(|schedule| {
            let schedule = Arc::clone(schedule);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                schedule.request_after(Duration::ZERO);
            });
        });
        assert!(
            (Duration::from_millis(50)..Duration::from_millis(300)).contains(&waited),
            "waited {waited:?}"
        );
    }

    #[test]
    fn a_new_pass_forgets_the_requests_it_covers() {
        let waited = waited_with(|schedule| {
            schedule.request_after(Duration::ZERO);
            schedule.begin_pass();
        });
        assert!(waited >= LATEST, "waited {waited:?}");
    }

    #[test]
    fn a_later_request_does_not_postpone_an_earlier_one() {
        let waited = waited_with(|schedule| {
            schedule.request_after(Duration::from_millis(50));
            schedule.request_after(Duration::from_secs(10));
        });
        assert!(
            (Duration::from_millis(50)..Duration::from_millis(300)).contains(&waited),
            "waited {waited:?}"
        );
    }
}
