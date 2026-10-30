//! The timeline as a stream panel (spec 01g, section 3): a private egui context in Rerun's style,
//! drawn off screen into [`TIMELINE_RASTER`] and published under [`TIMELINE_IMAGE_KEY`], driven
//! by the panel's gesture events.
//!
//! Gestures are hit-tested against the pixel rectangles of [`TimelineLayout`], the same ones the
//! UI pass draws, instead of being replayed as egui pointer events, so the mapping is
//! deterministic and tested without a GPU. Positions are in the sender's pixels, top-left
//! origin, clamped to the image (`QarGuiPanelPoint`, header L1180-1197).
//!
//! The mixer decides tap or drag: a drag that does not move within about 150 ms arrives as a
//! tap, which seeks to the touched point and ends there (ADR-0072). Nothing here can change
//! that window; a tap on the track and a drag that ends where it started do the same.

use std::time::{Duration, Instant};

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Shape, Stroke, ViewportId, vec2};
use frame_sink::perf;
use panel_server::{TimelinePosition, TimelineRequest, TimelineRequests};
use qaros_sink::qaros::PeerId;
use qaros_sink::{
    GesturePhase, PanelGesture, PanelGestureKind, PanelGestures, PanelImages, TIMELINE_IMAGE_KEY,
};
use rerun::external::re_viewer::external::re_ui;

use crate::panel_capture::publish_padded_for_mixer;
use crate::panel_render::OffscreenEguiRenderer;
use crate::timeline_scrub::TimelineSource;

/// The timeline panel's image in pixels: the 0.45 m x 0.12 m panel's aspect (3.75).
pub const TIMELINE_RASTER: [u32; 2] = [960, 256];
const PIXELS_PER_POINT: f32 = 2.0;

/// A pointer owner with no event for three of the mixer's 250 ms republish intervals has lost
/// its lifecycle (header L4351-4356).
const SILENT_LIFECYCLE: Duration = Duration::from_millis(750);
/// After a release the thumb stays at the finger while the viewer catches up (ADR-0072).
const RELEASE_HOLD: Duration = Duration::from_secs(1);
/// 15 Hz: faster renders show nothing a finger can follow and cost the UI thread.
const RENDER_INTERVAL: Duration = Duration::from_micros(66_667);
/// The UI thread's share of one render: the UI pass, the tessellation and the submit. The GPU
/// work and the readback are collected on a later viewer frame, never waited for.
const RENDER_BUDGET: Duration = Duration::from_millis(4);
const MARGIN: f32 = 24.0;
const BUTTON_SIDE: f32 = 208.0;
const GAP: f32 = 32.0;
const THUMB_RADIUS: f32 = 36.0;
const BAR_HALF_HEIGHT: f32 = 12.0;
const BAR_CENTER_Y: f32 = 168.0;
const LABEL_CENTER_Y: f32 = 76.0;
const LABEL_FONT_PIXELS: f32 = 40.0;

/// A rectangle in the raster's pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl PixelRect {
    #[allow(
        clippy::cast_precision_loss,
        reason = "pixel coordinates of a 960 px raster"
    )]
    pub fn contains(&self, [x, y]: [i32; 2]) -> bool {
        (self.left..=self.right).contains(&(x as f32))
            && (self.top..=self.bottom).contains(&(y as f32))
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    fn in_points(&self) -> Rect {
        Rect::from_min_max(
            Pos2::new(self.left, self.top) / PIXELS_PER_POINT,
            Pos2::new(self.right, self.bottom) / PIXELS_PER_POINT,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineTarget {
    Button,
    Track,
}

/// Where the controls sit in the raster's pixels: a Play/Pause button a finger can hit and a
/// track whose touch area is the full control height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineLayout {
    pub button: PixelRect,
    /// The span the thumb's centre travels, 0 at its left and 1 at its right.
    pub track: PixelRect,
    /// The track plus the thumb's reach to each side and the full control height.
    pub track_touch: PixelRect,
}

impl TimelineLayout {
    #[allow(clippy::cast_precision_loss, reason = "a 960 x 256 raster")]
    pub fn for_raster([width, _height]: [u32; 2]) -> Self {
        let control_top = MARGIN;
        let control_bottom = MARGIN + BUTTON_SIDE;
        let button = PixelRect {
            left: MARGIN,
            top: control_top,
            right: MARGIN + BUTTON_SIDE,
            bottom: control_bottom,
        };
        let track_touch = PixelRect {
            left: button.right + GAP,
            top: control_top,
            right: width as f32 - MARGIN,
            bottom: control_bottom,
        };
        let track = PixelRect {
            left: track_touch.left + THUMB_RADIUS,
            top: BAR_CENTER_Y - BAR_HALF_HEIGHT,
            right: track_touch.right - THUMB_RADIUS,
            bottom: BAR_CENTER_Y + BAR_HALF_HEIGHT,
        };
        Self {
            button,
            track,
            track_touch,
        }
    }

    pub fn target_at(&self, position: [i32; 2]) -> Option<TimelineTarget> {
        if self.button.contains(position) {
            Some(TimelineTarget::Button)
        } else if self.track_touch.contains(position) {
            Some(TimelineTarget::Track)
        } else {
            None
        }
    }

    /// `(x - track.left) / track.width`, clamped to `[0, 1]`.
    pub fn fraction_at(&self, x: i32) -> f64 {
        ((f64::from(x) - f64::from(self.track.left)) / f64::from(self.track.width()))
            .clamp(0.0, 1.0)
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "a pixel column of a 960 px raster"
    )]
    pub fn thumb_column(&self, fraction: f64) -> i32 {
        (f64::from(self.track.left) + fraction.clamp(0.0, 1.0) * f64::from(self.track.width()))
            .round() as i32
    }
}

/// A gesture lifecycle: the gesture id is unique per source peer only (header L4338-4350).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lifecycle {
    peer: PeerId,
    gesture_id: u64,
}

