use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frame_sink::perf;
use rerun::external::{eframe, re_viewer};

use crate::decoupled::DecoupledHeadset;
use crate::headset::{HeadsetRenderer, HeadsetSink, ViewerHead, with_sink_lent_to_pass};
use crate::memory_budget_log::MemoryBudgetLog;
use crate::panel_capture::PanelCapture;
use crate::timeline_scrub::{TimelineScrub, TimelineSource};

#[cfg(all(feature = "qaros", not(test)))]
pub(crate) use qaros_sink::end_process_after_teardown;

/// Without the QAROS sink no QAROS library was loaded, so a normal exit is safe.
#[cfg(all(not(feature = "qaros"), not(test)))]
pub(crate) fn end_process_after_teardown(code: i32) -> ! {
    perf::flush();
    std::process::exit(code)
}

/// The exit code a test's process end unwinds with, in place of ending the test binary.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProcessEnded(pub i32);

#[cfg(test)]
pub(crate) fn end_process_after_teardown(code: i32) -> ! {
    std::panic::panic_any(ProcessEnded(code))
}

pub type SharedHeadset = Arc<Mutex<HeadsetRenderer>>;

/// Set from another thread to close the viewer: QAROS closes stdin, Ctrl+C, a closed console.
pub type StopRequest = Arc<AtomicBool>;

/// The renderer is shared with the 3D view observer; the sink stays with the app on the UI
/// thread and is handed to the renderer per call.
pub struct Headset {
    pub renderer: SharedHeadset,
    pub sink: HeadsetSink,
}

impl Headset {
    fn renderer(&self) -> MutexGuard<'_, HeadsetRenderer> {
        self.renderer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_streaming(&self) -> bool {
        self.sink.is_streaming() || self.renderer().has_frames_open()
    }

    fn is_finished(&self) -> bool {
        self.sink.is_finished() && !self.renderer().has_frames_open()
    }
}

/// How often a viewer with a headset sink but nobody to render for repaints anyway, so the
/// sink polls the peer list (every 2 s, `qaros-sink` `users.rs`). Peer updates and render
/// requests wake it at once.
pub const IDLE_REPAINT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

impl Drop for Headset {
    /// The renderer outlives the sink (the 3D view observer keeps it), and its open frames may
    /// hold textures over the sink's images: they are let go before the sink goes.
    fn drop(&mut self) {
        self.renderer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .abandon_all(&mut self.sink);
    }
}

/// What a [`FrameHook`] learns about the headset each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeadsetFrame {
    /// The headset sink streams to some user in this frame.
    pub is_streaming: bool,
    /// The head of every user an eye frame was prepared for lately, in room space, by user;
    /// empty until a stream has started.
    pub heads: Vec<ViewerHead>,
}

/// Work that runs once per viewer frame on the UI thread, after the views, for example
/// keeping QAROS GUI panels in step with the published 2D views.
pub trait FrameHook {
    fn on_viewer_frame(&mut self, headset: &HeadsetFrame);
}

/// The stock Rerun viewer app plus what runs once per frame outside the views: the headset
/// sink, continuous repaint while a stream is active, panel capture and the timeline
/// panel's slider, frame hooks, the Vulkan memory budget log, and an optional exit once the
/// sink is done. `Viewer` is the Rerun app except in tests.
pub struct HeadsetViewerApp<Viewer = re_viewer::App> {
    pub inner: Viewer,
    pub headset: Option<Headset>,
    /// `--decoupled-render`: the eyes and the sink are on a render thread; `headset` is `None`.
    pub decoupled: Option<DecoupledHeadset>,
    /// `QAROS_RERUN_SCENE_DELAY_MS`: a stand-in for a heavy scene, slept after every UI pass.
    pub scene_delay: Option<std::time::Duration>,
    pub panel_capture: Option<PanelCapture>,
    /// With `--panels`: the timeline panel's slider requests, applied before the Rerun app's logic.
    pub timeline_scrub: Option<TimelineScrub>,
    pub frame_hooks: Vec<Box<dyn FrameHook>>,
    pub quit_when_done: bool,
    pub stop_request: Option<StopRequest>,
    pub summary_printed: bool,
    pub memory_budget_log: Option<MemoryBudgetLog>,
}

