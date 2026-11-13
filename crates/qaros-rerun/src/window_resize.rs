//! The window on an event loop of our own, so a resize reaches us before eframe: eframe
//! reconfigures the window's surface inside its `Resized` handler, and on Windows paints there
//! too, with nothing of the app's running first. With `--decoupled-render` the render thread
//! is parked between eye frames for the whole event (ADR-0081). While the window is hidden and
//! the headset streams, the loop runs the viewer's passes at headset rate itself (ADR-0097).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use rerun::external::eframe;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;
use winit::window::WindowId;

use crate::decoupled::RenderThreadPause;
use crate::hidden_window::HiddenWindow;

/// One eye frame takes a few milliseconds; past this the render thread is stuck elsewhere
/// (a QAROS call), and the resize goes ahead rather than freeze the window.
const RENDER_THREAD_PARK_LIMIT: Duration = Duration::from_millis(500);

/// The pass rate for a hidden window while the headset streams: the METALENSE 2's 90 Hz.
/// eframe sleeps 10 ms after each pass of a hidden window (`sleep_if_invisible_or_minimized`),
/// so the passes run somewhat slower.
const HIDDEN_WINDOW_PASS_INTERVAL: Duration = Duration::from_millis(11);

/// eframe's application with a resize parking the render thread first, and the passes of a
/// hidden window driven at headset rate while the headset streams.
struct ParksRenderThreadOnResize<'app> {
    eframe: eframe::EframeWinitApplication<'app>,
    render_thread: RenderThreadPause,
    hidden_window: HiddenWindow,
    /// The viewer's one window, from its first event.
    window_id: Option<WindowId>,
    next_hidden_window_pass: Instant,
}

impl ParksRenderThreadOnResize<'_> {
    /// eframe repaints a hidden window at most every 100 ms (`INVISIBLE_WINDOW_REPAINT_INTERVAL`
    /// in `run.rs`), and a covered macOS window gets no `RedrawRequested` at all; the scene
    /// and the panels would follow the recording at that rate. The pass is what a delivered
    /// `RedrawRequested` runs, and the app's `logic` has asked it to run `ui`.
    fn run_hidden_window_pass_when_due(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window_id) = self.window_id else {
            return;
        };
        if !self.hidden_window.wants_headset_frames() {
            return;
        }
        let now = Instant::now();
        if now >= self.next_hidden_window_pass {
            self.next_hidden_window_pass = now + HIDDEN_WINDOW_PASS_INTERVAL;
            self.eframe
                .window_event(event_loop, window_id, WindowEvent::RedrawRequested);
        }
        let wake_at = match event_loop.control_flow() {
            ControlFlow::Poll => return,
            ControlFlow::Wait => self.next_hidden_window_pass,
            ControlFlow::WaitUntil(eframe_wake_at) => {
                eframe_wake_at.min(self.next_hidden_window_pass)
            }
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(wake_at));
    }
}

impl ApplicationHandler<eframe::UserEvent> for ParksRenderThreadOnResize<'_> {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.eframe.new_events(event_loop, cause);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.resumed(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: eframe::UserEvent) {
        self.eframe.user_event(event_loop, event);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.window_id.get_or_insert(window_id);
        if !matches!(event, WindowEvent::Resized(_)) {
            self.eframe.window_event(event_loop, window_id, event);
            return;
        }
        let paused = self.render_thread.pause(RENDER_THREAD_PARK_LIMIT);
        if !paused.is_parked() {
            re_log::warn_once!(
                "headset: the render thread did not park within {RENDER_THREAD_PARK_LIMIT:?} \
                 for a window resize; the surface is reconfigured beside it"
            );
        }
        self.eframe.window_event(event_loop, window_id, event);
        drop(paused);
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: DeviceId,
        event: DeviceEvent,
    ) {
        self.eframe.device_event(event_loop, device_id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.about_to_wait(event_loop);
        self.run_hidden_window_pass_when_due(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.eframe.memory_warning(event_loop);
    }
}

/// `eframe::run_native` with the resize hook and the hidden window's passes: the same options,
/// app creator and on-demand event loop. eframe logs an error that ends its loop and keeps it to itself here, so an app
/// creator that failed or never ran is reported from this side.
pub fn run_native_parking_render_thread_on_resize(
    app_name: &str,
    native_options: eframe::NativeOptions,
    app_creator: eframe::AppCreator<'_>,
    render_thread: RenderThreadPause,
    hidden_window: HiddenWindow,
) -> Result<(), String> {
    let creation_outcome: Rc<RefCell<Option<Result<(), String>>>> = Rc::default();
    let recorded_outcome = Rc::clone(&creation_outcome);
    let recording_creator: eframe::AppCreator<'_> = Box::new(move |creation_context| {
        let created = app_creator(creation_context);
        *recorded_outcome.borrow_mut() =
            Some(created.as_ref().map(|_| ()).map_err(ToString::to_string));
        created
    });
    let mut event_loop = EventLoop::<eframe::UserEvent>::with_user_event()
        .build()
        .map_err(|error| format!("cannot create the window's event loop: {error}"))?;
    let eframe = eframe::create_native(app_name, native_options, recording_creator, &event_loop);
    let mut application = ParksRenderThreadOnResize {
        eframe,
        render_thread,
        hidden_window,
        window_id: None,
        next_hidden_window_pass: Instant::now(),
    };
    event_loop
        .run_app_on_demand(&mut application)
        .map_err(|error| format!("the window's event loop failed: {error}"))?;
    drop(application);
    creation_outcome.take().unwrap_or_else(|| {
        Err("the viewer window closed before the viewer started; see the log".to_owned())
    })
}