impl Lifecycle {
    fn of(gesture: &PanelGesture) -> Self {
        Self {
            peer: gesture.source_peer,
            gesture_id: gesture.gesture_id,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PointerOwner {
    lifecycle: Lifecycle,
    fraction: f64,
    last_event: Instant,
}

/// Who owns the pointer, and the fraction the thumb holds after a release.
#[derive(Default)]
pub struct TimelineGestureState {
    owner: Option<PointerOwner>,
    held: Option<(f64, Instant)>,
}

impl TimelineGestureState {
    /// The requests one gesture asks for. `playing` decides what the button sends.
    pub fn handle(
        &mut self,
        gesture: &PanelGesture,
        layout: &TimelineLayout,
        playing: bool,
    ) -> Vec<TimelineRequest> {
        match (gesture.kind, gesture.phase) {
            (PanelGestureKind::Tap, GesturePhase::Instant) => {
                self.handle_tap(gesture, layout, playing)
            }
            (PanelGestureKind::Drag, _) => self.handle_drag(gesture, layout),
            _ => Vec::new(),
        }
    }

    /// Taps carry gesture id 0 and are never owned (header L4419-4426), so while a drag owns
    /// the pointer they are gated on the source peer.
    fn handle_tap(
        &mut self,
        gesture: &PanelGesture,
        layout: &TimelineLayout,
        playing: bool,
    ) -> Vec<TimelineRequest> {
        if self
            .owner
            .is_some_and(|owner| owner.lifecycle.peer != gesture.source_peer)
        {
            return Vec::new();
        }
        match layout.target_at(gesture.position) {
            Some(TimelineTarget::Track) => {
                let fraction = layout.fraction_at(gesture.position[0]);
                self.held = Some((fraction, gesture.received));
                vec![TimelineRequest::SeekAndPause { fraction }]
            }
            Some(TimelineTarget::Button) if playing => vec![TimelineRequest::Pause],
            Some(TimelineTarget::Button) => vec![TimelineRequest::Play],
            None => Vec::new(),
        }
    }

    fn handle_drag(
        &mut self,
        gesture: &PanelGesture,
        layout: &TimelineLayout,
    ) -> Vec<TimelineRequest> {
        let lifecycle = Lifecycle::of(gesture);
        let fraction = layout.fraction_at(gesture.position[0]);
        match self.owner {
            Some(owner) if owner.lifecycle != lifecycle => Vec::new(),
            Some(_) => self.continue_owned_drag(lifecycle, gesture, fraction),
            None => self.start_drag(lifecycle, gesture, layout, fraction),
        }
    }

    fn continue_owned_drag(
        &mut self,
        lifecycle: Lifecycle,
        gesture: &PanelGesture,
        fraction: f64,
    ) -> Vec<TimelineRequest> {
        match gesture.phase {
            GesturePhase::Started | GesturePhase::Updated => {
                self.owner = Some(PointerOwner {
                    lifecycle,
                    fraction,
                    last_event: gesture.received,
                });
                vec![TimelineRequest::SeekAndPause { fraction }]
            }
            // A live lifecycle follows the clamped pixel to the edge, so an overshoot commits
            // the end value (header L4466-4470).
            GesturePhase::Ended | GesturePhase::Canceled => {
                self.release(fraction, gesture.received);
                vec![TimelineRequest::SeekAndPause { fraction }]
            }
            GesturePhase::Instant => Vec::new(),
        }
    }

    /// A drag takes the pointer only from the track. An update with no start seen starts from
    /// the start position every update carries (header L4357-4358).
    fn start_drag(
        &mut self,
        lifecycle: Lifecycle,
        gesture: &PanelGesture,
        layout: &TimelineLayout,
        fraction: f64,
    ) -> Vec<TimelineRequest> {
        if layout.target_at(gesture.start_position) != Some(TimelineTarget::Track) {
            return Vec::new();
        }
        let start_fraction = layout.fraction_at(gesture.start_position[0]);
        match gesture.phase {
            GesturePhase::Started | GesturePhase::Updated => {
                self.owner = Some(PointerOwner {
                    lifecycle,
                    fraction,
                    last_event: gesture.received,
                });
                vec![
                    TimelineRequest::SeekAndPause {
                        fraction: start_fraction,
                    },
                    TimelineRequest::SeekAndPause { fraction },
                ]
            }
            GesturePhase::Ended | GesturePhase::Canceled => {
                self.held = Some((fraction, gesture.received));
                vec![TimelineRequest::SeekAndPause { fraction }]
            }
            GesturePhase::Instant => Vec::new(),
        }
    }

    fn release(&mut self, fraction: f64, at: Instant) {
        self.owner = None;
        self.held = Some((fraction, at));
    }

    /// An owner silent for [`SILENT_LIFECYCLE`] counts as cancelled; its last seek stands.
    pub fn release_silent_owner(&mut self, now: Instant) {
        if let Some(owner) = self.owner
            && now.saturating_duration_since(owner.last_event) >= SILENT_LIFECYCLE
        {
            self.release(owner.fraction, now);
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.owner.is_some()
    }

    /// The finger's fraction while the pointer is owned, the released fraction for a second
    /// after, and then the viewer's.
    pub fn thumb(&self, now: Instant, published: Option<f64>) -> Option<f64> {
        if let Some(owner) = self.owner {
            return Some(owner.fraction);
        }
        match self.held {
            Some((fraction, since)) if now.saturating_duration_since(since) < RELEASE_HOLD => {
                Some(fraction)
            }
            _ => published,
        }
    }
}

/// What one image shows; a new image is drawn only when this changes. An unchanged image is
/// shown again by the sink's refresh, so it is not drawn again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimelineView {
    thumb_column: Option<i32>,
    playing: bool,
    has_timeline: bool,
    dragging: bool,
}

pub struct TimelinePanel {
    images: PanelImages,
    gestures: PanelGestures,
    requests: TimelineRequests,
    position: TimelinePosition,
    layout: TimelineLayout,
    gesture_state: TimelineGestureState,
    context: egui::Context,
    renderer: Option<OffscreenEguiRenderer>,
    created: Instant,
    last_render: Option<Instant>,
    last_view: Option<TimelineView>,
    /// When the oldest gesture the next image answers arrived, for finger-to-image timing.
    unanswered_gesture: Option<Instant>,
    /// The same for the render in flight, recorded once its image is published.
    gesture_in_flight: Option<Instant>,
}

impl TimelinePanel {
    pub fn new(
        images: PanelImages,
        gestures: PanelGestures,
        requests: TimelineRequests,
        position: TimelinePosition,
    ) -> Self {
        let context = egui::Context::default();
        re_ui::apply_style_and_install_loaders(&context);
        Self {
            images,
            gestures,
            requests,
            position,
            layout: TimelineLayout::for_raster(TIMELINE_RASTER),
            gesture_state: TimelineGestureState::default(),
            context,
            renderer: None,
            created: Instant::now(),
            last_render: None,
            last_view: None,
            unanswered_gesture: None,
            gesture_in_flight: None,
        }
    }

    /// Once per viewer frame on the UI thread, before `TimelineScrub::apply`: publishes the
    /// image of a render the GPU has finished, drains the gestures, pushes their requests, and
    /// submits a new render when one is due and none is in flight. It never waits for the GPU.
    pub fn on_logic(
        &mut self,
        viewer: &impl TimelineSource,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        now: Instant,
    ) {
        self.publish_finished_render();
        let Some(view) = self.update(viewer, now) else {
            return;
        };
        let started = Instant::now();
        let submitted = self.submit_render(device, queue, format, view, now);
        let took = started.elapsed();
        perf::record("timeline_render", None, took.as_micros());
        if took > RENDER_BUDGET {
            re_log::warn_once!(
                "panels: a timeline render took {took:?} on the UI thread, over its {RENDER_BUDGET:?} budget"
            );
        }
        match submitted {
            Ok(()) => self.gesture_in_flight = self.unanswered_gesture.take(),
            Err(error) => re_log::warn_once!("panels: the timeline render failed: {error}"),
        }
    }

    fn publish_finished_render(&mut self) {
        let Some(collected) = self
            .renderer
            .as_mut()
            .and_then(OffscreenEguiRenderer::try_collect)
        else {
            return;
        };
        self.publish_collected(collected);
    }

    fn publish_collected(&mut self, collected: Result<image::RgbaImage, String>) {
        let published = collected.and_then(|image| {
            let (width, height) = image.dimensions();
            publish_padded_for_mixer(
                &self.images,
                TIMELINE_IMAGE_KEY,
                width,
                height,
                image.into_raw(),
            )
        });
        match published {
            Ok(()) => {
                if let Some(received) = self.gesture_in_flight.take() {
                    perf::record(
                        "timeline_finger_to_image",
                        None,
                        received.elapsed().as_micros(),
                    );
                }
            }
            Err(error) => re_log::warn_once!("panels: the timeline render failed: {error}"),
        }
    }

    /// Waits for the render in flight and publishes it; tests have no later frame for it.
    #[cfg(test)]
    pub fn wait_for_render(&mut self) {
        if let Some(collected) = self
            .renderer
            .as_mut()
            .and_then(OffscreenEguiRenderer::wait_and_collect)
        {
            self.publish_collected(collected);
        }
    }

    /// The input half of [`Self::on_logic`], without the GPU: the view to draw, when due.
    fn update(&mut self, viewer: &impl TimelineSource, now: Instant) -> Option<TimelineView> {
        let timeline = viewer.active_timeline();
        let published = timeline.as_ref().and_then(|_| self.position.latest());
        let playing = timeline.as_ref().is_some_and(|timeline| timeline.playing);

        let gestures = self.gestures.take();
        if timeline.is_none() {
            if !gestures.is_empty() {
                re_log::debug!(
                    "panels: no timeline for {} timeline gesture(s)",
                    gestures.len()
                );
            }
        } else {
            self.apply_gestures(&gestures, playing);
        }
        self.gesture_state.release_silent_owner(now);

        let view = TimelineView {
            thumb_column: timeline.as_ref().and_then(|_| {
                self.gesture_state
                    .thumb(now, published)
                    .map(|fraction| self.layout.thumb_column(fraction))
            }),
            playing,
            has_timeline: timeline.is_some(),
            dragging: self.gesture_state.is_dragging(),
        };
        self.is_render_due(view, now).then(|| {
            self.last_render = Some(now);
            self.last_view = Some(view);
            view
        })
    }

    fn apply_gestures(&mut self, gestures: &[PanelGesture], playing: bool) {
        for gesture in gestures {
            let requests = self.gesture_state.handle(gesture, &self.layout, playing);
            if requests.is_empty() {
                continue;
            }
            self.unanswered_gesture.get_or_insert(gesture.received);
            for request in requests {
                self.requests.push(request);
            }
        }
    }

    fn is_render_due(&self, view: TimelineView, now: Instant) -> bool {
        if self
            .renderer
            .as_ref()
            .is_some_and(OffscreenEguiRenderer::is_busy)
        {
            return false;
        }
        let Some(last_render) = self.last_render else {
            return true;
        };
        now.saturating_duration_since(last_render) >= RENDER_INTERVAL
            && self.last_view != Some(view)
    }

    fn submit_render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        view: TimelineView,
        now: Instant,
    ) -> Result<(), String> {
        if self
            .renderer
            .as_ref()
            .is_some_and(|renderer| renderer.format() != format)
        {
            self.renderer = None;
        }
        let renderer = self
            .renderer
            .get_or_insert_with(|| OffscreenEguiRenderer::new(device, queue, format));
        submit_timeline(
            &self.context,
            renderer,
            &self.layout,
            view,
            now.saturating_duration_since(self.created),
        )
    }
}

fn submit_timeline(
    context: &egui::Context,
    renderer: &mut OffscreenEguiRenderer,
    layout: &TimelineLayout,
    view: TimelineView,
    elapsed: Duration,
) -> Result<(), String> {
    #[allow(clippy::cast_precision_loss, reason = "a 960 x 256 raster")]
    let size_in_points =
        vec2(TIMELINE_RASTER[0] as f32, TIMELINE_RASTER[1] as f32) / PIXELS_PER_POINT;
    let mut input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size_in_points)),
        time: Some(elapsed.as_secs_f64()),
        ..Default::default()
    };
    input
        .viewports
        .entry(ViewportId::ROOT)
        .or_default()
        .native_pixels_per_point = Some(PIXELS_PER_POINT);
    let output = context.run_ui(input, |ui| draw_timeline(ui, layout, view));
    renderer.submit(context, output)
}