impl<Viewer> HeadsetViewerApp<Viewer> {
    pub fn headset_is_streaming(&self) -> bool {
        self.headset.as_ref().is_some_and(Headset::is_streaming)
            || self
                .decoupled
                .as_ref()
                .is_some_and(DecoupledHeadset::is_streaming)
    }

    pub fn is_stop_requested(&self) -> bool {
        self.stop_request
            .as_ref()
            .is_some_and(|stop_request| stop_request.load(Ordering::Acquire))
    }

    /// True once a headset sink exists and has finished streaming (a frame limit reached);
    /// a sink idle without users is not done.
    pub fn is_done(&self) -> bool {
        self.headset.as_ref().is_some_and(Headset::is_finished)
            || self
                .decoupled
                .as_ref()
                .is_some_and(DecoupledHeadset::is_finished)
    }

    pub fn print_summary_once(&mut self) {
        if self.summary_printed {
            return;
        }
        if let Some(headset) = &self.headset {
            re_log::info!("headset: {}", headset.renderer().statistics().summary());
        }
        if let Some(decoupled) = &self.decoupled {
            re_log::info!("headset: {}", decoupled.summary());
        }
        self.summary_printed = true;
    }

    /// QAROS stopped the viewer (stdin closed) and kills it about 1.3 s later
    /// (`onboarding-and-sessions.mdx` L278 at `b1338df`). Tears down what QAROS sees first: the
    /// headset's sink with its render senders and the frame hooks with the GUI panels; the
    /// connection (session, runtime, library) goes with the last of them. Then exits at once:
    /// dropping a large Rerun store, the panel server and the async runtime took over a second
    /// on a heavy scene, enough to miss the window. On Windows the process is terminated
    /// rather than exited, so QAROS DLLs still mapped do not run their unload code
    /// (`qaros_sink::end_process_after_teardown`).
    pub fn exit_after_qaros_teardown(&mut self) -> ! {
        re_log::info!("headset: stop requested; leaving QAROS and exiting");
        self.print_summary_once();
        let teardown_started = std::time::Instant::now();
        self.leave_qaros();
        re_log::info!(
            "qaros: teardown took {} ms; exiting without the viewer's own teardown",
            teardown_started.elapsed().as_millis()
        );
        end_process_after_teardown(0);
    }

    /// `--decoupled-render`: the render thread panicked or got no render context, and has torn
    /// the sink down already. Ends the run as a Stop does, with exit code 1, so neither a
    /// `--quit-when-done` run nor the viewer keeps running without a headset.
    pub fn exit_after_render_thread_failure(&mut self) -> ! {
        re_log::error!("headset: the render thread failed; leaving QAROS and exiting");
        self.print_summary_once();
        self.leave_qaros();
        end_process_after_teardown(1);
    }

    fn has_render_thread_failed(&self) -> bool {
        self.decoupled
            .as_ref()
            .is_some_and(DecoupledHeadset::has_failed)
    }

    /// Drops the headset's sink with its render senders and the frame hooks with the GUI
    /// panels; the connection (session, runtime, library) goes with the last of them.
    pub fn leave_qaros(&mut self) {
        #[cfg(feature = "qaros")]
        qaros_sink::enter_teardown_step(qaros_sink::TeardownStep::LeaveSink);
        self.headset = None;
        if let Some(decoupled) = self.decoupled.take() {
            // The render thread drops the sink, then the panel hooks, before it ends.
            drop(decoupled);
            re_log::info!("headset: render thread joined");
        }
        self.frame_hooks.clear();
    }
}

