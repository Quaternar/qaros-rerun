//! The headset renderer: per viewer frame, renders the streamed 3D view once per eye of every
//! rendered user, with the poses the sink issued for that user's frame, and hands finished
//! frames to the sink: read back to the CPU for a [`FrameSink`], or written into the sink's own
//! textures for a [`GpuFrameSink`].
//!
//! The work splits over one viewer frame, with the sink lent to the UI pass
//! ([`with_sink_lent_to_pass`]):
//! 1. While the 3D view draws (the patched `re_view_spatial` calls [`HeadsetRenderer::on_view_3d_frame`]),
//!    frames whose readbacks arrived are shown, the sink returns the next frame of every user
//!    without one in flight, and two eye views per frame are built from copies of the same
//!    draw data, each with the pose and field of view of its own frame. Showing before
//!    beginning lets a user start a frame every viewer frame.
//! 2. After the viewer's UI pass ([`HeadsetRenderer::end_viewer_frame`]), the sink applies its
//!    user changes if no frames were begun in the pass, the eye views are drawn and submitted
//!    on the viewer's queue (on the GPU path also composited into a texture per eye), finished
//!    frames are shown, and stale frames are abandoned, every viewer frame whether or not a 3D
//!    view was drawn.
//!
//! On the CPU path, color and depth come back through the screenshot and picking readbacks and
//! the frame is shown once both have arrived. On the GPU path nothing is read back: the eye is
//! composited into a texture, the picking layer keeps its depth on the GPU, and one pass per
//! eye writes both into the sink's targets in the viewer frame the eyes were drawn in.
//!
//! Every frame carries its user and frame id, and is shown only with the images rendered for
//! its own request, so a user's frame can never show another frame's pose. `--frame-stamp`
//! makes that visible in each eye image, on the CPU and the GPU path.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use frame_sink::{
    EYE_COUNT, EyeImage, FrameKey, FrameRequest, FrameSink, FrameStamp, GpuEyeTargets,
    GpuFrameSink, ShowInfo, SinkError, UserId, ViewRequest, perf, stamp_eye,
};
use re_renderer::{
    GpuTexture, PickingLayerProcessor, QueueableDrawData, RenderContext, ScreenshotProcessor,
    ViewBuilder,
};
use re_view_spatial::view_3d_observer::View3DFrame;
use re_viewer_context::ViewId;
use xr_math::{AppPlacement, DepthRange, EyeCamera, ViewCoordinates};

use crate::draw_data_copy::copy_all;
use crate::eye_output::{
    DepthOutput, EyeOutput, KeptCompositeTargets, StampOverlay, composite_target_for_eye,
};
use crate::eye_render::{
    EyeReadbackIdentifiers, EyeReadbackTag, EyeViewSubmission, draw_and_submit,
    eye_view_builder_id, target_configuration,
};

const EYE_NAMES: [&str; EYE_COUNT] = ["left", "right"];

/// Distance written as the depth of frame stamp pixels, so the mixer keeps them in front.
const STAMP_DISTANCE_METERS: f32 = 1.0;

#[derive(Clone, Copy, Debug)]
pub struct HeadsetSettings {
    /// Where Rerun's world sits in the room; its `world_coordinates` are replaced by the view's
    /// own coordinates unless `world_coordinates_override` is set.
    pub placement: AppPlacement,
    pub world_coordinates_override: Option<ViewCoordinates>,
    /// Viewer frames to wait for a readback before the frame is abandoned.
    pub readback_timeout_in_viewer_frames: u64,
    /// Clear empty pixels to transparent, so only Rerun's content reaches the room.
    pub transparent_background: bool,
    /// Draw user, frame id and eye into every eye image (`--frame-stamp`).
    pub frame_stamp: bool,
    /// GPU path: keep the output pass's uniform buffers and one composite target per user and
    /// eye across frames instead of creating them for every eye frame (ADR-0057); off only with
    /// `QAROS_RERUN_EYE_REUSE=0`. `--frame-stamp` still uploads its stamp every eye frame.
    pub reuse_eye_resources: bool,
}

impl Default for HeadsetSettings {
    fn default() -> Self {
        Self {
            placement: AppPlacement::default(),
            world_coordinates_override: None,
            readback_timeout_in_viewer_frames: 30,
            transparent_background: true,
            frame_stamp: false,
            reuse_eye_resources: true,
        }
    }
}

/// The sink the renderer feeds.
pub enum HeadsetSink {
    /// Color and depth read back and handed over as pixels.
    Cpu(Box<dyn FrameSink>),
    /// Both eyes rendered into the sink's textures.
    Gpu(Box<dyn GpuFrameSink>),
}

impl HeadsetSink {
    pub fn is_streaming(&self) -> bool {
        match self {
            Self::Cpu(sink) => sink.is_streaming(),
            Self::Gpu(sink) => sink.is_streaming(),
        }
    }

    pub fn is_finished(&self) -> bool {
        match self {
            Self::Cpu(sink) => sink.is_finished(),
            Self::Gpu(sink) => sink.is_finished(),
        }
    }

    pub fn depth_range(&self) -> DepthRange {
        match self {
            Self::Cpu(sink) => sink.depth_range(),
            Self::Gpu(sink) => sink.depth_range(),
        }
    }

    fn abandon_frame(&mut self, key: FrameKey) {
        match self {
            Self::Cpu(sink) => sink.abandon_frame(key),
            Self::Gpu(sink) => sink.abandon_frame(key),
        }
    }

    fn update_users(&mut self) {
        match self {
            Self::Cpu(sink) => sink.update_users(),
            Self::Gpu(sink) => sink.update_users(),
        }
    }

    fn begin_frames(&mut self) -> Result<Vec<PendingRequest>, SinkError> {
        Ok(match self {
            Self::Cpu(sink) => sink
                .begin_frames()?
                .into_iter()
                .map(|request| PendingRequest {
                    request,
                    gpu_targets: None,
                })
                .collect(),
            Self::Gpu(sink) => sink
                .begin_frames()?
                .into_iter()
                .map(|gpu_request| PendingRequest {
                    request: gpu_request.request,
                    gpu_targets: Some(gpu_request.eyes),
                })
                .collect(),
        })
    }
}

/// Frames the sink began, whose eye views are still to be built.
pub struct BegunFrames(Vec<PendingRequest>);