fn draw_timeline(ui: &egui::Ui, layout: &TimelineLayout, view: TimelineView) {
    let visuals = ui.visuals();
    let painter = ui.painter();
    let raster_rect = PixelRect {
        left: 0.0,
        top: 0.0,
        #[allow(clippy::cast_precision_loss, reason = "a 960 x 256 raster")]
        right: TIMELINE_RASTER[0] as f32,
        #[allow(clippy::cast_precision_loss, reason = "a 960 x 256 raster")]
        bottom: TIMELINE_RASTER[1] as f32,
    };
    painter.rect_filled(raster_rect.in_points(), 0.0, visuals.panel_fill);

    let content_color = if view.has_timeline {
        visuals.strong_text_color()
    } else {
        visuals.weak_text_color()
    };
    let button = layout.button.in_points();
    let button_fill = if view.has_timeline {
        visuals.widgets.inactive.bg_fill
    } else {
        visuals.widgets.noninteractive.bg_fill
    };
    painter.rect_filled(button, CornerRadius::same(12), button_fill);
    if view.playing {
        draw_pause_glyph(painter, button, content_color);
    } else {
        draw_play_glyph(painter, button, content_color);
    }

    let track = layout.track.in_points();
    let font = FontId::proportional(LABEL_FONT_PIXELS / PIXELS_PER_POINT);
    let label_y = LABEL_CENTER_Y / PIXELS_PER_POINT;
    let status = match (view.has_timeline, view.playing) {
        (false, _) => "no recording",
        (true, true) => "Playing",
        (true, false) => "Paused",
    };
    painter.text(
        Pos2::new(track.left(), label_y),
        Align2::LEFT_CENTER,
        status,
        font.clone(),
        visuals.text_color(),
    );

    painter.rect_filled(
        track,
        CornerRadius::same(6),
        visuals.widgets.inactive.bg_fill,
    );
    let Some(thumb_column) = view.thumb_column else {
        return;
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "a pixel column of a 960 px raster"
    )]
    let thumb_x = thumb_column as f32 / PIXELS_PER_POINT;
    let percent = layout.fraction_at(thumb_column) * 100.0;
    painter.text(
        Pos2::new(track.right(), label_y),
        Align2::RIGHT_CENTER,
        format!("{percent:.0} %"),
        font,
        visuals.text_color(),
    );
    painter.rect_filled(
        Rect::from_min_max(track.min, Pos2::new(thumb_x, track.max.y)),
        CornerRadius::same(6),
        visuals.selection.bg_fill,
    );
    let thumb_radius = THUMB_RADIUS / PIXELS_PER_POINT * if view.dragging { 1.0 } else { 0.8 };
    painter.circle_filled(
        Pos2::new(thumb_x, track.center().y),
        thumb_radius,
        content_color,
    );
}

