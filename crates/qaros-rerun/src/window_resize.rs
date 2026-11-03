//! The window on an event loop of our own, so a resize reaches us before eframe: eframe
//! reconfigures the window's surface inside its `Resized` handler, and on Windows paints there
//! too, with nothing of the app's running first. With `--decoupled-render` the render thread
//! is parked between eye frames for the whole event (ADR-0081).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use rerun::external::eframe;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;
use winit::window::WindowId;

use crate::decoupled::RenderThreadPause;

/// One eye frame takes a few milliseconds; past this the render thread is stuck elsewhere
/// (a QAROS call), and the resize goes ahead rather than freeze the window.
const RENDER_THREAD_PARK_LIMIT: Duration = Duration::from_millis(500);

/// eframe's application with a resize parking the render thread first.
struct ParksRenderThreadOnResize<'app> {
    eframe: eframe::EframeWinitApplication<'app>,
    render_thread: RenderThreadPause,
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

/// `eframe::run_native` with the resize hook: the same options, app creator and on-demand
/// event loop. eframe logs an error that ends its loop and keeps it to itself here, so an app
/// creator that failed or never ran is reported from this side.
pub fn run_native_parking_render_thread_on_resize(
    app_name: &str,
    native_options: eframe::NativeOptions,
    app_creator: eframe::AppCreator<'_>,
    render_thread: RenderThreadPause,
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
    };
    event_loop
        .run_app_on_demand(&mut application)
        .map_err(|error| format!("the window's event loop failed: {error}"))?;
    drop(application);
    creation_outcome.take().unwrap_or_else(|| {
        Err("the viewer window closed before the viewer started; see the log".to_owned())
    })
}
