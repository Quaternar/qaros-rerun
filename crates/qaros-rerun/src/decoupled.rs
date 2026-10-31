//! `--decoupled-render` (experiment): the eyes are drawn on a render thread of their own, from
//! the last scene the UI thread finished, at whatever rate the sink and the GPU allow.
//!
//! The UI thread runs Rerun as usual. The 3D view observer only copies the streamed view's draw
//! data ([`SceneCapture`]); after the UI pass, once Rerun's `before_submit` has queued the
//! uploads that draw data depends on, the copy is published as a [`RetainedScene`]
//! ([`DecoupledHeadset::end_viewer_frame`]). The render thread owns the sink and a second
//! `re_renderer` context on the same device and queue; every eye frame it brackets a frame of
//! its own context, takes the newest scene, begins every user's next frame with fresh poses and
//! draws the eyes from per-eye copies of the retained draw data (`docs/decoupled-render.md`).
//!
//! The sink is built on the render thread and never leaves it, so thread-bound QAROS handles
//! stay on one thread; the GUI panel hooks, which use the same QAROS session, run there too.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use frame_sink::perf;
use re_renderer::{QueueableDrawData, RenderConfig, RenderContext};
use re_view_spatial::view_3d_observer::View3DFrame;
use re_viewer_context::ViewId;
use rerun::external::eframe;
use xr_math::AppPlacement;

use crate::draw_data_copy::copy_all;
use crate::headset::{
    BegunFrames, FrameCount, HeadsetRenderer, HeadsetSettings, HeadsetSink, HeadsetStatistics,
    SceneSource, claim_streaming_view, placement_for_view,
};
use crate::viewer_app::{FrameHook, HeadsetFrame};

/// While streaming with nothing to begin and nothing in flight, the render thread waits this
/// long for a new scene or a wake before it asks the sink again (QAROS frames are polled).
const STREAMING_IDLE_WAIT: Duration = Duration::from_millis(1);
/// The same while nobody is rendered.
const NOT_STREAMING_IDLE_WAIT: Duration = Duration::from_millis(20);
/// With frames in flight but none to begin, at most this long for the GPU, so readbacks map.
const IN_FLIGHT_GPU_WAIT: Duration = Duration::from_millis(8);
/// How often the render thread checks for a stop while it waits for the viewer's device.
const DEVICE_WAIT_STEP: Duration = Duration::from_millis(100);
/// A frame whose readback has not arrived this long after it began is abandoned: the default
/// path's 30 viewer frames at 60 per second, since the render thread runs far more eye frames.
const READBACK_TIME_LIMIT: Duration = Duration::from_millis(500);

/// One finished scene: the streamed 3D view's draw data as one viewer frame built it, kept
/// alive (and its GPU resources out of `re_renderer`'s pools) until no eye frame needs it.
pub struct RetainedScene {
    pub sequence: u64,
    pub view_seed: u64,
    pub placement: AppPlacement,
    pub draw_data: Vec<QueueableDrawData>,
    pub published_at: Instant,
}

impl RetainedScene {
    pub fn source(&self) -> SceneSource<'_> {
        SceneSource {
            view_seed: self.view_seed,
            placement: self.placement,
            draw_data: &self.draw_data,
        }
    }
}

#[derive(Default)]
struct HandoffState {
    scene: Option<Arc<RetainedScene>>,
    /// Counts publications, wakes and the stop, so a waiter sees any change since it looked.
    generation: u64,
    stop_requested: bool,
}

/// The newest scene, handed from the UI thread to the render thread. Neither side holds the
/// lock while it works: the render thread takes its own `Arc` once per eye frame.
#[derive(Default)]
pub struct SceneHandoff {
    state: Mutex<HandoffState>,
    changed: Condvar,
}

impl SceneHandoff {
    fn state(&self) -> MutexGuard<'_, HandoffState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the scene; `None` withdraws it (the view was not drawn).
    pub fn publish(&self, scene: Option<Arc<RetainedScene>>) {
        let replaced = {
            let mut state = self.state();
            state.generation += 1;
            std::mem::replace(&mut state.scene, scene)
        };
        self.changed.notify_all();
        drop(replaced);
    }

    pub fn latest(&self) -> (Option<Arc<RetainedScene>>, u64) {
        let state = self.state();
        (state.scene.clone(), state.generation)
    }

    /// Ends a render thread wait early, for example when QAROS has a frame ready.
    pub fn wake(&self) {
        self.state().generation += 1;
        self.changed.notify_all();
    }

    pub fn request_stop(&self) {
        {
            let mut state = self.state();
            state.stop_requested = true;
            state.generation += 1;
        }
        self.changed.notify_all();
    }

    pub fn is_stop_requested(&self) -> bool {
        self.state().stop_requested
    }