fn draw_play_glyph(painter: &egui::Painter, button: Rect, color: Color32) {
    let center = button.center();
    let half = button.height() * 0.25;
    painter.add(Shape::convex_polygon(
        vec![
            Pos2::new(center.x - half * 0.7, center.y - half),
            Pos2::new(center.x + half, center.y),
            Pos2::new(center.x - half * 0.7, center.y + half),
        ],
        color,
        Stroke::NONE,
    ));
}

fn draw_pause_glyph(painter: &egui::Painter, button: Rect, color: Color32) {
    let center = button.center();
    let half = button.height() * 0.25;
    let bar_width = half * 0.6;
    for bar_center_x in [center.x - half * 0.55, center.x + half * 0.55] {
        painter.rect_filled(
            Rect::from_center_size(
                Pos2::new(bar_center_x, center.y),
                vec2(bar_width, half * 2.0),
            ),
            CornerRadius::same(2),
            color,
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the layout's pixel values are exact"
)]
mod tests {
    use re_viewer_context::external::re_log_types::{
        AbsoluteTimeRange, StoreId, StoreKind, TimeInt,
    };

    use super::*;
    use crate::timeline_scrub::ActiveTimeline;

    const FIRST_PEER: u8 = 0xa1;
    const SECOND_PEER: u8 = 0xb2;

    fn layout() -> TimelineLayout {
        TimelineLayout::for_raster(TIMELINE_RASTER)
    }

    fn track_x(fraction: f64) -> i32 {
        layout().thumb_column(fraction)
    }

    fn track_point(fraction: f64) -> [i32; 2] {
        [track_x(fraction), BAR_CENTER_Y as i32]
    }

    fn button_point() -> [i32; 2] {
        [(MARGIN + BUTTON_SIDE / 2.0) as i32, 128]
    }

    #[derive(Clone, Copy)]
    struct GestureAt {
        peer: u8,
        gesture_id: u64,
        kind: PanelGestureKind,
        phase: GesturePhase,
        position: [i32; 2],
        start_position: [i32; 2],
        received: Instant,
    }

    fn gesture(at: GestureAt) -> PanelGesture {
        PanelGesture {
            source_peer: PeerId([at.peer; 16]),
            gesture_id: at.gesture_id,
            kind: at.kind,
            phase: at.phase,
            position: at.position,
            start_position: at.start_position,
            tap_count: u8::from(at.kind == PanelGestureKind::Tap),
            received: at.received,
        }
    }

    fn tap(peer: u8, position: [i32; 2], received: Instant) -> PanelGesture {
        gesture(GestureAt {
            peer,
            gesture_id: 0,
            kind: PanelGestureKind::Tap,
            phase: GesturePhase::Instant,
            position,
            start_position: position,
            received,
        })
    }

    fn drag(
        peer: u8,
        gesture_id: u64,
        phase: GesturePhase,
        start_position: [i32; 2],
        position: [i32; 2],
        received: Instant,
    ) -> PanelGesture {
        gesture(GestureAt {
            peer,
            gesture_id,
            kind: PanelGestureKind::Drag,
            phase,
            position,
            start_position,
            received,
        })
    }

    fn seek(fraction: f64) -> TimelineRequest {
        TimelineRequest::SeekAndPause { fraction }
    }

    #[test]
    fn the_layout_keeps_the_button_and_the_track_apart_within_the_raster() {
        let layout = layout();
        assert_eq!(
            layout.target_at(button_point()),
            Some(TimelineTarget::Button)
        );
        assert_eq!(
            layout.target_at(track_point(0.5)),
            Some(TimelineTarget::Track)
        );
        assert_eq!(
            layout.target_at([(layout.button.right + GAP / 2.0) as i32, 128]),
            None,
            "the gap between them is neither"
        );
        assert_eq!(
            layout.target_at([480, 4]),
            None,
            "the top margin is neither"
        );
        assert!(layout.track_touch.right <= 960.0 && layout.button.bottom <= 256.0);
        assert!(
            layout.button.width() >= 200.0,
            "a finger-sized button: {}",
            layout.button.width()
        );
        assert_eq!(layout.track.left, 300.0);
        assert_eq!(layout.track.width(), 600.0);
    }

    #[test]
    fn pixels_map_to_the_track_fraction_clamped_at_its_ends() {
        let layout = layout();
        assert_eq!(layout.fraction_at(300), 0.0);
        assert_eq!(layout.fraction_at(450), 0.25);
        assert_eq!(layout.fraction_at(900), 1.0);
        assert_eq!(
            layout.fraction_at(270),
            0.0,
            "the thumb's reach left of the track"
        );
        assert_eq!(layout.fraction_at(930), 1.0, "and right of it");
        assert_eq!(layout.thumb_column(0.25), 450);
    }

    #[test]
    fn a_tap_on_the_track_seeks_to_its_fraction() {
        let mut state = TimelineGestureState::default();
        let now = Instant::now();
        let requests = state.handle(&tap(FIRST_PEER, [450, 140], now), &layout(), true);
        assert_eq!(requests, [seek(0.25)]);
        assert_eq!(
            state.thumb(now, Some(0.9)),
            Some(0.25),
            "the thumb holds the tap"
        );
    }

    #[test]
    fn a_tap_on_the_button_plays_when_paused_and_pauses_when_playing() {
        let mut state = TimelineGestureState::default();
        let now = Instant::now();
        let paused = state.handle(&tap(FIRST_PEER, button_point(), now), &layout(), false);
        let playing = state.handle(&tap(FIRST_PEER, button_point(), now), &layout(), true);
        assert_eq!(paused, [TimelineRequest::Play]);
        assert_eq!(playing, [TimelineRequest::Pause]);
    }

    #[test]
    fn a_drag_from_the_track_seeks_at_its_start_follows_its_updates_and_commits_its_end() {
        let mut state = TimelineGestureState::default();
        let layout = layout();
        let now = Instant::now();
        let start = track_point(0.1);
        let started = state.handle(
            &drag(
                FIRST_PEER,
                7,
                GesturePhase::Started,
                start,
                track_point(0.2),
                now,
            ),
            &layout,
            false,
        );
        assert_eq!(started, [seek(0.1), seek(0.2)]);
        assert!(state.is_dragging());
        let updated = state.handle(
            &drag(
                FIRST_PEER,
                7,
                GesturePhase::Updated,
                start,
                track_point(0.6),
                now,
            ),
            &layout,
            false,
        );
        assert_eq!(updated, [seek(0.6)]);
        let ended = state.handle(
            &drag(FIRST_PEER, 7, GesturePhase::Ended, start, [959, 160], now),
            &layout,
            false,
        );
        assert_eq!(ended, [seek(1.0)], "an overshoot commits the end value");
        assert!(!state.is_dragging());
    }

    #[test]
    fn a_drag_from_the_button_does_nothing() {
        let mut state = TimelineGestureState::default();
        let layout = layout();
        let now = Instant::now();
        for phase in [
            GesturePhase::Started,
            GesturePhase::Updated,
            GesturePhase::Ended,
        ] {
            let requests = state.handle(
                &drag(FIRST_PEER, 3, phase, button_point(), track_point(0.5), now),
                &layout,
                false,
            );
            assert_eq!(requests, [], "{phase:?}");
        }
        assert!(!state.is_dragging());
    }

    #[test]
    fn while_a_drag_owns_the_pointer_another_peers_tap_and_drag_are_ignored() {
        let mut state = TimelineGestureState::default();
        let layout = layout();
        let now = Instant::now();
        let start = track_point(0.3);
        state.handle(
            &drag(FIRST_PEER, 1, GesturePhase::Started, start, start, now),
            &layout,
            false,
        );

        assert_eq!(
            state.handle(&tap(SECOND_PEER, track_point(0.9), now), &layout, false),
            []
        );
        assert_eq!(
            state.handle(&tap(SECOND_PEER, button_point(), now), &layout, false),
            []
        );
        for phase in [
            GesturePhase::Started,
            GesturePhase::Updated,
            GesturePhase::Ended,
        ] {
            let other = drag(
                SECOND_PEER,
                2,
                phase,
                track_point(0.8),
                track_point(0.8),
                now,
            );
            assert_eq!(state.handle(&other, &layout, false), [], "{phase:?}");
        }
        assert_eq!(
            state.thumb(now, None),
            Some(0.3),
            "the owner's finger stays"
        );
        assert_eq!(
            state.handle(&tap(FIRST_PEER, button_point(), now), &layout, true),
            [TimelineRequest::Pause],
            "the owner's own peer still taps"
        );
    }

    #[test]
    fn the_same_gesture_id_from_two_peers_is_two_lifecycles() {
        let mut state = TimelineGestureState::default();
        let layout = layout();
        let now = Instant::now();
        let start = track_point(0.2);
        state.handle(
            &drag(FIRST_PEER, 5, GesturePhase::Started, start, start, now),
            &layout,
            false,
        );
        let second = drag(
            SECOND_PEER,
            5,
            GesturePhase::Updated,
            start,
            track_point(0.7),
            now,
        );
        assert_eq!(state.handle(&second, &layout, false), []);
        let ending = drag(
            SECOND_PEER,
            5,
            GesturePhase::Ended,
            start,
            track_point(0.7),
            now,
        );
        assert_eq!(state.handle(&ending, &layout, false), []);
        assert!(
            state.is_dragging(),
            "the second peer's end releases nothing"
        );
    }

    #[test]
    fn an_update_with_no_start_seen_starts_from_its_start_position() {
        let mut state = TimelineGestureState::default();
        let now = Instant::now();
        let requests = state.handle(
            &drag(
                FIRST_PEER,
                9,
                GesturePhase::Updated,
                track_point(0.25),
                track_point(0.5),
                now,
            ),
            &layout(),
            false,
        );
        assert_eq!(requests, [seek(0.25), seek(0.5)]);
        assert!(state.is_dragging());
    }

    #[test]
    fn an_owner_silent_for_750_ms_is_released() {
        let mut state = TimelineGestureState::default();
        let layout = layout();
        let started = Instant::now();
        let start = track_point(0.4);
        state.handle(
            &drag(FIRST_PEER, 1, GesturePhase::Started, start, start, started),
            &layout,
            false,
        );
        state.release_silent_owner(started + Duration::from_millis(749));
        assert!(state.is_dragging());
        state.release_silent_owner(started + SILENT_LIFECYCLE);
        assert!(!state.is_dragging());
        let other = drag(
            SECOND_PEER,
            2,
            GesturePhase::Started,
            start,
            track_point(0.9),
            started,
        );
        assert_eq!(
            state.handle(&other, &layout, false),
            [seek(0.4), seek(0.9)],
            "another peer can take the pointer now"
        );
    }

    #[test]
    fn the_thumb_shows_the_finger_holds_a_second_then_follows_the_viewer() {
        let mut state = TimelineGestureState::default();
        let layout = layout();
        let started = Instant::now();
        let start = track_point(0.2);
        assert_eq!(state.thumb(started, Some(0.9)), Some(0.9));
        state.handle(
            &drag(
                FIRST_PEER,
                1,
                GesturePhase::Started,
                start,
                track_point(0.3),
                started,
            ),
            &layout,
            false,
        );
        assert_eq!(state.thumb(started, Some(0.9)), Some(0.3));
        let released = started + Duration::from_millis(200);
        state.handle(
            &drag(
                FIRST_PEER,
                1,
                GesturePhase::Ended,
                start,
                track_point(0.6),
                released,
            ),
            &layout,
            false,
        );
        let held = released + Duration::from_millis(999);
        assert_eq!(state.thumb(held, Some(0.9)), Some(0.6));
        assert_eq!(state.thumb(released + RELEASE_HOLD, Some(0.9)), Some(0.9));
    }

    #[test]
    fn hover_and_long_press_are_ignored() {
        let mut state = TimelineGestureState::default();
        let now = Instant::now();
        for kind in [PanelGestureKind::Hover, PanelGestureKind::LongPress] {
            let event = gesture(GestureAt {
                peer: FIRST_PEER,
                gesture_id: 4,
                kind,
                phase: GesturePhase::Started,
                position: track_point(0.5),
                start_position: track_point(0.5),
                received: now,
            });
            assert_eq!(state.handle(&event, &layout(), false), [], "{kind:?}");
        }
    }

    struct StandInViewer {
        store_id: StoreId,
        playing: bool,
    }

    impl TimelineSource for StandInViewer {
        fn active_timeline(&self) -> Option<ActiveTimeline> {
            Some(ActiveTimeline {
                store_id: self.store_id.clone(),
                range: AbsoluteTimeRange::new(TimeInt::new_temporal(0), TimeInt::new_temporal(100)),
                cursor: TimeInt::new_temporal(0),
                playing: self.playing,
            })
        }
    }

    struct NoTimeline;

    impl TimelineSource for NoTimeline {
        fn active_timeline(&self) -> Option<ActiveTimeline> {
            None
        }
    }

    struct PanelFixture {
        panel: TimelinePanel,
        gestures: PanelGestures,
        requests: TimelineRequests,
        position: TimelinePosition,
        viewer: StandInViewer,
    }

    fn panel_fixture() -> PanelFixture {
        let gestures = PanelGestures::default();
        let requests = TimelineRequests::default();
        let position = TimelinePosition::default();
        PanelFixture {
            panel: TimelinePanel::new(
                PanelImages::default(),
                gestures.clone(),
                requests.clone(),
                position.clone(),
            ),
            gestures,
            requests,
            position,
            viewer: StandInViewer {
                store_id: StoreId::random(StoreKind::Recording, "demo"),
                playing: false,
            },
        }
    }

    #[test]
    fn the_time_control_decides_the_status_and_the_button_from_the_first_frame() {
        let mut fixture = panel_fixture();
        let started = Instant::now();
        fixture.viewer.playing = true;
        fixture.position.publish(0.1);
        let view = fixture.panel.update(&fixture.viewer, started);
        assert!(
            view.is_some_and(|view| view.playing),
            "playing before any button press, with a still cursor"
        );
        fixture
            .gestures
            .push(tap(FIRST_PEER, button_point(), started));
        fixture.panel.update(&fixture.viewer, started);
        assert_eq!(fixture.requests.take(), [TimelineRequest::Pause]);

        fixture.viewer.playing = false;
        fixture.position.publish(0.2);
        let moved = started + Duration::from_millis(100);
        let view = fixture.panel.update(&fixture.viewer, moved);
        assert!(
            view.is_none_or(|view| !view.playing),
            "a moving cursor does not make a paused time control play"
        );
        fixture
            .gestures
            .push(tap(FIRST_PEER, button_point(), moved));
        fixture.panel.update(&fixture.viewer, moved);
        assert_eq!(fixture.requests.take(), [TimelineRequest::Play]);
    }

    #[test]
    fn a_seek_of_ours_is_not_taken_for_playback() {
        let mut fixture = panel_fixture();
        let started = Instant::now();
        fixture.position.publish(0.1);
        fixture.panel.update(&fixture.viewer, started);
        fixture
            .gestures
            .push(tap(FIRST_PEER, track_point(0.5), started));
        fixture.panel.update(&fixture.viewer, started);
        fixture.position.publish(0.5);
        let after_seek = started + Duration::from_millis(100);
        fixture.panel.update(&fixture.viewer, after_seek);
        fixture
            .gestures
            .push(tap(FIRST_PEER, button_point(), after_seek));
        fixture.panel.update(&fixture.viewer, after_seek);
        assert_eq!(fixture.requests.take(), [seek(0.5), TimelineRequest::Play]);
    }

    #[test]
    fn with_no_recording_gestures_are_dropped_and_the_track_is_empty() {
        let mut fixture = panel_fixture();
        let now = Instant::now();
        fixture.position.publish(0.4);
        fixture
            .gestures
            .push(tap(FIRST_PEER, track_point(0.5), now));
        let view = fixture.panel.update(&NoTimeline, now);
        assert_eq!(fixture.requests.take(), []);
        assert_eq!(
            view,
            Some(TimelineView {
                thumb_column: None,
                playing: false,
                has_timeline: false,
                dragging: false,
            })
        );
    }

    #[test]
    fn renders_only_on_a_change_at_most_at_15_hz() {
        let mut fixture = panel_fixture();
        let started = Instant::now();
        fixture.position.publish(0.5);
        assert!(
            fixture.panel.update(&fixture.viewer, started).is_some(),
            "the first"
        );
        let soon = started + Duration::from_millis(20);
        assert!(
            fixture.panel.update(&fixture.viewer, soon).is_none(),
            "unchanged"
        );

        fixture.position.publish(0.6);
        assert!(
            fixture.panel.update(&fixture.viewer, soon).is_none(),
            "changed, but within the 15 Hz cap"
        );
        let capped = started + RENDER_INTERVAL;
        assert!(fixture.panel.update(&fixture.viewer, capped).is_some());

        fixture.position.publish(0.6 + 0.4 / 600.0);
        let below_a_pixel = capped + RENDER_INTERVAL;
        assert!(
            fixture
                .panel
                .update(&fixture.viewer, below_a_pixel)
                .is_none(),
            "a move under one track pixel draws nothing"
        );
        let idle = capped + Duration::from_secs(5);
        assert!(
            fixture.panel.update(&fixture.viewer, idle).is_none(),
            "an unchanged image is not drawn again, however long it stands"
        );
    }

    struct TestDevice {
        device: wgpu::Device,
        queue: wgpu::Queue,
    }

    fn test_device() -> Option<TestDevice> {
        let instance = wgpu::Instance::default();
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            eprintln!("skipped: no wgpu adapter");
            return None;
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("timeline panel test"),
            ..Default::default()
        }))
        .expect("a device on the adapter");
        Some(TestDevice { device, queue })
    }

    /// The middle of the brightest pixels just above the bar, right of the button: the thumb.
    fn thumb_column_in(image: &image::RgbaImage) -> u32 {
        let row = BAR_CENTER_Y as u32 - (BAR_HALF_HEIGHT as u32 + 4);
        let brightness = |column: u32| {
            let pixel = image.get_pixel(column, row);
            u32::from(pixel[0]) + u32::from(pixel[1]) + u32::from(pixel[2])
        };
        let layout = layout();
        let track_columns = layout.track_touch.left as u32..layout.track_touch.right as u32;
        let brightest = track_columns.clone().map(brightness).max().unwrap_or(0);
        let columns: Vec<u32> = track_columns
            .filter(|&column| brightness(column) == brightest)
            .collect();
        (columns.first().unwrap_or(&0) + columns.last().unwrap_or(&0)) / 2
    }

    #[test]
    fn the_timeline_renders_at_its_raster_with_the_thumb_where_the_viewer_stands() {
        let Some(test_device) = test_device() else {
            return;
        };
        let mut fixture = panel_fixture();
        let images = PanelImages::default();
        fixture.panel.images = images.clone();
        let mut columns = Vec::new();
        let mut now = Instant::now();
        for (index, fraction) in [0.2, 0.8].into_iter().enumerate() {
            fixture.position.publish(fraction);
            now += RENDER_INTERVAL;
            let published_before = images
                .latest(TIMELINE_IMAGE_KEY)
                .map(|image| image.sequence);
            let rendered_at = Instant::now();
            fixture.panel.on_logic(
                &fixture.viewer,
                &test_device.device,
                &test_device.queue,
                wgpu::TextureFormat::Bgra8Unorm,
                now,
            );
            eprintln!("render {index} submitted in {:?}", rendered_at.elapsed());
            assert_eq!(
                images
                    .latest(TIMELINE_IMAGE_KEY)
                    .map(|image| image.sequence),
                published_before,
                "the submit publishes nothing; the image comes once the GPU is done"
            );
            fixture.panel.wait_for_render();
            let published = images
                .latest(TIMELINE_IMAGE_KEY)
                .expect("a published image");
            assert_eq!([published.width, published.height], TIMELINE_RASTER);
            let mut rgba = (*published.bgra8).clone();
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            let image = image::RgbaImage::from_raw(published.width, published.height, rgba)
                .expect("a full image");
            if index == 0
                && let Ok(path) = std::env::var("TIMELINE_PANEL_PNG")
            {
                image.save(&path).expect("the png written");
            }
            columns.push(thumb_column_in(&image));
        }
        let expected = [track_x(0.2), track_x(0.8)];
        for (column, expected) in columns.iter().zip(expected) {
            assert!(
                (i64::from(*column) - i64::from(expected)).abs() <= 2,
                "thumb at {column}, expected {expected} ({columns:?})"
            );
        }
    }
}