impl BegunFrames {
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// A frame the sink began, to be rendered by the 3D view in the same pass.
struct PendingRequest {
    request: FrameRequest,
    /// The sink's textures, for a GPU sink.
    gpu_targets: Option<[GpuEyeTargets; EYE_COUNT]>,
}

thread_local! {
    /// The viewer app's sink while its UI pass runs, so the 3D view observer can show
    /// finished frames and begin the next ones inside the pass. The observer must be
    /// `Send + Sync` and cannot hold a thread-bound sink (QAROS handles); the pass and the
    /// observer run on the UI thread, which is the only thread that ever sees the sink.
    static SINK_DURING_PASS: RefCell<Option<HeadsetSink>> = const { RefCell::new(None) };
}

/// Runs the viewer's UI pass with `sink` reachable from [`HeadsetRenderer::on_view_3d_frame`]
/// on this thread, and puts it back afterwards.
pub fn with_sink_lent_to_pass<Output>(
    sink: &mut HeadsetSink,
    pass: impl FnOnce() -> Output,
) -> Output {
    let stand_in = HeadsetSink::Cpu(Box::new(SinkLentToPass {
        depth_range: sink.depth_range(),
    }));
    SINK_DURING_PASS.set(Some(std::mem::replace(sink, stand_in)));
    let output = pass();
    if let Some(returned) = SINK_DURING_PASS.take() {
        *sink = returned;
    }
    output
}

/// Holds the sink's place in the viewer app while the real one is lent to the UI pass.
struct SinkLentToPass {
    depth_range: DepthRange,
}

impl FrameSink for SinkLentToPass {
    fn begin_frames(&mut self) -> Result<Vec<FrameRequest>, SinkError> {
        Ok(Vec::new())
    }

    fn submit_view(&mut self, key: FrameKey, _: usize, _: EyeImage) -> Result<(), SinkError> {
        Err(SinkError::UnknownFrame(key))
    }

    fn show_frame(&mut self, key: FrameKey, _: ShowInfo) -> Result<(), SinkError> {
        Err(SinkError::UnknownFrame(key))
    }

    fn abandon_frame(&mut self, _: FrameKey) {}

    fn is_streaming(&self) -> bool {
        false
    }

    fn is_finished(&self) -> bool {
        false
    }

    fn depth_range(&self) -> DepthRange {
        self.depth_range
    }
}

#[derive(Clone, Debug, Default)]
pub struct UserStatistics {
    pub frames_shown: u64,
    pub total_begin_to_show: Duration,
    /// Viewer frames between begin and show, or with `--decoupled-render` eye frames that
    /// drew ([`HeadsetStatistics::frames_counted_in_eye_frames`]).
    pub total_frames_in_flight: u64,
    pub first_show: Option<Instant>,
    pub last_show: Option<Instant>,
}

impl UserStatistics {
    /// Shown frames per second between this user's first and last shown frame.
    #[allow(clippy::cast_precision_loss)]
    pub fn shown_per_second(&self) -> f64 {
        match (self.first_show, self.last_show) {
            (Some(first), Some(last)) if self.frames_shown > 1 => {
                (self.frames_shown - 1) as f64 / last.duration_since(first).as_secs_f64()
            }
            _ => 0.0,
        }
    }

    fn record_show(&mut self, started_at: Instant, frames_in_flight: u64) {
        let now = Instant::now();
        self.frames_shown += 1;
        self.total_begin_to_show += now.duration_since(started_at);
        self.total_frames_in_flight += frames_in_flight;
        self.first_show.get_or_insert(now);
        self.last_show = Some(now);
    }
}

/// How many frames of one kind ran, and when the first and the last did.
#[derive(Clone, Debug, Default)]
pub struct FrameCount {
    pub count: u64,
    pub first: Option<Instant>,
    pub last: Option<Instant>,
}

impl FrameCount {
    /// Frames per second between the first and the last.
    #[allow(clippy::cast_precision_loss)]
    pub fn per_second(&self) -> f64 {
        match (self.first, self.last) {
            (Some(first), Some(last)) if self.count > 1 => {
                (self.count - 1) as f64 / last.duration_since(first).as_secs_f64()
            }
            _ => 0.0,
        }
    }