    /// Waits until something changed since `seen_generation`, or `timeout` passed.
    pub fn wait_for_change(&self, seen_generation: u64, timeout: Duration) {
        let state = self.state();
        let _waited = self
            .changed
            .wait_timeout_while(state, timeout, |state| state.generation == seen_generation)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// What the UI thread needs to know of the render thread every viewer frame, without waiting
/// for it.
#[derive(Default)]
pub struct RenderStatus {
    streaming: AtomicBool,
    finished: AtomicBool,
    frames_open: AtomicBool,
    /// The render thread panicked or got no render context: it tore the sink down, and the
    /// UI ends the run.
    failed: AtomicBool,
    statistics: Mutex<HeadsetStatistics>,
}

impl RenderStatus {
    pub fn is_streaming(&self) -> bool {
        !self.has_failed()
            && (self.streaming.load(Ordering::Acquire) || self.frames_open.load(Ordering::Acquire))
    }

    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    fn mark_failed(&self) {
        self.failed.store(true, Ordering::Release);
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire) && !self.frames_open.load(Ordering::Acquire)
    }

    pub fn statistics(&self) -> HeadsetStatistics {
        self.statistics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update(&self, renderer: &HeadsetRenderer, sink: &HeadsetSink) {
        self.streaming.store(sink.is_streaming(), Ordering::Release);
        self.finished.store(sink.is_finished(), Ordering::Release);
        self.frames_open
            .store(renderer.has_frames_open(), Ordering::Release);
        renderer.statistics().clone_into(
            &mut self
                .statistics
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// The 3D view observer's side on the UI thread: copies the streamed view's draw data every
/// viewer frame it is drawn.
pub struct SceneCapture {
    settings: HeadsetSettings,
    streaming_view: Option<(ViewId, u64)>,
    captured: Option<RetainedScene>,
    next_sequence: u64,
    status: Arc<RenderStatus>,
}

impl SceneCapture {
    pub fn on_view_3d_frame(&mut self, frame: &View3DFrame<'_, '_>) {
        let viewer_frame = frame.viewer_ctx.egui_ctx().cumulative_pass_nr();
        if !claim_streaming_view(&mut self.streaming_view, frame.query.view_id, viewer_frame) {
            return;
        }
        if self.status.is_streaming() {
            frame.viewer_ctx.egui_ctx().request_repaint();
        }
        let placement = match placement_for_view(&self.settings, frame) {
            Ok(placement) => placement,
            Err(error) => {
                re_log::warn_once!("headset: not streaming this view: {error}");
                return;
            }
        };
        let started = Instant::now();
        let draw_data = copy_all(frame.draw_data);
        perf::record("scene_capture", None, started.elapsed().as_micros());
        self.captured = Some(RetainedScene {
            sequence: self.next_sequence,
            view_seed: frame.query.view_id.hash(),
            placement,
            draw_data,
            published_at: started,
        });
        self.next_sequence += 1;
    }
}

/// The parts of the sink's setup that live on the render thread.
pub struct RenderThreadSink {
    pub sink: HeadsetSink,
    pub frame_hooks: Vec<Box<dyn FrameHook>>,
}

/// The viewer's device as Rerun's own context uses it, for the render thread's context.
struct SharedDevice {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    output_format_color: wgpu::TextureFormat,
    render_config: RenderConfig,
}

/// The UI thread's handle on the render thread. Dropping it stops the thread and waits for
/// it: the thread abandons open frames, drops the sink and then the panel hooks.
pub struct DecoupledHeadset {
    pub capture: Arc<Mutex<SceneCapture>>,
    handoff: Arc<SceneHandoff>,
    status: Arc<RenderStatus>,
    device_sender: Option<mpsc::Sender<SharedDevice>>,
    thread: Option<JoinHandle<()>>,
    scenes_published: FrameCount,
}

impl DecoupledHeadset {
    /// Starts the render thread, which runs `build` and keeps its [`RenderThreadSink`]; the
    /// rest of what `build` returns comes back here. `handoff` is created by the caller so
    /// QAROS callbacks inside `build` can wake the render thread.
    pub fn start<Reply: Send + 'static>(
        settings: HeadsetSettings,
        handoff: Arc<SceneHandoff>,
        build: impl FnOnce() -> Result<(RenderThreadSink, Reply), String> + Send + 'static,
    ) -> Result<(Self, Reply), String> {
        let status = Arc::new(RenderStatus::default());
        let (reply_sender, reply_receiver) = mpsc::channel();
        let (device_sender, device_receiver) = mpsc::channel();
        let thread = {
            let handoff = Arc::clone(&handoff);
            let status = Arc::clone(&status);
            std::thread::Builder::new()
                .name("headset render".to_owned())
                .spawn(move || {
                    let sink_side = match build() {
                        Ok((sink_side, reply)) => {
                            if reply_sender.send(Ok(reply)).is_err() {
                                return;
                            }
                            sink_side
                        }
                        Err(error) => {
                            let _ = reply_sender.send(Err(error));
                            return;
                        }
                    };
                    run_render_thread(settings, sink_side, &device_receiver, &handoff, &status);
                })
                .map_err(|error| format!("cannot start the headset render thread: {error}"))?
        };
        let reply = reply_receiver
            .recv()
            .map_err(|_| "the headset render thread ended during setup".to_owned())
            .and_then(|reply| reply);
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                let _ = thread.join();
                return Err(error);
            }
        };
        let capture = Arc::new(Mutex::new(SceneCapture {
            settings,
            streaming_view: None,
            captured: None,
            next_sequence: 0,
            status: Arc::clone(&status),
        }));
        Ok((
            Self {
                capture,
                handoff,
                status,
                device_sender: Some(device_sender),
                thread: Some(thread),
                scenes_published: FrameCount::default(),
            },
            reply,
        ))
    }

    /// Hands the viewer's device to the render thread, once, when the frame has one and Rerun
    /// created its context on it.
    pub fn share_device(&mut self, frame: &eframe::Frame) {
        if self.device_sender.is_none() {
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let renderer = render_state.renderer.read();
        let Some(rerun_context) = renderer.callback_resources.get::<RenderContext>() else {
            return;
        };
        let shared = SharedDevice {
            adapter: render_state.adapter.clone(),
            device: render_state.device.clone(),
            queue: render_state.queue.clone(),
            output_format_color: rerun_context.output_format_color(),
            render_config: *rerun_context.render_config(),
        };
        drop(renderer);
        self.send_device(shared);
    }

    fn send_device(&mut self, shared: SharedDevice) {
        if let Some(sender) = self.device_sender.take()
            && sender.send(shared).is_err()
        {
            re_log::error!("headset: the render thread ended before it got the device");
        }
    }

    /// After the UI pass, when Rerun's `before_submit` has queued this viewer frame's uploads:
    /// publishes the scene captured in the pass, or withdraws the last one if the streamed
    /// view was not drawn.
    pub fn end_viewer_frame(&mut self) {
        let captured = self
            .capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .captured
            .take();
        if captured.is_some() {
            self.scenes_published.record();
        }
        self.handoff.publish(captured.map(|mut scene| {
            scene.published_at = Instant::now();
            Arc::new(scene)
        }));
    }

    pub fn is_streaming(&self) -> bool {
        self.status.is_streaming()
    }

    pub fn is_finished(&self) -> bool {
        self.status.is_finished()
    }

    /// The render thread panicked or could not start rendering; the sink is gone.
    pub fn has_failed(&self) -> bool {
        self.status.has_failed()
    }

    pub fn summary(&self) -> String {
        format!(
            "decoupled; scenes published {} ({:.1} per second); {}",
            self.scenes_published.count,
            self.scenes_published.per_second(),
            self.status.statistics().summary()
        )
    }
}

impl Drop for DecoupledHeadset {
    fn drop(&mut self) {
        self.handoff.request_stop();
        self.device_sender = None;
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            re_log::error!("headset: the render thread panicked");
        }
    }
}

#[cfg(test)]
impl DecoupledHeadset {
    /// A handle whose render thread failed, with no thread behind it.
    pub fn failed_without_a_thread() -> Self {
        let status = Arc::new(RenderStatus::default());
        status.mark_failed();
        Self {
            capture: Arc::new(Mutex::new(SceneCapture {
                settings: HeadsetSettings::default(),
                streaming_view: None,
                captured: None,
                next_sequence: 0,
                status: Arc::clone(&status),
            })),
            handoff: Arc::default(),
            status,
            device_sender: None,
            thread: None,
            scenes_published: FrameCount::default(),
        }
    }
}

/// Raw Vulkan submissions of a GPU sink (acquire and release batches, semaphores staged on
/// wgpu's queue, waits for an idle queue) must not interleave with another thread's submits.
/// Sinks that submit through wgpu alone need no lock: wgpu synchronizes its own submits.
fn sink_submission_lock(sink: &HeadsetSink) -> Option<MutexGuard<'static, ()>> {
    let submits_outside_wgpu = matches!(sink, HeadsetSink::Gpu(gpu) if gpu.submits_outside_wgpu());
    submits_outside_wgpu.then(re_renderer::queue_submission_lock)
}

/// Marks the render thread failed when it unwinds past this point, whatever panicked.
struct FailedOnUnwind<'status>(&'status RenderStatus);