impl<Viewer: eframe::App + TimelineSource> eframe::App for HeadsetViewerApp<Viewer> {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.inner.clear_color(visuals)
    }

    fn persist_egui_memory(&self) -> bool {
        self.inner.persist_egui_memory()
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.inner.save(storage);
    }

    /// eframe calls `logic` before every `ui`, and alone while the window is minimised or
    /// occluded (`eframe` 0.37.0 `wgpu_integration.rs`, `if !show_ui`), so the stop is checked
    /// here: a check in `ui` alone never runs for a minimised window. The Rerun app runs its
    /// pending commands in its `logic`, so the slider's commands go in just before it.
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.is_stop_requested() {
            self.exit_after_qaros_teardown();
        }
        if self.has_render_thread_failed() {
            self.exit_after_render_thread_failure();
        }
        if let Some(timeline_scrub) = &self.timeline_scrub
            && timeline_scrub.apply(&self.inner)
        {
            ctx.request_repaint();
        }
        self.inner.logic(ctx, frame);
        if let Some(timeline_scrub) = &self.timeline_scrub {
            timeline_scrub.publish_position(&self.inner);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let viewer_frame = ui.ctx().cumulative_pass_nr();
        match &mut self.headset {
            Some(headset) => {
                headset.renderer().begin_viewer_frame(&headset.sink);
                let _ui = perf::Span::start("ui");
                with_sink_lent_to_pass(&mut headset.sink, || self.inner.ui(ui, frame));
            }
            None => match &mut self.decoupled {
                Some(decoupled) => {
                    decoupled.share_device(frame);
                    let _ui = perf::Span::start("ui");
                    self.inner.ui(ui, frame);
                }
                None => self.inner.ui(ui, frame),
            },
        }
        if let Some(scene_delay) = self.scene_delay {
            let _delay = perf::Span::start("scene_delay");
            std::thread::sleep(scene_delay);
        }
        if let Some(decoupled) = &mut self.decoupled {
            decoupled.end_viewer_frame();
        }
        if perf::is_enabled() {
            let poll_wait_micros =
                re_renderer::POLL_DEVICE_WAIT_MICROS.swap(0, std::sync::atomic::Ordering::Relaxed);
            perf::record("rr_poll_wait", None, u128::from(poll_wait_micros));
            perf::record(
                "rr_resources_created",
                None,
                u128::from(
                    re_renderer::POOL_RESOURCES_CREATED.load(std::sync::atomic::Ordering::Relaxed),
                ),
            );
        }

        if let (Some(headset), Some(render_state)) = (&mut self.headset, frame.wgpu_render_state())
        {
            let egui_renderer = render_state.renderer.read();
            if let Some(render_context) = egui_renderer
                .callback_resources
                .get::<re_renderer::RenderContext>()
            {
                let _end = perf::Span::start("end_viewer_frame");
                headset
                    .renderer
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .end_viewer_frame(render_context, viewer_frame, &mut headset.sink);
            }
        }

        if let Some(panel_capture) = &mut self.panel_capture {
            panel_capture.on_frame(ui.ctx());
        }
        let headset_is_streaming = self.headset_is_streaming();
        let headset_frame = HeadsetFrame {
            is_streaming: headset_is_streaming,
            heads: self.headset.as_ref().map_or_else(Vec::new, |headset| {
                headset.renderer().recent_heads(std::time::Instant::now())
            }),
        };
        for hook in &mut self.frame_hooks {
            hook.on_viewer_frame(&headset_frame);
        }
        if let Some(memory_budget_log) = &mut self.memory_budget_log {
            memory_budget_log.log_if_due();
        }

        if headset_is_streaming {
            ui.ctx().request_repaint();
        } else if self.is_done() {
            self.print_summary_once();
            if self.quit_when_done {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else if self.headset.is_some() || self.decoupled.is_some() {
            ui.ctx().request_repaint_after(IDLE_REPAINT_INTERVAL);
        }
    }

    fn on_exit(&mut self) {
        self.print_summary_once();
        self.leave_qaros();
        self.inner.on_exit();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::rc::Rc;

    use panel_server::{TimelinePosition, TimelineRequest, TimelineRequests};
    use re_viewer_context::external::re_log_types::{
        AbsoluteTimeRange, StoreId, StoreKind, TimeInt,
    };
    use re_viewer_context::{SystemCommand, command_channel};

    use super::*;
    use crate::timeline_scrub::ActiveTimeline;

    #[derive(Default)]
    struct RecordingViewer {
        logic_passes: Rc<Cell<u32>>,
    }

    impl eframe::App for RecordingViewer {
        fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
            self.logic_passes.set(self.logic_passes.get() + 1);
        }

        fn ui(&mut self, _ui: &mut egui::Ui, _frame: &mut eframe::Frame) {}
    }

    impl TimelineSource for RecordingViewer {
        fn active_timeline(&self) -> Option<ActiveTimeline> {
            Some(ActiveTimeline {
                store_id: StoreId::random(StoreKind::Recording, "test"),
                range: AbsoluteTimeRange::new(TimeInt::new_temporal(0), TimeInt::new_temporal(10)),
                cursor: TimeInt::new_temporal(4),
            })
        }
    }

    /// Stands for the GUI panels: dropped at the teardown.
    struct PanelsHook {
        dropped: Rc<Cell<bool>>,
    }

    impl FrameHook for PanelsHook {
        fn on_viewer_frame(&mut self, _headset: &HeadsetFrame) {}
    }

    impl Drop for PanelsHook {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }

    struct TestApp {
        app: HeadsetViewerApp<RecordingViewer>,
        stop_request: StopRequest,
        logic_passes: Rc<Cell<u32>>,
        hook_dropped: Rc<Cell<bool>>,
    }

    fn test_app() -> TestApp {
        let stop_request: StopRequest = Arc::new(AtomicBool::new(false));
        let viewer = RecordingViewer::default();
        let logic_passes = Rc::clone(&viewer.logic_passes);
        let hook_dropped = Rc::new(Cell::new(false));
        let app = HeadsetViewerApp {
            inner: viewer,
            headset: None,
            decoupled: None,
            scene_delay: None,
            panel_capture: None,
            timeline_scrub: None,
            frame_hooks: vec![Box::new(PanelsHook {
                dropped: Rc::clone(&hook_dropped),
            })],
            quit_when_done: false,
            stop_request: Some(Arc::clone(&stop_request)),
            summary_printed: false,
            memory_budget_log: None,
        };
        TestApp {
            app,
            stop_request,
            logic_passes,
            hook_dropped,
        }
    }

    /// The pass eframe runs for a minimised window: `logic` and no `ui`.
    fn logic_only_pass(app: &mut HeadsetViewerApp<RecordingViewer>) -> Option<ProcessEnded> {
        let ctx = egui::Context::default();
        let mut frame = eframe::Frame::_new_kittest();
        let pass = catch_unwind(AssertUnwindSafe(|| {
            eframe::App::logic(app, &ctx, &mut frame);
        }));
        match pass {
            Ok(()) => None,
            Err(payload) => Some(
                *payload
                    .downcast::<ProcessEnded>()
                    .unwrap_or_else(|_| panic!("the pass panicked for another reason")),
            ),
        }
    }

    #[test]
    fn a_failed_render_thread_ends_the_process_with_an_error_after_the_teardown() {
        let mut test = test_app();
        test.app.decoupled = Some(DecoupledHeadset::failed_without_a_thread());
        assert_eq!(logic_only_pass(&mut test.app), Some(ProcessEnded(1)));
        assert!(test.app.decoupled.is_none(), "the render thread was joined");
        assert!(test.hook_dropped.get(), "the panels went with the teardown");
        assert_eq!(test.logic_passes.get(), 0);
    }

    #[test]
    fn a_stop_requested_while_the_window_is_minimised_tears_down_and_ends_the_process() {
        let mut test = test_app();

        assert_eq!(logic_only_pass(&mut test.app), None, "no stop yet");
        assert_eq!(test.logic_passes.get(), 1, "logic reaches the Rerun app");
        assert!(!test.hook_dropped.get());

        test.stop_request.store(true, Ordering::Release);
        assert_eq!(logic_only_pass(&mut test.app), Some(ProcessEnded(0)));
        assert!(test.hook_dropped.get(), "the panels went with the teardown");
        assert!(test.app.frame_hooks.is_empty());
        assert_eq!(
            test.logic_passes.get(),
            1,
            "the Rerun app's logic does not run once the stop is seen"
        );
    }

    #[test]
    fn slider_requests_reach_the_viewer_in_the_logic_pass_also_while_minimised() {
        let mut test = test_app();
        let (command_sender, command_receiver) = command_channel();
        let requests = TimelineRequests::default();
        let position = TimelinePosition::default();
        test.app.timeline_scrub = Some(TimelineScrub::new(
            requests.clone(),
            position.clone(),
            command_sender,
        ));

        requests.push(TimelineRequest::SeekAndPause { fraction: 0.5 });
        assert_eq!(logic_only_pass(&mut test.app), None);

        let Some((_, SystemCommand::TimeControlCommands { time_commands, .. })) =
            command_receiver.recv_system()
        else {
            panic!("no time control commands sent");
        };
        assert_eq!(
            format!("{time_commands:?}"),
            "[SetPlayState(Paused), SetTimeClamped(TimeReal(5))]"
        );
        assert_eq!(test.logic_passes.get(), 1, "and the Rerun app's logic ran");
        assert_eq!(
            position.latest(),
            Some(0.4),
            "the pass published the cursor the viewer shows"
        );
    }
}