    pub fn record(&mut self) {
        let now = Instant::now();
        self.count += 1;
        self.first.get_or_insert(now);
        self.last = Some(now);
    }
}

#[derive(Clone, Debug, Default)]
pub struct HeadsetStatistics {
    /// `--decoupled-render`: render thread iterations that drew eyes.
    pub eye_frames: FrameCount,
    /// `--decoupled-render`: frames in flight are counted in [`Self::eye_frames`], not in
    /// viewer frames.
    pub frames_counted_in_eye_frames: bool,
    pub frames_started: u64,
    pub frames_shown: u64,
    pub frames_abandoned: u64,
    pub draw_data_received: u64,
    pub draw_data_copied: u64,
    pub per_user: BTreeMap<UserId, UserStatistics>,
}

impl HeadsetStatistics {
    #[allow(clippy::cast_precision_loss)]
    pub fn summary(&self) -> String {
        let mut summary = format!(
            "frames started {}, shown {}, abandoned {}; draw data copied {} of {}",
            self.frames_started,
            self.frames_shown,
            self.frames_abandoned,
            self.draw_data_copied,
            self.draw_data_received,
        );
        if self.eye_frames.count > 0 {
            let _ = write!(
                summary,
                "; render thread eye frames {} ({:.1} per second)",
                self.eye_frames.count,
                self.eye_frames.per_second(),
            );
        }
        let frame_kind = if self.frames_counted_in_eye_frames {
            "eye frames"
        } else {
            "viewer frames"
        };
        for (user, statistics) in &self.per_user {
            let shown = statistics.frames_shown.max(1) as f64;
            let _ = write!(
                summary,
                "\n  {user}: shown {}; {:.1} shown per second; begin to show {:.1} ms and \
                 {:.2} {frame_kind} on average",
                statistics.frames_shown,
                statistics.shown_per_second(),
                statistics.total_begin_to_show.as_secs_f64() * 1000.0 / shown,
                statistics.total_frames_in_flight as f64 / shown,
            );
        }
        summary
    }
}

enum EyeInFlight {
    /// CPU path: the screenshot and picking readbacks, filled as they arrive.
    Readback {
        identifiers: EyeReadbackIdentifiers,
        color_rgba8: Option<Vec<u8>>,
        reverse_depth: Option<Vec<f32>>,
    },
    /// GPU path: Rerun's composited eye and the picking layer's reverse-Z depth, both written
    /// when the eye views are drawn.
    OnGpu {
        composited: wgpu::Texture,
        reverse_depth: GpuTexture,
    },
}

impl EyeInFlight {
    fn is_complete(&self) -> bool {
        match self {
            Self::Readback {
                color_rgba8,
                reverse_depth,
                ..
            } => color_rgba8.is_some() && reverse_depth.is_some(),
            Self::OnGpu { .. } => true,
        }
    }
}

/// Makes pixels where nothing was drawn fully transparent.
///
/// `re_renderer`'s screenshot compositing writes opaque alpha everywhere, so an empty pixel
/// would reach the mixer as opaque black. Coverage comes from the depth layer instead: Rerun
/// clears reverse-Z depth to 0, and anything drawn writes a positive value.
pub fn clear_uncovered_pixels(color_rgba8: &mut [u8], reverse_depth: &[f32]) {
    for (pixel, &depth) in color_rgba8.chunks_exact_mut(4).zip(reverse_depth) {
        if !is_covered(depth) {
            pixel.copy_from_slice(&[0, 0, 0, 0]);
        }
    }
}

fn is_covered(reverse_depth: f32) -> bool {
    reverse_depth > 0.0
}

/// One 3D view streams at a time: the first one drawn keeps the stream for as long as it keeps
/// being drawn every viewer frame.
pub fn claim_streaming_view(
    streaming_view: &mut Option<(ViewId, u64)>,
    view_id: ViewId,
    viewer_frame: u64,
) -> bool {
    match *streaming_view {
        Some((owner, last_frame)) if owner != view_id && viewer_frame <= last_frame + 1 => false,
        _ => {
            *streaming_view = Some((view_id, viewer_frame));
            true
        }
    }
}

/// Fails for coordinates that cannot be placed in the room (left-handed), before any frame is
/// requested from the sink.
pub fn placement_for_view(
    settings: &HeadsetSettings,
    frame: &View3DFrame<'_, '_>,
) -> Result<AppPlacement, String> {
    let world_coordinates = settings.world_coordinates_override.unwrap_or_else(|| {
        ViewCoordinates::parse(&frame.view_coordinates.describe_short()).unwrap_or_else(|error| {
            re_log::warn_once!("headset: {error}; assuming Z up (RFU)");
            ViewCoordinates::RIGHT_FORWARD_UP
        })
    });
    world_coordinates
        .room_from_coordinates()
        .map_err(|error| format!("{error} ({world_coordinates:?})"))?;
    Ok(AppPlacement {
        world_coordinates,
        ..settings.placement
    })
}

/// Begins the sink's frames only for a view that can be placed in the room, so a view that
/// cannot be placed (left-handed coordinates) costs no user a QAROS frame. The sink's
/// `begin_frames` runs exactly when `placement` is `Ok`.
fn begin_placeable_frames(
    placement: Result<AppPlacement, String>,
    sink: &mut HeadsetSink,
) -> Result<(AppPlacement, Vec<PendingRequest>), String> {
    let placement = placement.map_err(|error| format!("not streaming this view: {error}"))?;
    let pending = sink
        .begin_frames()
        .map_err(|error| format!("sink cannot begin frames: {error}"))?;
    Ok((placement, pending))
}

fn frame_stamp(key: FrameKey, eye_index: usize) -> FrameStamp {
    FrameStamp {
        user: key.user,
        frame_id: key.frame_id,
        eye_index,
    }
}

/// Where stamp pixels sit, in room meters, inside the planes the frame is rendered with.
fn stamp_distance(depth_range: DepthRange) -> f32 {
    STAMP_DISTANCE_METERS.clamp(depth_range.near(), depth_range.far())
}

/// The stamp pixels' depth in Rerun's reverse-Z, `near / distance` as the picking layer holds
/// it, so the GPU output pass converts it like any drawn pixel.
fn stamp_reverse_depth(depth_range: DepthRange) -> f32 {
    depth_range.near() / stamp_distance(depth_range)
}

/// The GPU path's stamp: the top rows of an eye `eye_width` pixels wide, transparent where
/// nothing is stamped.
fn gpu_stamp_image(key: FrameKey, eye_index: usize, eye_width: u32) -> EyeImage {
    let rows = frame_sink::frame_stamp::stamp_rows(eye_width);
    let pixel_count = eye_width as usize * rows as usize;
    let mut image = EyeImage {
        resolution_in_pixel: [eye_width, rows],
        color_rgba8: vec![0; pixel_count * 4],
        depth: vec![0.0; pixel_count],
    };
    stamp_eye(&mut image, frame_stamp(key, eye_index), 0.0);
    image
}

/// What a frame's eyes are drawn from: the 3D view drawn in this viewer frame, or a scene
/// retained from an earlier one (`--decoupled-render`).
pub struct SceneSource<'scene> {
    /// Seeds the eye view builder ids, so `re_renderer`'s per-view caches follow the view.
    pub view_seed: u64,
    pub placement: AppPlacement,
    pub draw_data: &'scene [QueueableDrawData],
}

struct FrameInFlight {
    request: FrameRequest,
    /// Unique per prepared frame; names its readbacks.
    frame_sequence: u64,
    depth_range: DepthRange,
    eyes: [EyeInFlight; EYE_COUNT],
    gpu_targets: Option<[GpuEyeTargets; EYE_COUNT]>,
    started_at: Instant,
    started_at_viewer_frame: u64,
    /// `--decoupled-render`: the eye frames that drew before the one that draws it, counted
    /// like [`HeadsetStatistics::eye_frames`].
    started_at_eye_frame: u64,
}

/// Renders the eyes. The sink is not stored here: the viewer app owns it, passes it in on
/// the UI thread and lends it to the UI pass, so sinks holding thread-bound handles (QAROS)
/// never cross threads, while this renderer is shared with the 3D view observer, which must
/// be `Send + Sync`.
pub struct HeadsetRenderer {
    settings: HeadsetSettings,
    depth_range: DepthRange,
    sink_is_streaming: bool,
    streaming_view: Option<(ViewId, u64)>,
    /// The sink's `begin_frames` ran in this viewer frame's UI pass, which applied its user
    /// changes already.
    sink_began_during_ui: bool,
    frames_failed_during_ui: Vec<FrameKey>,
    in_flight: Vec<FrameInFlight>,
    eye_views_to_submit: Vec<EyeViewSubmission>,
    /// Created on the first GPU frame, on the viewer's device.
    eye_output: Option<EyeOutput>,
    /// `None` creates a composite target for every eye frame.
    kept_composite_targets: Option<KeptCompositeTargets>,
    next_frame_sequence: u64,
    statistics: HeadsetStatistics,
    /// `--decoupled-render`: a frame whose readback has not arrived this long after it began
    /// is abandoned, in place of `readback_timeout_in_viewer_frames`, since eye frames on the
    /// render thread come far faster than viewer frames.
    readback_time_limit: Option<Duration>,
    /// The head of the user the last frame was prepared for, in room space.
    latest_head: Option<xr_math::Pose>,
}