impl Drop for FailedOnUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.mark_failed();
        }
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

fn wait_for_device(
    device_receiver: &mpsc::Receiver<SharedDevice>,
    handoff: &SceneHandoff,
) -> Option<SharedDevice> {
    loop {
        match device_receiver.recv_timeout(DEVICE_WAIT_STEP) {
            Ok(shared) => return Some(shared),
            Err(mpsc::RecvTimeoutError::Timeout) if !handoff.is_stop_requested() => {}
            Err(_) => return None,
        }
    }
}

/// A panic anywhere after the sink was built (the renderer's setup, the wait for the device,
/// the render context, the eye frames) is caught: the sink is still torn down here, with its
/// open frames abandoned first and under the submission lock when the sink submits outside
/// wgpu, and the UI sees the failure and ends the run. A panic in the teardown itself is caught
/// too; the sink then drops while the lock is still held. The render context goes after the
/// teardown (ADR-0060).
fn run_render_thread(
    settings: HeadsetSettings,
    mut sink_side: RenderThreadSink,
    device_receiver: &mpsc::Receiver<SharedDevice>,
    handoff: &SceneHandoff,
    status: &RenderStatus,
) {
    let _failed_on_unwind = FailedOnUnwind(status);
    let mut renderer = None;
    let mut render_context = None;
    let ran = catch_unwind(AssertUnwindSafe(|| {
        render_until_stopped(
            settings,
            &mut sink_side,
            device_receiver,
            handoff,
            status,
            &mut renderer,
            &mut render_context,
        );
    }));
    if let Err(panic) = ran {
        re_log::error!(
            "headset: the render thread panicked ({}); tearing down the sink and ending the run",
            panic_message(panic.as_ref())
        );
        status.mark_failed();
    }
    let torn_down = catch_unwind(AssertUnwindSafe(|| {
        tear_down(renderer, sink_side, status);
    }));
    if let Err(panic) = torn_down {
        re_log::error!(
            "headset: the render thread panicked in its teardown ({}); ending the run",
            panic_message(panic.as_ref())
        );
        status.mark_failed();
    }
    drop(render_context);
}

/// Sets up the renderer and the render context, kept in the caller's slots so the caller tears
/// down what was built whatever panics, then runs eye frames until a stop is requested.
fn render_until_stopped(
    settings: HeadsetSettings,
    sink_side: &mut RenderThreadSink,
    device_receiver: &mpsc::Receiver<SharedDevice>,
    handoff: &SceneHandoff,
    status: &RenderStatus,
    renderer_slot: &mut Option<HeadsetRenderer>,
    render_context_slot: &mut Option<RenderContext>,
) {
    let renderer = renderer_slot.insert(
        HeadsetRenderer::new(settings, &sink_side.sink)
            .with_readback_time_limit(READBACK_TIME_LIMIT),
    );
    let Some(shared) = wait_for_device(device_receiver, handoff) else {
        return;
    };
    let created = RenderContext::new_sharing_device(
        &shared.adapter,
        shared.device,
        shared.queue,
        shared.output_format_color,
        |_| shared.render_config,
    );
    let render_context = match created {
        Ok(render_context) => render_context_slot.insert(render_context),
        Err(error) => {
            re_log::error!(
                "headset: no render context for the render thread: {error}; ending the run"
            );
            status.mark_failed();
            return;
        }
    };
    re_log::info!("headset: rendering on a thread of its own (--decoupled-render)");
    let mut eye_frame = 0;
    while !handoff.is_stop_requested() {
        eye_frame += 1;
        run_eye_frame(
            render_context,
            renderer,
            sink_side,
            handoff,
            status,
            eye_frame,
        );
    }
}

/// One eye frame on the render thread, bracketing a frame of its own `re_renderer` context.
/// Only the sink's calls hold the submission lock; building and drawing the eye views does not
/// (`draw_eyes` locks just its queue submit), so the UI thread's submits are not held up by
/// work that grows with the scene.
fn run_eye_frame(
    render_context: &mut RenderContext,
    renderer: &mut HeadsetRenderer,
    sink_side: &mut RenderThreadSink,
    handoff: &SceneHandoff,
    status: &RenderStatus,
    eye_frame: u64,
) {
    let _eye_frame_span = perf::Span::start("eye_frame");
    let (scene, generation) = handoff.latest();
    render_context.begin_frame();
    let sink = &mut sink_side.sink;
    renderer.begin_viewer_frame(sink);
    let begun_frames = {
        let _submission_lock = sink_submission_lock(sink);
        if scene.is_some() {
            Some(renderer.begin_frames_for_scene(render_context, eye_frame, sink))
        } else {
            renderer.show_finished_frames(render_context, eye_frame, sink);
            None
        }
    };
    let begun = begun_frames.as_ref().map_or(0, BegunFrames::len);
    if let (Some(begun_frames), Some(scene)) = (begun_frames, &scene) {
        renderer.prepare_frames_from_scene(
            render_context,
            &scene.source(),
            begun_frames,
            eye_frame,
        );
    }
    if begun > 0
        && let Some(scene) = &scene
    {
        perf::record(
            "scene_age_at_eye_frame",
            None,
            scene.published_at.elapsed().as_micros(),
        );
        perf::record(
            "scene_sequence_at_eye_frame",
            None,
            u128::from(scene.sequence),
        );
    }
    render_context.before_submit();
    {
        let _submission_lock = sink_submission_lock(sink);
        renderer.settle_frames_before_drawing(sink);
    }
    let failed_frames = renderer.draw_eyes(render_context);
    {
        let _submission_lock = sink_submission_lock(sink);
        renderer.show_and_release_after_drawing(render_context, eye_frame, sink, failed_frames);
    }
    if begun > 0 {
        // Counted after this frame's shows, so a frame shown in the eye frame that drew it was
        // in flight for none, as on the viewer frame path.
        renderer.record_eye_frame();
    }
    drop(scene);
    let streaming = sink.is_streaming();
    let headset_frame = HeadsetFrame {
        is_streaming: streaming,
        heads: renderer.recent_heads(Instant::now()),
    };
    for hook in &mut sink_side.frame_hooks {
        hook.on_viewer_frame(&headset_frame);
    }
    status.update(renderer, &sink_side.sink);
    if begun == 0 {
        wait_for_work(render_context, renderer, handoff, generation, streaming);
    }
}