/// A user's head from the two eye poses of one frame: midway between the eyes, turned like
/// the left eye.
fn head_between_eyes(request: &FrameRequest) -> xr_math::Pose {
    let [left, right] = [request.views[0].pose, request.views[1].pose];
    xr_math::Pose::new(left.orientation, (left.position + right.position) * 0.5)
}

impl HeadsetRenderer {
    pub fn new(settings: HeadsetSettings, sink: &HeadsetSink) -> Self {
        Self {
            settings,
            depth_range: sink.depth_range(),
            sink_is_streaming: sink.is_streaming(),
            streaming_view: None,
            sink_began_during_ui: false,
            frames_failed_during_ui: Vec::new(),
            in_flight: Vec::new(),
            eye_views_to_submit: Vec::new(),
            eye_output: None,
            kept_composite_targets: settings
                .reuse_eye_resources
                .then(KeptCompositeTargets::default),
            next_frame_sequence: 0,
            statistics: HeadsetStatistics::default(),
            readback_time_limit: None,
            latest_head: None,
        }
    }

    /// The head of the user this renderer last prepared a frame for, in room space (the sink
    /// maps every eye pose into the room before it reaches the renderer). Frames of every
    /// streaming user pass through here, a headset's and a desktop receiver's alike. `None`
    /// until a stream has started.
    pub fn latest_head(&self) -> Option<xr_math::Pose> {
        self.latest_head
    }

    pub fn statistics(&self) -> &HeadsetStatistics {
        &self.statistics
    }

    /// Abandons frames whose readback has not arrived within `limit`, counted in time instead
    /// of viewer frames.
    #[must_use]
    pub fn with_readback_time_limit(mut self, limit: Duration) -> Self {
        self.readback_time_limit = Some(limit);
        self.statistics.frames_counted_in_eye_frames = true;
        self
    }

    pub fn has_frames_open(&self) -> bool {
        !self.in_flight.is_empty()
    }

    /// Runs once per viewer frame before the UI pass, while the app still holds the sink.
    pub fn begin_viewer_frame(&mut self, sink: &HeadsetSink) {
        self.sink_is_streaming = sink.is_streaming();
        self.depth_range = sink.depth_range();
    }

    /// Called by the patched 3D view for every 3D view, every viewer frame it is drawn. With
    /// the sink lent to the pass, shows the frames that are complete, then begins and prepares
    /// the next frame of every user without one in flight.
    pub fn on_view_3d_frame(&mut self, frame: &View3DFrame<'_, '_>) {
        let viewer_frame = frame.viewer_ctx.egui_ctx().cumulative_pass_nr();
        if !self.claim_view(frame.query.view_id, viewer_frame) {
            return;
        }
        if self.sink_is_streaming || self.has_frames_open() {
            frame.viewer_ctx.egui_ctx().request_repaint();
        }
        SINK_DURING_PASS.with_borrow_mut(|lent| {
            if let Some(sink) = lent.as_mut() {
                self.render_frames(frame, sink, viewer_frame);
            }
        });
    }

    fn render_frames(
        &mut self,
        frame: &View3DFrame<'_, '_>,
        sink: &mut HeadsetSink,
        viewer_frame: u64,
    ) {
        // re_renderer maps last viewer frame's readbacks early in this pass, so finished
        // frames go out first and their users can begin the next frame in the same pass.
        {
            let _readbacks = perf::Span::start("ui_readbacks_and_show");
            self.poll_readbacks(frame.viewer_ctx.render_ctx(), viewer_frame, sink);
        }
        let begin_span = perf::Span::start("begin_frames");
        let placement = self.placement_for(frame);
        self.sink_began_during_ui |= placement.is_ok();
        let begun = begin_placeable_frames(placement, sink);
        drop(begin_span);
        let (placement, pending) = match begun {
            Ok(begun) => begun,
            Err(error) => {
                re_log::warn_once!("headset: {error}");
                return;
            }
        };
        let _prepare = perf::Span::start("prepare_frames");
        let scene = SceneSource {
            view_seed: frame.query.view_id.hash(),
            placement,
            draw_data: frame.draw_data,
        };
        for request in pending {
            re_log::debug!(
                "headset: frame {:?}, scene bounds {:?} to {:?} in {}",
                request.request.key,
                frame.scene_bounding_box.min,
                frame.scene_bounding_box.max,
                frame.view_coordinates.describe_short()
            );
            self.prepare_frame(frame.viewer_ctx.render_ctx(), &scene, request, viewer_frame);
        }
    }

    /// `--decoupled-render`, on the render thread inside its own `re_renderer` frame: shows the
    /// frames whose readbacks arrived, then begins the next frame of every user without one in
    /// flight, for [`Self::prepare_frames_from_scene`]. The only part of the two that uses the
    /// sink.
    pub fn begin_frames_for_scene(
        &mut self,
        render_context: &RenderContext,
        eye_frame: u64,
        sink: &mut HeadsetSink,
    ) -> BegunFrames {
        self.poll_readbacks(render_context, eye_frame, sink);
        self.sink_began_during_ui = true;
        match sink.begin_frames() {
            Ok(pending) => BegunFrames(pending),
            Err(error) => {
                re_log::warn_once!("headset: sink cannot begin frames: {error}");
                BegunFrames(Vec::new())
            }
        }
    }

    /// Builds the eye views of the frames just begun from the retained `scene`, without the
    /// sink.
    pub fn prepare_frames_from_scene(
        &mut self,
        render_context: &RenderContext,
        scene: &SceneSource<'_>,
        begun: BegunFrames,
        eye_frame: u64,
    ) {
        for request in begun.0 {
            self.prepare_frame(render_context, scene, request, eye_frame);
        }
    }

    /// `--decoupled-render`: an eye frame with no scene to draw still shows finished frames.
    pub fn show_finished_frames(
        &mut self,
        render_context: &RenderContext,
        eye_frame: u64,
        sink: &mut HeadsetSink,
    ) {
        self.poll_readbacks(render_context, eye_frame, sink);
    }

    pub fn record_eye_frame(&mut self) {
        self.statistics.eye_frames.record();
    }

    /// Runs once per viewer frame after the UI pass, whether or not a 3D view was drawn:
    /// applies the sink's user changes when no frames were begun, releases frames that could
    /// not be set up, draws and submits the eye views built during this frame, then shows
    /// finished frames and releases frames whose readback never arrived.
    pub fn end_viewer_frame(
        &mut self,
        render_context: &RenderContext,
        viewer_frame: u64,
        sink: &mut HeadsetSink,
    ) {
        self.settle_frames_before_drawing(sink);
        let failed_frames = self.draw_eyes(render_context);
        self.show_and_release_after_drawing(render_context, viewer_frame, sink, failed_frames);
    }

    /// The first part of [`Self::end_viewer_frame`]: user changes and frames that could not be
    /// set up.
    pub fn settle_frames_before_drawing(&mut self, sink: &mut HeadsetSink) {
        self.update_users_unless_begun(sink);
        for key in std::mem::take(&mut self.frames_failed_during_ui) {
            sink.abandon_frame(key);
            self.statistics.frames_abandoned += 1;
        }
    }

    /// The second part of [`Self::end_viewer_frame`], without the sink: draws and submits the
    /// eye views, returning the frames whose eye failed to draw.
    pub fn draw_eyes(&mut self, render_context: &RenderContext) -> Vec<FrameKey> {
        draw_and_submit(
            render_context,
            std::mem::take(&mut self.eye_views_to_submit),
        )
    }

    /// The last part of [`Self::end_viewer_frame`]: shows finished frames and releases frames
    /// that failed to draw or whose readback never arrived.
    pub fn show_and_release_after_drawing(
        &mut self,
        render_context: &RenderContext,
        viewer_frame: u64,
        sink: &mut HeadsetSink,
        failed_frames: Vec<FrameKey>,
    ) {
        for key in failed_frames {
            self.abandon_in_flight(key, sink);
        }
        let _show = perf::Span::start("end_readbacks_and_show");
        self.poll_readbacks(render_context, viewer_frame, sink);
        self.abandon_stale_frames(viewer_frame, sink);
        let now = Instant::now();
        if let Some(kept_targets) = &mut self.kept_composite_targets {
            kept_targets.release_unused(now);
        }
        if let Some(eye_output) = &mut self.eye_output {
            eye_output.release_unused(now);
        }
    }

    /// Users join, leave and get their senders every viewer frame, also while no placeable 3D
    /// view is drawn (a 2D-only layout, a collapsed tab), once per viewer frame.
    fn update_users_unless_begun(&mut self, sink: &mut HeadsetSink) {
        if !std::mem::take(&mut self.sink_began_during_ui) {
            sink.update_users();
        }
    }

    /// Abandons every frame the sink began, drawn or not, and drops what refers to the sink's
    /// textures.
    pub fn abandon_all(&mut self, sink: &mut HeadsetSink) {
        let begun = self
            .in_flight
            .drain(..)
            .map(|frame| frame.request.key)
            .chain(
                self.eye_views_to_submit
                    .drain(..)
                    .map(|submission| submission.key),
            )
            .chain(self.frames_failed_during_ui.drain(..));
        let mut abandoned: Vec<FrameKey> = begun.collect();
        abandoned.sort_unstable();
        abandoned.dedup();
        for key in abandoned {
            sink.abandon_frame(key);
            self.statistics.frames_abandoned += 1;
        }
    }

    fn abandon_in_flight(&mut self, key: FrameKey, sink: &mut HeadsetSink) {
        let before = self.in_flight.len();
        self.in_flight.retain(|frame| frame.request.key != key);
        if self.in_flight.len() != before {
            sink.abandon_frame(key);
            self.statistics.frames_abandoned += 1;
        }
    }

    fn claim_view(&mut self, view_id: ViewId, viewer_frame: u64) -> bool {
        claim_streaming_view(&mut self.streaming_view, view_id, viewer_frame)
    }

    fn placement_for(&self, frame: &View3DFrame<'_, '_>) -> Result<AppPlacement, String> {
        placement_for_view(&self.settings, frame)
    }