/// Nothing began: wait for the GPU while frames are in flight (their readbacks map once it is
/// done), else for a new scene, a wake or the idle timeout.
fn wait_for_work(
    render_context: &RenderContext,
    renderer: &HeadsetRenderer,
    handoff: &SceneHandoff,
    generation: u64,
    streaming: bool,
) {
    let _wait = perf::Span::start("eye_frame_wait");
    if renderer.has_frames_open() {
        if let Err(error) = render_context.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(IN_FLIGHT_GPU_WAIT),
        }) && !matches!(error, wgpu::PollError::Timeout)
        {
            re_log::warn_once!("headset: waiting for the GPU on the render thread: {error}");
        }
        return;
    }
    let idle_wait = if streaming {
        STREAMING_IDLE_WAIT
    } else {
        NOT_STREAMING_IDLE_WAIT
    };
    handoff.wait_for_change(generation, idle_wait);
}

/// Open frames go first, while the sink is alive; then the sink with its senders, then the
/// panel hooks, as `HeadsetViewerApp::leave_qaros` orders them on the default path. The lock is
/// taken before the sink is unpacked, so a sink that drops while this unwinds still drops
/// under it.
fn tear_down(
    renderer: Option<HeadsetRenderer>,
    sink_side: RenderThreadSink,
    status: &RenderStatus,
) {
    let submission_lock = sink_submission_lock(&sink_side.sink);
    let frame_hooks = sink_side.frame_hooks;
    let mut sink = sink_side.sink;
    if let Some(mut renderer) = renderer {
        renderer.abandon_all(&mut sink);
        status.update(&renderer, &sink);
    }
    drop(sink);
    drop(submission_lock);
    drop(frame_hooks);
    re_log::info!("headset: render thread stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_scene(sequence: u64) -> Arc<RetainedScene> {
        Arc::new(RetainedScene {
            sequence,
            view_seed: 7,
            placement: AppPlacement::default(),
            draw_data: Vec::new(),
            published_at: Instant::now(),
        })
    }

    #[test]
    fn the_newest_scene_replaces_the_last_and_a_withdrawal_clears_it() {
        let handoff = SceneHandoff::default();
        assert!(handoff.latest().0.is_none());
        handoff.publish(Some(empty_scene(1)));
        handoff.publish(Some(empty_scene(2)));
        let (latest, generation) = handoff.latest();
        assert_eq!(latest.map(|scene| scene.sequence), Some(2));
        assert_eq!(generation, 2);
        handoff.publish(None);
        assert!(handoff.latest().0.is_none());
    }

    #[test]
    fn a_scene_in_use_by_the_render_thread_outlives_its_replacement() {
        let handoff = SceneHandoff::default();
        let first = empty_scene(1);
        let watch = Arc::downgrade(&first);
        handoff.publish(Some(first));
        let (in_use, _) = handoff.latest();
        handoff.publish(Some(empty_scene(2)));
        assert!(watch.upgrade().is_some(), "still drawn by the eye frame");
        drop(in_use);
        assert!(
            watch.upgrade().is_none(),
            "dropped once the eye frame is done"
        );
    }

    #[test]
    fn a_publication_from_another_thread_ends_the_wait() {
        let handoff = Arc::new(SceneHandoff::default());
        let (_, generation) = handoff.latest();
        let publisher = {
            let handoff = Arc::clone(&handoff);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                handoff.publish(Some(empty_scene(1)));
            })
        };
        let started = Instant::now();
        handoff.wait_for_change(generation, Duration::from_secs(5));
        let waited = started.elapsed();
        publisher.join().expect("publisher");
        assert!(
            (Duration::from_millis(20)..Duration::from_secs(2)).contains(&waited),
            "waited {waited:?}"
        );
        assert_eq!(handoff.latest().0.map(|scene| scene.sequence), Some(1));
    }

    mod on_the_gpu {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicBool, Ordering};

        use re_renderer::renderer::{VolumeDrawData, VolumeOptions};
        use re_renderer::resource_managers::{Texture3DDataDesc, create_and_upload_texture_3d};
        use re_renderer::view_builder::TargetConfiguration;
        use re_renderer::{
            Color32, Colormap, GaussianSplatBuilder, OutlineMaskPreference, PickingLayerId,
            PickingLayerInstanceId, PointCloudBuilder, Rgba, Rgba32Unmul, Size, SortOrderCache,
            ViewBuilder, ViewBuilderId,
        };

        use super::*;
        use crate::draw_data_copy::{copy_all, copy_draw_data};

        const RESOLUTION: u32 = 64;

        /// The colors and volume values of one scene; the retained scene and the scenes drawn
        /// after it differ only in these.
        struct SceneLook {
            opaque_point: Color32,
            transparent_points: Color32,
            splats: Rgba32Unmul,
            volume_peak: f32,
        }

        const RETAINED_LOOK: SceneLook = SceneLook {
            opaque_point: Color32::from_rgb(230, 20, 20),
            transparent_points: Color32::from_rgba_unmultiplied_const(20, 230, 20, 128),
            splats: Rgba32Unmul([230, 230, 20, 160]),
            volume_peak: 1.0,
        };
        const LATER_LOOK: SceneLook = SceneLook {
            opaque_point: Color32::from_rgb(20, 20, 230),
            transparent_points: Color32::from_rgba_unmultiplied_const(230, 20, 230, 128),
            splats: Rgba32Unmul([20, 230, 230, 160]),
            volume_peak: 0.4,
        };

        struct TestDevice {
            adapter: wgpu::Adapter,
            device: wgpu::Device,
            queue: wgpu::Queue,
        }

        fn test_device() -> Option<TestDevice> {
            let instance = wgpu::Instance::default();
            let Ok(adapter) = pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions::default()),
            ) else {
                eprintln!("skipped: no wgpu adapter");
                return None;
            };
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("retained draw data test"),
                    required_limits: adapter.limits(),
                    ..Default::default()
                }))
                .expect("a device on the adapter");
            Some(TestDevice {
                adapter,
                device,
                queue,
            })
        }

        /// One big opaque point at the world origin, 5 m in front of the camera.
        fn opaque_point(render_context: &RenderContext, look: &SceneLook) -> QueueableDrawData {
            let mut builder = PointCloudBuilder::new(render_context);
            builder.reserve(1).expect("room for one point");
            builder.batch("retained draw data test").add_points_slow(
                &[glam::Vec3::ZERO],
                &[Size::new_scene_units(1.0)],
                &[look.opaque_point],
                &[PickingLayerInstanceId::default()],
            );
            builder.into_draw_data().expect("point cloud").into()
        }

        /// Three overlapping alpha-blended points on the left, one behind the other, sorted back
        /// to front: the transparent path whose per-frame drawables a copy must not share.
        fn transparent_points(
            render_context: &RenderContext,
            look: &SceneLook,
        ) -> QueueableDrawData {
            let positions = [-0.6, -0.3, 0.0].map(|depth| glam::Vec3::new(-1.6, 0.0, depth));
            let mut builder = PointCloudBuilder::new(render_context);
            builder
                .reserve(positions.len())
                .expect("room for the points");
            builder
                .batch("retained draw data test, transparent")
                .enable_alpha_blending(true)
                .sort_order(SortOrderCache::default())
                .add_points_slow(
                    &positions,
                    &[Size::new_scene_units(0.5); 3],
                    &[look.transparent_points; 3],
                    &[PickingLayerInstanceId::default(); 3],
                );
            builder.into_draw_data().expect("point cloud").into()
        }

        /// Three overlapping gaussian splats on the right, sorted back to front.
        fn splats(render_context: &RenderContext, look: &SceneLook) -> QueueableDrawData {
            let centers = [-0.6, -0.3, 0.0].map(|depth| glam::Vec3::new(1.6, 0.0, depth));
            let mut builder = GaussianSplatBuilder::new(render_context);
            builder.reserve(centers.len()).expect("room for the splats");
            builder
                .batch("retained draw data test, splats")
                .sort_order(SortOrderCache::default())
                .add_gaussians(
                    &centers,
                    &[glam::Vec3::splat(0.3); 3],
                    &[glam::Quat::IDENTITY; 3],
                    &[look.splats; 3],
                    &[],
                    0,
                    &[PickingLayerInstanceId::default(); 3],
                );
            builder.into_draw_data().expect("gaussian splats").into()
        }

        /// A raymarched 4x4x4 volume in a unit cube above the opaque point.
        fn volume(render_context: &RenderContext, look: &SceneLook) -> QueueableDrawData {
            let values: Vec<f32> = (0..64u8)
                .map(|voxel| f32::from(voxel % 5) / 4.0 * look.volume_peak)
                .collect();
            let texture = create_and_upload_texture_3d(
                render_context,
                &Texture3DDataDesc::f32_as_f16(
                    "retained draw data test, volume".into(),
                    &values,
                    [4, 4, 4],
                ),
            )
            .expect("the volume texture");
            VolumeDrawData::new(
                render_context,
                &texture,
                VolumeOptions {
                    world_from_volume: glam::Affine3A::from_translation(glam::Vec3::new(
                        -0.5, 1.0, -0.5,
                    )),
                    value_range: [0.0, 1.0],
                    colormap: Colormap::Viridis,
                    gamma: 1.0,
                    optical_density: 4.0,
                    picking_layer_id: PickingLayerId::default(),
                    outline_mask_ids: OutlineMaskPreference::NONE,
                },
            )
            .expect("the volume")
            .into()
        }

        type SceneBuilder = fn(&RenderContext, &SceneLook) -> QueueableDrawData;

        const SCENE_PARTS: [(&str, SceneBuilder); 4] = [
            ("opaque point", opaque_point),
            ("transparent points", transparent_points),
            ("splats", splats),
            ("volume", volume),
        ];

        fn scene(render_context: &RenderContext, look: &SceneLook) -> Vec<QueueableDrawData> {
            SCENE_PARTS
                .iter()
                .map(|(_, build_part)| build_part(render_context, look))
                .collect()
        }

        struct SetOnDrop(Arc<AtomicBool>);

        impl Drop for SetOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        fn covered_pixel_count(image: &[u8]) -> usize {
            image.chunks_exact(4).filter(|pixel| pixel[3] > 0).count()
        }

        /// Draws `draw_data` in a frame of `render_context` that its caller began, and reads the
        /// composited view back, row by row.
        fn render_image(
            render_context: &mut RenderContext,
            draw_data: Vec<QueueableDrawData>,
        ) -> Vec<u8> {
            let config = TargetConfiguration {
                name: "retained draw data test".into(),
                resolution_in_pixel: [RESOLUTION; 2],
                view_from_world: re_math::IsoTransform::look_at_rh(
                    glam::Vec3::new(0.0, 0.0, 5.0),
                    glam::Vec3::ZERO,
                    glam::Vec3::Y,
                )
                .expect("a camera"),
                ..TargetConfiguration::default()
            };
            let mut view_builder =
                ViewBuilder::new(render_context, config, ViewBuilderId::new(11)).expect("a view");
            for draw_data in draw_data {
                view_builder
                    .queue_draw(render_context, draw_data)
                    .expect("queued");
            }
            let device = render_context.device.clone();
            let target = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("retained draw data test"),
                size: wgpu::Extent3d {
                    width: RESOLUTION,
                    height: RESOLUTION,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: render_context.output_format_color(),
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let drawn = view_builder
                .draw(render_context, Rgba::TRANSPARENT)
                .expect("drawn");
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("retained draw data test"),
            });
            crate::eye_render::composite(render_context, &view_builder, &mut encoder, &target);
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("retained draw data test"),
                size: u64::from(RESOLUTION) * u64::from(RESOLUTION) * 4,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                target.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(RESOLUTION * 4),
                        rows_per_image: Some(RESOLUTION),
                    },
                },
                target.size(),
            );
            render_context.before_submit();
            render_context.queue.submit([drawn, encoder.finish()]);
            readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("waiting for the readback");
            readback
                .slice(..)
                .get_mapped_range()
                .expect("mapping the readback")
                .to_vec()
        }

        fn center_pixel(image: &[u8]) -> [u8; 4] {
            let center = ((RESOLUTION / 2 * RESOLUTION + RESOLUTION / 2) * 4) as usize;
            [
                image[center],
                image[center + 1],
                image[center + 2],
                image[center + 3],
            ]
        }

        fn new_context(gpu: &TestDevice) -> RenderContext {
            RenderContext::new(
                &gpu.adapter,
                gpu.device.clone(),
                gpu.queue.clone(),
                wgpu::TextureFormat::Rgba8UnormSrgb,
                |_| re_renderer::RenderConfig::testing(),
            )
            .expect("a render context")
        }

        #[test]
        fn a_volume_is_copied() {
            let Some(gpu) = test_device() else {
                return;
            };
            let render_context = new_context(&gpu);
            let original = volume(&render_context, &RETAINED_LOOK);
            let copy = copy_draw_data(&original).expect("a copy of the volume");
            assert_eq!(copy.renderer_name(), original.renderer_name());
        }

        /// The cut `--decoupled-render` rests on: draw data built in one context, kept while that
        /// context runs on and recycles its pools, draws the same image from a second context on
        /// the same device, on another thread. Meanwhile the first context, on a third thread,
        /// draws copies of the same kept data and a new scene in turns, as the desktop view and
        /// the eyes do. The scene holds an opaque point, sorted transparent points, sorted splats
        /// and a volume.
        #[test]
        fn retained_draw_data_draws_unchanged_from_a_second_context_on_another_thread() {
            let Some(gpu) = test_device() else {
                return;
            };
            let mut first_context = new_context(&gpu);

            for (part_name, build_part) in SCENE_PARTS {
                first_context.begin_frame();
                let part = build_part(&first_context, &RETAINED_LOOK);
                let image = render_image(&mut first_context, vec![part]);
                assert!(
                    covered_pixel_count(&image) > 0,
                    "the {part_name} cover part of the image"
                );
            }

            first_context.begin_frame();
            let retained = scene(&first_context, &RETAINED_LOOK);
            let kept = Arc::new(copy_all(&retained));
            assert_eq!(kept.len(), retained.len(), "every part is copied");
            let reference = render_image(&mut first_context, retained);
            let reference_center = center_pixel(&reference);
            assert!(
                reference_center[0] > 150 && reference_center[2] < 80,
                "the retained opaque point is red at the center: {reference_center:?}"
            );

            // Many frames past the pools' reuse grace, each building and drawing a scene of the
            // same shape, which takes any buffer or texture the pools freed.
            let mut later = Vec::new();
            for _ in 0..12 {
                first_context.begin_frame();
                let replacement = scene(&first_context, &LATER_LOOK);
                later = render_image(&mut first_context, replacement);
            }
            let later_center = center_pixel(&later);
            assert!(
                later_center[2] > 150 && later_center[0] < 80,
                "later frames draw a blue opaque point: {later_center:?}"
            );

            let TestDevice {
                adapter,
                device,
                queue,
            } = gpu;
            let both_started = Arc::new(Barrier::new(2));
            let second_is_done = Arc::new(AtomicBool::new(false));
            let first_thread = {
                let both_started = Arc::clone(&both_started);
                let second_is_done = Arc::clone(&second_is_done);
                let kept = Arc::clone(&kept);
                std::thread::spawn(move || {
                    both_started.wait();
                    let mut images = Vec::new();
                    while images.len() < 12 || !second_is_done.load(Ordering::Acquire) {
                        first_context.begin_frame();
                        images.push(render_image(&mut first_context, copy_all(&kept)));
                        first_context.begin_frame();
                        let replacement = scene(&first_context, &LATER_LOOK);
                        render_image(&mut first_context, replacement);
                    }
                    (first_context, images)
                })
            };
            let second_thread = std::thread::spawn(move || {
                let mut second_context = RenderContext::new_sharing_device(
                    &adapter,
                    device,
                    queue,
                    wgpu::TextureFormat::Rgba8UnormSrgb,
                    |_| re_renderer::RenderConfig::testing(),
                )
                .expect("the second context");
                both_started.wait();
                // Set on a panic too, so the first thread never draws on alone.
                let _done_on_exit = SetOnDrop(second_is_done);
                let mut images = Vec::new();
                for _ in 0..24 {
                    second_context.begin_frame();
                    images.push(render_image(&mut second_context, copy_all(&kept)));
                }
                images
            });
            let from_second_context = second_thread.join().expect("the second render thread");
            let (first_context, from_first_context) =
                first_thread.join().expect("the first render thread");

            for (context_name, image) in from_second_context
                .iter()
                .map(|image| ("second", image))
                .chain(from_first_context.iter().map(|image| ("first", image)))
            {
                assert!(
                    *image == reference,
                    "the retained scene drawn by the {context_name} context differs from the \
                     reference in {} bytes; center {:?} against {reference_center:?}",
                    image
                        .iter()
                        .zip(&reference)
                        .filter(|(drawn, expected)| drawn != expected)
                        .count(),
                    center_pixel(image)
                );
            }
            drop(first_context);
        }

        fn shared_device(gpu: &TestDevice) -> SharedDevice {
            SharedDevice {
                adapter: gpu.adapter.clone(),
                device: gpu.device.clone(),
                queue: gpu.queue.clone(),
                output_format_color: wgpu::TextureFormat::Rgba8UnormSrgb,
                render_config: re_renderer::RenderConfig::testing(),
            }
        }

        type DroppedOn = Arc<Mutex<Option<String>>>;

        fn record_thread(dropped_on: &DroppedOn) {
            *dropped_on.lock().unwrap_or_else(PoisonError::into_inner) =
                std::thread::current().name().map(str::to_owned);
        }

        fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
            let started = Instant::now();
            while !done() {
                assert!(started.elapsed() < Duration::from_secs(10), "{what}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        /// Asks for frames by panicking, and records the thread it is dropped on.
        struct PanickingSink {
            dropped_on: DroppedOn,
        }

        impl frame_sink::FrameSink for PanickingSink {
            fn begin_frames(
                &mut self,
            ) -> Result<Vec<frame_sink::FrameRequest>, frame_sink::SinkError> {
                panic!("a sink failure injected on the render thread");
            }

            fn submit_view(
                &mut self,
                key: frame_sink::FrameKey,
                _view_index: usize,
                _image: frame_sink::EyeImage,
            ) -> Result<(), frame_sink::SinkError> {
                Err(frame_sink::SinkError::UnknownFrame(key))
            }

            fn show_frame(
                &mut self,
                key: frame_sink::FrameKey,
                _show: frame_sink::ShowInfo,
            ) -> Result<(), frame_sink::SinkError> {
                Err(frame_sink::SinkError::UnknownFrame(key))
            }

            fn abandon_frame(&mut self, _key: frame_sink::FrameKey) {}

            fn is_streaming(&self) -> bool {
                true
            }

            fn is_finished(&self) -> bool {
                false
            }

            fn depth_range(&self) -> frame_sink::DepthRange {
                frame_sink::DepthRange::new(0.05, 20.0, xr_math::DepthEncoding::ForwardNormalized)
                    .expect("a depth range")
            }
        }

        impl Drop for PanickingSink {
            fn drop(&mut self) {
                record_thread(&self.dropped_on);
            }
        }

        /// A panic in an eye frame is caught on the render thread: the sink is still torn down
        /// there, the UI sees the failure (and stops streaming), and the join is clean.
        #[test]
        fn a_render_thread_panic_tears_the_sink_down_there_and_shows_as_a_failure() {
            let Some(gpu) = test_device() else {
                return;
            };
            let dropped_on = DroppedOn::default();
            let handoff = Arc::new(SceneHandoff::default());
            let sink_dropped_on = Arc::clone(&dropped_on);
            let (mut decoupled, ()) = DecoupledHeadset::start(
                HeadsetSettings::default(),
                Arc::clone(&handoff),
                move || {
                    let sink = PanickingSink {
                        dropped_on: sink_dropped_on,
                    };
                    Ok((
                        RenderThreadSink {
                            sink: HeadsetSink::Cpu(Box::new(sink)),
                            frame_hooks: Vec::new(),
                        },
                        (),
                    ))
                },
            )
            .expect("the render thread starts");
            decoupled.send_device(shared_device(&gpu));
            handoff.publish(Some(empty_scene(1)));
            wait_until("the render thread failure is seen", || {
                decoupled.has_failed()
            });
            assert!(!decoupled.is_streaming());
            wait_until("the sink is torn down", || {
                dropped_on
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some()
            });
            assert_eq!(
                dropped_on
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_deref(),
                Some("headset render")
            );
            drop(decoupled);
        }

        #[cfg(feature = "qaros")]
        mod against_the_stub {
            use std::rc::Rc;

            use qaros_sink::stub_control::StubControl;
            use qaros_sink::{
                ConnectOptions, PoseSpace, QarosConnection, QarosSink, QarosSinkOptions,
                TeardownStep, connect, current_teardown_step,
            };

            use super::*;
            use crate::viewer_app::{FrameHook, HeadsetFrame};

            /// The stub's state is process-wide, so its tests run one at a time.
            static STUB_SERIAL: Mutex<()> = Mutex::new(());

            /// Holds the last reference to the connection, as the GUI panel hooks do, and
            /// records the thread it goes on and the teardown step reached once it went.
            struct ConnectionHook {
                connection: Option<Rc<QarosConnection>>,
                dropped_on: DroppedOn,
                step_after: Arc<Mutex<Option<TeardownStep>>>,
            }

            impl FrameHook for ConnectionHook {
                fn on_viewer_frame(&mut self, _headset: &HeadsetFrame) {}
            }

            impl Drop for ConnectionHook {
                fn drop(&mut self) {
                    drop(self.connection.take());
                    *self
                        .step_after
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Some(current_teardown_step());
                    record_thread(&self.dropped_on);
                }
            }

            fn connect_options(scratch: &std::path::Path) -> ConnectOptions {
                ConnectOptions {
                    library_path: Some(StubControl::library_path().expect("the stub library")),
                    runtime_binaries_directory: None,
                    storage_directory: Some(scratch.join("storage")),
                    onboarding_id_file: scratch.join("onboarding-id.txt"),
                    unanswered_peers_file: Some(scratch.join("unanswered-peers.txt")),
                    pairing_code: Some("ABCD-EFGH".to_owned()),
                    launch_line: None,
                    local_app: false,
                    peer_display_name: "decoupled render test".to_owned(),
                    volume_name: "rerun".to_owned(),
                    volume_display_name: "decoupled render test".to_owned(),
                    volume_size: [2.0, 2.0, 1.5],
                    placement: AppPlacement::default(),
                    console_logging: false,
                    raise_gpu_priority: false,
                    adapter: None,
                }
            }

            /// `--decoupled-render` with the QAROS sink against the stub: the sink is built on
            /// the render thread, streams from a published scene, and at Stop the senders, the
            /// app volume and the session go on that thread in that order, leaving no handle.
            #[test]
            fn a_stop_tears_the_qaros_sink_and_session_down_on_the_render_thread_in_order() {
                let Some(gpu) = test_device() else {
                    return;
                };
                let _serial = STUB_SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
                let stub = StubControl::open().expect("the stub");
                let scratch = std::env::temp_dir()
                    .join(format!("qaros-rerun-decoupled-test-{}", std::process::id()));
                std::fs::create_dir_all(&scratch).expect("a scratch directory");
                let dropped_on = DroppedOn::default();
                let step_after = Arc::new(Mutex::new(None));
                let handoff = Arc::new(SceneHandoff::default());
                let (hook_dropped_on, hook_step_after, thread_scratch) = (
                    Arc::clone(&dropped_on),
                    Arc::clone(&step_after),
                    scratch.clone(),
                );
                let (mut decoupled, ()) = DecoupledHeadset::start(
                    HeadsetSettings::default(),
                    Arc::clone(&handoff),
                    move || {
                        let connection =
                            Rc::new(connect(&connect_options(&thread_scratch), Arc::new(|| {}))?);
                        let sink = QarosSink::new(
                            Rc::clone(&connection),
                            QarosSinkOptions {
                                depth_range: frame_sink::DepthRange::new(
                                    0.05,
                                    20.0,
                                    xr_math::DepthEncoding::ForwardNormalized,
                                )
                                .ok_or("no depth range")?,
                                pose_space: PoseSpace::Room,
                                color_format: qaros_sink::qaros::PixelFormat::B8G8R8A8,
                                max_users: 1,
                                stop_after_frames: None,
                            },
                        );
                        let hook = ConnectionHook {
                            connection: Some(connection),
                            dropped_on: hook_dropped_on,
                            step_after: hook_step_after,
                        };
                        Ok((
                            RenderThreadSink {
                                sink: HeadsetSink::Cpu(Box::new(sink)),
                                frame_hooks: vec![Box::new(hook)],
                            },
                            (),
                        ))
                    },
                )
                .expect("the render thread starts");
                decoupled.send_device(shared_device(&gpu));
                let mut sequence = 0;
                wait_until("a frame is shown to the stub visualizer", || {
                    sequence += 1;
                    handoff.publish(Some(empty_scene(sequence)));
                    decoupled.status.statistics().frames_shown > 0
                });
                assert!(decoupled.is_streaming());
                drop(decoupled);

                assert_eq!(
                    dropped_on
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .as_deref(),
                    Some("headset render"),
                    "the connection went on the render thread"
                );
                let step_after = step_after
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                    .expect("the hook was dropped");
                assert!(
                    step_after as u8 >= TeardownStep::SessionDestroy as u8,
                    "the session was destroyed with the connection: {step_after:?}"
                );
                let calls = stub.calls().expect("the call log");
                let errors: Vec<&String> = calls
                    .iter()
                    .filter(|call| call.starts_with("ERROR"))
                    .collect();
                assert!(errors.is_empty(), "{errors:?}");
                let last_of = |name: &str| {
                    calls
                        .iter()
                        .rposition(|call| call == name)
                        .unwrap_or_else(|| panic!("no {name} in the call log"))
                };
                let sender_destroyed = last_of("render_stream_handle_destroy");
                let volume_closed = last_of("app_volumes_close_volume");
                let session_destroyed = last_of("session_handle_destroy");
                assert!(
                    sender_destroyed < volume_closed && volume_closed < session_destroyed,
                    "sender at {sender_destroyed}, volume at {volume_closed}, session at \
                     {session_destroyed}"
                );
                assert_eq!(stub.live_handle_count().expect("the handle count"), 0);
                let _ = std::fs::remove_dir_all(&scratch);
            }

            /// Changes the app volume's scale once the headset has streamed for a few eye
            /// frames, from the render thread, as a person scaling the volume in the Hub would.
            struct VolumeScalingHook {
                connection: Rc<QarosConnection>,
                streaming_frames: u32,
                scaled: Arc<AtomicBool>,
            }

            impl FrameHook for VolumeScalingHook {
                fn on_viewer_frame(&mut self, headset: &HeadsetFrame) {
                    if !headset.is_streaming || self.scaled.load(Ordering::Acquire) {
                        return;
                    }
                    self.streaming_frames += 1;
                    if self.streaming_frames == 3 {
                        self.connection
                            .session()
                            .app_volumes()
                            .set_app_scale(self.connection.volume(), 0.5)
                            .expect("the stub changes the app scale");
                        self.scaled.store(true, Ordering::Release);
                    }
                }
            }

            /// With the sink on the render thread, a change of the app volume reaches the
            /// frames begun there: near and far go back at the requested scale 1 before it and
            /// at the changed scale 0.5 after it.
            #[test]
            fn an_app_volume_change_reaches_the_frames_begun_on_the_render_thread() {
                let Some(gpu) = test_device() else {
                    return;
                };
                let _serial = STUB_SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
                let stub = StubControl::open().expect("the stub");
                let scratch = std::env::temp_dir().join(format!(
                    "qaros-rerun-decoupled-volume-test-{}",
                    std::process::id()
                ));
                std::fs::create_dir_all(&scratch).expect("a scratch directory");
                let handoff = Arc::new(SceneHandoff::default());
                let scaled = Arc::new(AtomicBool::new(false));
                let (hook_scaled, thread_scratch) = (Arc::clone(&scaled), scratch.clone());
                let (mut decoupled, ()) = DecoupledHeadset::start(
                    HeadsetSettings::default(),
                    Arc::clone(&handoff),
                    move || {
                        let connection =
                            Rc::new(connect(&connect_options(&thread_scratch), Arc::new(|| {}))?);
                        let sink = QarosSink::new(
                            Rc::clone(&connection),
                            QarosSinkOptions {
                                depth_range: frame_sink::DepthRange::new(
                                    0.05,
                                    20.0,
                                    xr_math::DepthEncoding::ForwardNormalized,
                                )
                                .ok_or("no depth range")?,
                                pose_space: PoseSpace::App,
                                color_format: qaros_sink::qaros::PixelFormat::B8G8R8A8,
                                max_users: 1,
                                stop_after_frames: None,
                            },
                        );
                        let hook = VolumeScalingHook {
                            connection,
                            streaming_frames: 0,
                            scaled: hook_scaled,
                        };
                        Ok((
                            RenderThreadSink {
                                sink: HeadsetSink::Cpu(Box::new(sink)),
                                frame_hooks: vec![Box::new(hook)],
                            },
                            (),
                        ))
                    },
                )
                .expect("the render thread starts");
                decoupled.send_device(shared_device(&gpu));
                let shown_at = |calls: &[String], line: &str| calls.iter().any(|call| call == line);
                let mut sequence = 0;
                wait_until("a frame at the changed scale is shown", || {
                    sequence += 1;
                    handoff.publish(Some(empty_scene(sequence)));
                    let calls = stub.calls().expect("the call log");
                    shown_at(&calls, "shown near=0.1 far=40 depth_scale=1")
                });
                assert!(scaled.load(Ordering::Acquire));
                drop(decoupled);

                let calls = stub.calls().expect("the call log");
                assert!(
                    shown_at(&calls, "shown near=0.05 far=20 depth_scale=1"),
                    "frames before the change at the requested scale 1"
                );
                let errors: Vec<&String> = calls
                    .iter()
                    .filter(|call| call.starts_with("ERROR"))
                    .collect();
                assert!(errors.is_empty(), "{errors:?}");
                assert_eq!(stub.live_handle_count().expect("the handle count"), 0);
                let _ = std::fs::remove_dir_all(&scratch);
            }
        }
    }

    #[test]
    fn without_a_change_the_wait_lasts_its_timeout_and_a_stop_ends_it() {
        let handoff = Arc::new(SceneHandoff::default());
        let (_, generation) = handoff.latest();
        let started = Instant::now();
        handoff.wait_for_change(generation, Duration::from_millis(40));
        assert!(started.elapsed() >= Duration::from_millis(40));

        let stopper = {
            let handoff = Arc::clone(&handoff);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                handoff.request_stop();
            })
        };
        let started = Instant::now();
        handoff.wait_for_change(generation, Duration::from_secs(5));
        stopper.join().expect("stopper");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(handoff.is_stop_requested());
    }
}