    fn prepare_frame(
        &mut self,
        render_context: &RenderContext,
        scene: &SceneSource<'_>,
        pending: PendingRequest,
        viewer_frame: u64,
    ) {
        let placement = scene.placement;
        let PendingRequest {
            request,
            gpu_targets,
        } = pending;
        // A user released and admitted again starts its frame ids over; a frame still in
        // flight under the same key belongs to the old stream, whose sink frame is gone.
        self.in_flight
            .retain(|stale| stale.request.key != request.key);
        self.latest_head = Some(head_between_eyes(&request));
        let frame_sequence = self.next_frame_sequence;
        self.next_frame_sequence += 1;
        let depth_range = self.depth_range;
        let mut submissions = Vec::with_capacity(EYE_COUNT);
        let mut eyes = Vec::with_capacity(EYE_COUNT);

        for (eye_index, view) in request.views.iter().enumerate() {
            let read_back = gpu_targets.is_none();
            let eye_view = self
                .build_eye_view(
                    render_context,
                    scene,
                    &request,
                    frame_sequence,
                    eye_index,
                    *view,
                    placement,
                    depth_range,
                    read_back,
                )
                .and_then(|view_builder| {
                    let eye = if read_back {
                        EyeInFlight::Readback {
                            identifiers: EyeReadbackIdentifiers::for_eye(frame_sequence, eye_index),
                            color_rgba8: None,
                            reverse_depth: None,
                        }
                    } else {
                        let reverse_depth = view_builder
                            .picking_depth_target()
                            .cloned()
                            .ok_or("the eye view has no picking layer")?;
                        let create_span = perf::Span::start("composite_target_create");
                        let composited = composite_target_for_eye(
                            self.kept_composite_targets.as_mut(),
                            &render_context.device,
                            render_context.output_format_color(),
                            request.resolution_in_pixel,
                            (request.key.user, eye_index),
                            Instant::now(),
                        );
                        drop(create_span);
                        EyeInFlight::OnGpu {
                            composited,
                            reverse_depth,
                        }
                    };
                    Ok((view_builder, eye))
                });
            match eye_view {
                Ok((view_builder, eye)) => {
                    let composite_into = match &eye {
                        EyeInFlight::OnGpu { composited, .. } => Some(composited.clone()),
                        EyeInFlight::Readback { .. } => None,
                    };
                    // Drawn and composited in this viewer frame's `end_viewer_frame`, before
                    // the frame is checked for completion.
                    submissions.push(EyeViewSubmission {
                        key: request.key,
                        view_builder,
                        composite_into,
                    });
                    eyes.push(eye);
                }
                Err(error) => {
                    re_log::warn_once!(
                        "headset: cannot set up the {} eye: {error}",
                        EYE_NAMES[eye_index]
                    );
                    self.frames_failed_during_ui.push(request.key);
                    return;
                }
            }
        }
        let Ok(eyes) = <[EyeInFlight; EYE_COUNT]>::try_from(eyes) else {
            self.frames_failed_during_ui.push(request.key);
            return;
        };

        self.eye_views_to_submit.extend(submissions);
        self.statistics.frames_started += 1;
        perf::record("frame_begun", Some(&request.key), u128::from(viewer_frame));
        self.in_flight.push(FrameInFlight {
            request,
            frame_sequence,
            depth_range,
            eyes,
            gpu_targets,
            started_at: Instant::now(),
            started_at_viewer_frame: viewer_frame,
            started_at_eye_frame: self.statistics.eye_frames.count,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn build_eye_view(
        &mut self,
        render_context: &RenderContext,
        scene: &SceneSource<'_>,
        request: &FrameRequest,
        frame_sequence: u64,
        eye_index: usize,
        view: ViewRequest,
        placement: AppPlacement,
        depth_range: DepthRange,
        read_back: bool,
    ) -> Result<ViewBuilder, String> {
        let camera = EyeCamera::new(view.pose, view.fov, placement, depth_range)
            .map_err(|error| error.to_string())?;
        let identifiers = EyeReadbackIdentifiers::for_eye(frame_sequence, eye_index);
        let config = target_configuration(
            &camera,
            request.resolution_in_pixel,
            EYE_NAMES[eye_index],
            identifiers.depth,
        );
        let view_builder_id = eye_view_builder_id(scene.view_seed, request.key.user, eye_index);
        let view_builder_span = perf::Span::start("view_builder_new");
        let mut view_builder = if read_back {
            ViewBuilder::new(render_context, config, view_builder_id)
        } else {
            ViewBuilder::new_with_picking_depth_on_gpu(render_context, config, view_builder_id)
        }
        .map_err(|error| error.to_string())?;
        drop(view_builder_span);
        if read_back {
            view_builder
                .schedule_screenshot(
                    render_context,
                    identifiers.color,
                    EyeReadbackTag {
                        frame_sequence,
                        eye_index,
                    },
                )
                .map_err(|error| error.to_string())?;
        }

        let copies = {
            let _copy = perf::Span::start("copy_draw_data");
            copy_all(scene.draw_data)
        };
        self.statistics.draw_data_received += scene.draw_data.len() as u64;
        self.statistics.draw_data_copied += copies.len() as u64;
        let _queue_span = perf::Span::start("queue_draw");
        for draw_data in copies {
            view_builder
                .queue_draw(render_context, draw_data)
                .map_err(|error| error.to_string())?;
        }
        Ok(view_builder)
    }

    fn poll_readbacks(
        &mut self,
        render_context: &RenderContext,
        viewer_frame: u64,
        sink: &mut HeadsetSink,
    ) {
        for frame in &mut self.in_flight {
            let frame_sequence = frame.frame_sequence;
            for (eye_index, eye) in frame.eyes.iter_mut().enumerate() {
                let EyeInFlight::Readback {
                    identifiers,
                    color_rgba8,
                    reverse_depth,
                } = eye
                else {
                    continue;
                };
                if color_rgba8.is_none() {
                    ScreenshotProcessor::next_readback_result::<EyeReadbackTag>(
                        render_context,
                        identifiers.color,
                        |data, _extent, tag| {
                            if tag.frame_sequence == frame_sequence && tag.eye_index == eye_index {
                                *color_rgba8 = Some(data.to_vec());
                            }
                        },
                    );
                }
                if reverse_depth.is_none()
                    && let Some(picking) =
                        PickingLayerProcessor::readback_result(render_context, identifiers.depth)
                {
                    *reverse_depth = Some(picking.picking_depth_data);
                }
            }
        }

        // A GPU frame is complete once its eye views are drawn, in `end_viewer_frame`.
        let awaiting_draw: Vec<FrameKey> = self
            .eye_views_to_submit
            .iter()
            .map(|submission| submission.key)
            .collect();
        let (complete, pending): (Vec<_>, Vec<_>) = std::mem::take(&mut self.in_flight)
            .into_iter()
            .partition(|frame| {
                !awaiting_draw.contains(&frame.request.key)
                    && frame.eyes.iter().all(EyeInFlight::is_complete)
            });
        self.in_flight = pending;
        for frame in complete {
            let key = frame.request.key;
            let started_at = frame.started_at;
            let frames_in_flight = if self.statistics.frames_counted_in_eye_frames {
                self.statistics
                    .eye_frames
                    .count
                    .saturating_sub(frame.started_at_eye_frame)
            } else {
                viewer_frame.saturating_sub(frame.started_at_viewer_frame)
            };
            let shown = perf::time_for_user("show", &key, || match sink {
                HeadsetSink::Cpu(sink) => self.show_on_cpu(frame, sink.as_mut()),
                HeadsetSink::Gpu(sink) => self.show_on_gpu(frame, render_context, sink.as_mut()),
            });
            match shown {
                Ok(()) => {
                    perf::record("frame_shown", Some(&key), started_at.elapsed().as_micros());
                    self.statistics.frames_shown += 1;
                    self.statistics
                        .per_user
                        .entry(key.user)
                        .or_default()
                        .record_show(started_at, frames_in_flight);
                }
                Err(error) => {
                    re_log::warn_once!("headset: sink cannot show {key}: {error}");
                    sink.abandon_frame(key);
                    self.statistics.frames_abandoned += 1;
                }
            }
        }
    }

    /// The eye image as the sink receives it: transparent where nothing was drawn, depth in
    /// the sink's encoding, and the frame stamp when enabled.
    fn eye_image(
        &self,
        key: FrameKey,
        eye_index: usize,
        mut color_rgba8: Vec<u8>,
        reverse_depth: &[f32],
        resolution_in_pixel: [u32; 2],
        depth_range: DepthRange,
    ) -> EyeImage {
        if self.settings.transparent_background {
            clear_uncovered_pixels(&mut color_rgba8, reverse_depth);
        }
        let mut image = EyeImage {
            resolution_in_pixel,
            color_rgba8,
            depth: depth_range.convert_buffer_from_reverse_z(reverse_depth),
        };
        if self.settings.frame_stamp {
            stamp_eye(
                &mut image,
                frame_stamp(key, eye_index),
                depth_range.encode_distance(stamp_distance(depth_range)),
            );
        }
        image
    }

    fn show_on_cpu(&self, frame: FrameInFlight, sink: &mut dyn FrameSink) -> Result<(), String> {
        let key = frame.request.key;
        for (eye_index, eye) in frame.eyes.into_iter().enumerate() {
            let EyeInFlight::Readback {
                color_rgba8: Some(color_rgba8),
                reverse_depth: Some(reverse_depth),
                ..
            } = eye
            else {
                return Err(format!("the {} eye has no readback", EYE_NAMES[eye_index]));
            };
            let image = self.eye_image(
                key,
                eye_index,
                color_rgba8,
                &reverse_depth,
                frame.request.resolution_in_pixel,
                frame.depth_range,
            );
            sink.submit_view(key, eye_index, image)
                .map_err(|error| format!("the {} eye: {error}", EYE_NAMES[eye_index]))?;
        }
        sink.show_frame(key, ShowInfo::from_depth_range(frame.depth_range))
            .map_err(|error| error.to_string())
    }

    /// Writes both eyes into the sink's targets and hands the commands to the sink, which
    /// submits them inside its own synchronization and shows the frame.
    fn show_on_gpu(
        &mut self,
        frame: FrameInFlight,
        render_context: &RenderContext,
        sink: &mut dyn GpuFrameSink,
    ) -> Result<(), String> {
        let device = &render_context.device;
        let Some(targets) = frame.gpu_targets else {
            return Err("a GPU frame without targets".to_owned());
        };
        let eye_output = self
            .eye_output
            .get_or_insert_with(|| EyeOutput::new(device, self.settings.reuse_eye_resources));
        let depth_output = DepthOutput {
            depth_range: frame.depth_range,
            transparent_background: self.settings.transparent_background,
        };
        let encode_span = perf::Span::start("output_encode");
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("headset eye output"),
        });
        for ((eye_index, eye), eye_targets) in frame.eyes.into_iter().enumerate().zip(&targets) {
            let EyeInFlight::OnGpu {
                composited,
                reverse_depth,
            } = eye
            else {
                return Err(format!(
                    "the {} eye is not on the GPU",
                    EYE_NAMES[eye_index]
                ));
            };
            let stamp_image = self
                .settings
                .frame_stamp
                .then(|| gpu_stamp_image(frame.request.key, eye_index, eye_targets.color.width()));
            let stamp = stamp_image.as_ref().map(|image| StampOverlay {
                image,
                reverse_depth: stamp_reverse_depth(frame.depth_range),
            });
            eye_output
                .encode(
                    device,
                    &mut encoder,
                    &composited,
                    &reverse_depth.texture,
                    depth_output,
                    stamp,
                    eye_targets,
                )
                .map_err(|error| format!("the {} eye: {error}", EYE_NAMES[eye_index]))?;
        }
        let commands = encoder.finish();
        drop(encode_span);
        sink.submit_and_show(
            frame.request.key,
            commands,
            ShowInfo::from_depth_range(frame.depth_range),
        )
        .map_err(|error| error.to_string())
    }

    fn abandon_stale_frames(&mut self, viewer_frame: u64, sink: &mut HeadsetSink) {
        let timeout = self.settings.readback_timeout_in_viewer_frames;
        let time_limit = self.readback_time_limit;
        let (stale, fresh): (Vec<_>, Vec<_>) = std::mem::take(&mut self.in_flight)
            .into_iter()
            .partition(|frame| match time_limit {
                Some(limit) => frame.started_at.elapsed() > limit,
                None => viewer_frame > frame.started_at_viewer_frame + timeout,
            });
        self.in_flight = fresh;
        for frame in stale {
            re_log::warn_once!(
                "headset: readback for {} did not arrive within {}",
                frame.request.key,
                time_limit.map_or_else(
                    || format!("{timeout} viewer frames"),
                    |limit| format!("{limit:?}")
                )
            );
            sink.abandon_frame(frame.request.key);
            self.statistics.frames_abandoned += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CPU path's alpha is coverage, 0 or 1: opaque covered pixels and all-zero uncovered
    /// ones are premultiplied as they stand (ADR-0050).
    #[test]
    fn uncovered_pixels_become_transparent_and_premultiplied() {
        let mut color = [10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255];
        clear_uncovered_pixels(&mut color, &[0.0, 0.25, 0.0]);
        assert_eq!(color, [0, 0, 0, 0, 40, 50, 60, 255, 0, 0, 0, 0]);
        assert!(
            color
                .chunks_exact(4)
                .all(|pixel| pixel[..3].iter().all(|&channel| channel <= pixel[3]))
        );
    }

    fn renderer(frame_stamp: bool, depth_range: DepthRange) -> HeadsetRenderer {
        let settings = HeadsetSettings {
            frame_stamp,
            ..HeadsetSettings::default()
        };
        HeadsetRenderer {
            settings,
            depth_range,
            sink_is_streaming: true,
            streaming_view: None,
            sink_began_during_ui: false,
            frames_failed_during_ui: Vec::new(),
            in_flight: Vec::new(),
            eye_views_to_submit: Vec::new(),
            eye_output: None,
            kept_composite_targets: settings
                .reuse_eye_resources
                .then(KeptCompositeTargets::default),
            next_frame_sequence: 0,
            statistics: HeadsetStatistics::default(),
            readback_time_limit: None,
            latest_head: None,
        }
    }

    #[test]
    fn eye_resources_are_kept_by_default_and_made_per_frame_only_on_opt_out() {
        let sink = HeadsetSink::Cpu(Box::new(UserUpdateCounter {
            updates: std::rc::Rc::new(std::cell::Cell::new(0)),
        }));
        let by_default = HeadsetRenderer::new(HeadsetSettings::default(), &sink);
        assert!(by_default.settings.reuse_eye_resources);
        assert!(by_default.kept_composite_targets.is_some());

        let opted_out = HeadsetRenderer::new(
            HeadsetSettings {
                reuse_eye_resources: false,
                ..HeadsetSettings::default()
            },
            &sink,
        );
        assert!(opted_out.kept_composite_targets.is_none());
    }

    #[test]
    fn eye_images_carry_the_stamp_of_their_own_user_frame_and_eye() {
        let depth_range =
            DepthRange::new(0.05, 20.0, xr_math::DepthEncoding::ForwardNormalized).unwrap();
        let resolution = [256, 128];
        let pixel_count = 256 * 128;
        let key = FrameKey {
            user: UserId(2),
            frame_id: 4321,
        };
        for eye_index in 0..EYE_COUNT {
            let image = renderer(true, depth_range).eye_image(
                key,
                eye_index,
                vec![120; pixel_count * 4],
                &vec![0.5; pixel_count],
                resolution,
                depth_range,
            );
            let strip = frame_sink::frame_stamp::read_strip(&image.color_rgba8, resolution)
                .expect("stamped");
            assert!(strip.matches(key.frame_id, eye_index), "{strip:?}");
            let side = if eye_index == 0 { 'L' } else { 'R' };
            assert_eq!(
                frame_sink::frame_stamp::read_text(&image.color_rgba8, resolution),
                format!("U2 F4321 {side}")
            );
            let stamp_depth = depth_range.decode_distance(image.depth[0]);
            assert!((stamp_depth - 1.0).abs() < 1e-3, "{stamp_depth}");
        }
        let unstamped = renderer(false, depth_range).eye_image(
            key,
            0,
            vec![120; pixel_count * 4],
            &vec![0.5; pixel_count],
            resolution,
            depth_range,
        );
        assert!(frame_sink::frame_stamp::read_strip(&unstamped.color_rgba8, resolution).is_none());
    }

    fn image_sink_with_one_user(directory: &std::path::Path) -> HeadsetSink {
        let options = frame_sink::ImageSinkOptions {
            output_directory: directory.to_owned(),
            resolution_in_pixel: [4, 2],
            depth_range: DepthRange::new(0.1, 10.0, xr_math::DepthEncoding::ForwardNormalized)
                .unwrap(),
            write_every_nth_frame: 1,
            frames_per_user: 2,
            write_raw_depth: false,
        };
        let user = frame_sink::SimulatedUser {
            label: "only".to_owned(),
            joins_at_tick: 0,
            pose_source: Box::new(frame_sink::ScriptedHeadMotion::new(
                frame_sink::HeadMotion::Still {
                    position: glam::Vec3::new(0.0, 1.6, 0.0),
                    yaw: 0.0,
                },
            )),
        };
        let sink = frame_sink::ImageSink::new(
            options,
            vec![user],
            Box::new(frame_sink::MaxUsers { maximum: 2 }),
        )
        .unwrap();
        HeadsetSink::Cpu(Box::new(sink))
    }

    #[test]
    fn a_view_that_cannot_be_placed_begins_no_frame() {
        let directory = std::env::temp_dir().join(format!(
            "qaros-rerun-placeable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut sink = image_sink_with_one_user(&directory);

        let refused = begin_placeable_frames(Err("left-handed (RUF)".to_owned()), &mut sink);
        assert_eq!(
            refused.err().as_deref(),
            Some("not streaming this view: left-handed (RUF)")
        );

        let world_center = glam::Vec3::new(1.0, 2.0, 3.0);
        let placement = AppPlacement {
            world_center,
            ..AppPlacement::default()
        };
        let (begun_placement, pending) =
            begin_placeable_frames(Ok(placement), &mut sink).expect("placeable");
        assert_eq!(begun_placement.world_center, world_center);
        let keys: Vec<FrameKey> = pending.iter().map(|request| request.request.key).collect();
        assert_eq!(
            keys,
            [FrameKey {
                user: UserId(1),
                frame_id: 0
            }],
            "the refused view left the user's frame unbegun"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Counts `update_users` calls; begins nothing.
    struct UserUpdateCounter {
        updates: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl FrameSink for UserUpdateCounter {
        fn begin_frames(&mut self) -> Result<Vec<FrameRequest>, SinkError> {
            Ok(Vec::new())
        }

        fn update_users(&mut self) {
            self.updates.set(self.updates.get() + 1);
        }

        fn submit_view(&mut self, key: FrameKey, _: usize, _: EyeImage) -> Result<(), SinkError> {
            Err(SinkError::UnknownFrame(key))
        }

        fn show_frame(&mut self, key: FrameKey, _: ShowInfo) -> Result<(), SinkError> {
            Err(SinkError::UnknownFrame(key))
        }

        fn abandon_frame(&mut self, _: FrameKey) {}

        fn is_streaming(&self) -> bool {
            false
        }

        fn depth_range(&self) -> DepthRange {
            DepthRange::new(0.05, 20.0, xr_math::DepthEncoding::ForwardNormalized).unwrap()
        }
    }

    #[test]
    fn users_are_updated_once_per_viewer_frame_without_a_drawn_3d_view() {
        let updates = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut sink = HeadsetSink::Cpu(Box::new(UserUpdateCounter {
            updates: std::rc::Rc::clone(&updates),
        }));
        let mut headset = renderer(false, sink.depth_range());

        headset.update_users_unless_begun(&mut sink);
        headset.update_users_unless_begun(&mut sink);
        assert_eq!(
            updates.get(),
            2,
            "one update per viewer frame without a 3D view"
        );

        headset.sink_began_during_ui = true;
        headset.update_users_unless_begun(&mut sink);
        assert_eq!(
            updates.get(),
            2,
            "begin_frames updated the users in the pass"
        );
        headset.update_users_unless_begun(&mut sink);
        assert_eq!(
            updates.get(),
            3,
            "the next frame without a 3D view updates again"
        );
    }

    #[test]
    fn summary_reports_each_user_separately() {
        let mut statistics = HeadsetStatistics::default();
        let started = Instant::now();
        for user in [UserId(1), UserId(2)] {
            for _ in 0..3 {
                statistics
                    .per_user
                    .entry(user)
                    .or_default()
                    .record_show(started, 1);
            }
        }
        let summary = statistics.summary();
        assert!(summary.contains("user-1: shown 3"), "{summary}");
        assert!(summary.contains("user-2: shown 3"), "{summary}");
        assert!(
            summary.contains("1.00 viewer frames on average"),
            "{summary}"
        );

        statistics.frames_counted_in_eye_frames = true;
        let decoupled = statistics.summary();
        assert!(
            decoupled.contains("1.00 eye frames on average") && !decoupled.contains("viewer"),
            "{decoupled}"
        );
    }
}
