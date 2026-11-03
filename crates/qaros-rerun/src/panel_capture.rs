//! 2D views as QAROS GUI panels: the viewer's own pixels for each non-3D view, cropped from a
//! window screenshot and handed to the panels' video senders.
//!
//! This is the on-screen variant: egui's `Context::request_screenshot` captures the whole
//! window, and each view's rectangle comes from Rerun's `ViewRectPublisher` (the same source
//! Rerun's "screenshot view" command uses). It covers every view type, time series and bar
//! charts included, and follows the desktop time cursor by construction. Its limit is that a
//! view must be visible on the desktop to reach a panel.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[cfg(feature = "qaros")]
use qaros_sink::{PanelGestures, PanelImages};
use re_viewer_context::{PublishedViewInfo, ViewId, ViewRectPublisher};

use crate::hidden_window::is_minimised;
use crate::timeline_requests::{TimelinePosition, TimelineRequests};

/// The largest side of a streamed view, in pixels; a larger crop is halved until it fits.
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub const STREAM_PIXEL_CAP: u32 = 1280;

/// How long a view's crop keeps one size before its stream frame is fixed from it. A desktop
/// resize changes the crop at every capture, so a size held this long is the layout at rest.
pub const STREAM_FRAME_SETTLE: Duration = Duration::from_secs(1);

/// QAROS issue #343 test: every mixer `VulkanDeviceLost` on 3177 came as a mixer connected
/// one of our stream panel streams, at 1016x665 and 960x256. The mixer's panel copy has no
/// size-dependent arithmetic, so aligned sizes are not expected to avoid the fault by
/// themselves; they rule out a size mismatch between the exported destination and its alias.
/// Every stream panel image is padded to these multiples; setting both to 1 turns it off.
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub const MIXER_WIDTH_ALIGNMENT: u32 = 64;
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub const MIXER_HEIGHT_ALIGNMENT: u32 = 8;

/// Publishes a stream panel image padded by [`pad_for_mixer`], and logs the padded size once
/// per name and size. The panel's metres follow the padded image, so its content area keeps
/// the padded aspect and no scroll bar appears.
#[cfg(feature = "qaros")]
pub fn publish_padded_for_mixer(
    images: &PanelImages,
    name: &str,
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
) -> Result<(), String> {
    let (padded_width, padded_height, padded) = pad_for_mixer(width, height, rgba8);
    if (padded_width, padded_height) != (width, height) {
        re_log::info_once!(
            "panels: {name} at {width}x{height} is padded to {padded_width}x{padded_height} for the mixer (QAROS #343 test)"
        );
    }
    images
        .publish_rgba8(name, padded_width, padded_height, padded)
        .map(|_| ())
}

/// Pads the image on the right and at the bottom to multiples of [`MIXER_WIDTH_ALIGNMENT`] and
/// [`MIXER_HEIGHT_ALIGNMENT`], without scaling. The padding takes the bottom-right pixel's
/// colour, the view's background in a Rerun view.
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub fn pad_for_mixer(width: u32, height: u32, rgba8: Vec<u8>) -> (u32, u32, Vec<u8>) {
    let padded_width = width.next_multiple_of(MIXER_WIDTH_ALIGNMENT);
    let padded_height = height.next_multiple_of(MIXER_HEIGHT_ALIGNMENT);
    let source_row = width as usize * 4;
    let is_whole_image = width > 0 && height > 0 && rgba8.len() == source_row * height as usize;
    if (padded_width, padded_height) == (width, height) || !is_whole_image {
        return (width, height, rgba8);
    }
    let background: [u8; 4] = rgba8[rgba8.len() - 4..]
        .try_into()
        .unwrap_or([0, 0, 0, u8::MAX]);
    let padded_row = padded_width as usize * 4;
    let mut padded = Vec::with_capacity(padded_row * padded_height as usize);
    for row in rgba8.chunks_exact(source_row) {
        padded.extend_from_slice(row);
        for _ in width..padded_width {
            padded.extend_from_slice(&background);
        }
    }
    for _ in 0..(padded_width as usize * (padded_height - height) as usize) {
        padded.extend_from_slice(&background);
    }
    (padded_width, padded_height, padded)
}

/// What the QAROS panel hook reads the published views from, and where the timeline panel's
/// gestures arrive.
#[cfg(feature = "qaros")]
#[derive(Clone)]
pub struct PanelOutlet {
    pub images: PanelImages,
    pub gestures: PanelGestures,
}

/// Panels need the QAROS sink, so a build without it has no outlet to capture into.
#[cfg(not(feature = "qaros"))]
#[derive(Clone)]
pub enum PanelOutlet {}

/// Views seen by the 3D observer this frame, so panel capture can skip them.
#[derive(Default)]
pub struct ViewDirectory {
    pub views_3d: HashSet<ViewId>,
    pub all_views: Vec<ViewId>,
}

pub type SharedViewDirectory = Arc<Mutex<ViewDirectory>>;

pub struct PanelCapture {
    outlet: PanelOutlet,
    /// The timeline panel's requests and the cursor for its thumb, shared with the viewer's
    /// own timeline panel.
    timeline_requests: TimelineRequests,
    timeline_position: TimelinePosition,
    capture_interval: Duration,
    last_request: Option<Instant>,
    arrived_screenshots: Arc<Mutex<Vec<Arc<egui::ColorImage>>>>,
    view_directory: SharedViewDirectory,
    announced_views: HashSet<String>,
    panel_names: PanelNames,
    stream_frames: StreamFrames,
}

impl PanelCapture {
    pub fn new(
        outlet: PanelOutlet,
        capture_interval: Duration,
        view_directory: SharedViewDirectory,
    ) -> Self {
        Self {
            outlet,
            timeline_requests: TimelineRequests::default(),
            timeline_position: TimelinePosition::default(),
            capture_interval,
            last_request: None,
            arrived_screenshots: Arc::default(),
            view_directory,
            announced_views: HashSet::new(),
            panel_names: PanelNames::default(),
            stream_frames: StreamFrames::default(),
        }
    }

    /// The published views, shared with the QAROS thread.
    pub fn outlet(&self) -> PanelOutlet {
        self.outlet.clone()
    }

    /// The timeline panel's requests and where the viewer's time cursor stands, shared with
    /// the viewer's own timeline panel.
    pub fn timeline_link(&self) -> (TimelineRequests, TimelinePosition) {
        (
            self.timeline_requests.clone(),
            self.timeline_position.clone(),
        )
    }

    /// Publishes the view scaled into its fixed stream frame ([`StreamFrames`]), so its
    /// panel's sender never has to change size. A view whose frame is not fixed yet is not
    /// published; returns the frame it was published in.
    #[cfg(feature = "qaros")]
    fn publish_view(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        rgba8: Vec<u8>,
        now: Instant,
    ) -> Result<Option<StreamFrame>, String> {
        let Some(frame) = self.stream_frames.frame_for(name, width, height, now) else {
            return Ok(None);
        };
        let framed = letterbox_into_frame(width, height, rgba8, frame)?;
        self.outlet
            .images
            .publish_rgba8(name, frame.frame_width, frame.frame_height, framed)?;
        Ok(Some(frame))
    }

    #[cfg(not(feature = "qaros"))]
    fn publish_view(
        &mut self,
        _: &str,
        _: u32,
        _: u32,
        _: Vec<u8>,
        _: Instant,
    ) -> Result<Option<StreamFrame>, String> {
        match self.outlet {}
    }

    #[cfg(feature = "qaros")]
    fn withdraw_views_not_on_screen(&self, on_screen_names: &[String]) -> Vec<String> {
        self.outlet.images.retain_views(on_screen_names)
    }

    #[cfg(not(feature = "qaros"))]
    fn withdraw_views_not_on_screen(&self, _: &[String]) -> Vec<String> {
        match self.outlet {}
    }

    pub fn on_frame(&mut self, egui_ctx: &egui::Context) {
        let screenshots = std::mem::take(
            &mut *self
                .arrived_screenshots
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for screenshot in screenshots {
            self.publish_views(egui_ctx, &screenshot);
        }

        if self.take_due() {
            let arrived_screenshots = Arc::clone(&self.arrived_screenshots);
            let repaint_ctx = egui_ctx.clone();
            // The integration calls back after the frame is read and asks for no new frame, so
            // the callback wakes the UI to publish the views without waiting a capture interval.
            egui_ctx.request_screenshot(move |screenshot| {
                arrived_screenshots
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(screenshot);
                repaint_ctx.request_repaint();
            });
        }
        egui_ctx.request_repaint_after(self.capture_interval);
    }

    /// True when the next capture is due; the headless runner holds the capture itself, renders
    /// a frame through its `PanelRenderer` and calls [`PanelCapture::publish_views`].
    pub fn take_due(&mut self) -> bool {
        let is_due = self
            .last_request
            .is_none_or(|last| last.elapsed() >= self.capture_interval);
        if is_due {
            self.last_request = Some(Instant::now());
        }
        is_due
    }

    /// How long until [`PanelCapture::take_due`] next reports a capture due.
    pub fn until_due(&self) -> Duration {
        self.last_request.map_or(Duration::ZERO, |last| {
            self.capture_interval.saturating_sub(last.elapsed())
        })
    }

    pub fn publish_views(&mut self, egui_ctx: &egui::Context, screenshot: &egui::ColorImage) {
        let panel_views: Vec<ViewId> = {
            let directory = self
                .view_directory
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            directory
                .all_views
                .iter()
                .filter(|view_id| !directory.views_3d.contains(view_id))
                .copied()
                .collect()
        };
        let pixels_per_point = egui_ctx.pixels_per_point();
        let on_screen_views: Vec<(ViewId, PublishedViewInfo)> = panel_views
            .into_iter()
            .filter_map(|view_id| {
                egui_ctx
                    .memory_mut(|memory| {
                        memory
                            .caches
                            .cache::<ViewRectPublisher>()
                            .get(&view_id)
                            .cloned()
                    })
                    .map(|view_info| (view_id, view_info))
            })
            .collect();
        let window_is_minimised = egui_ctx.input(|input| is_minimised(input.viewport()));
        self.publish_on_screen_views(
            on_screen_views,
            screenshot,
            pixels_per_point,
            window_is_minimised,
            Instant::now(),
        );
    }

    /// Publishes the views on screen in this capture, and withdraws those that were published
    /// before and are not on screen now, so their panels follow the layout when another
    /// recording becomes active or the blueprint drops a view. A capture that never happens
    /// withdraws nothing, and neither does a minimised window's capture without a view on
    /// screen: its layout was lost, not its views (ADR-0099).
    fn publish_on_screen_views(
        &mut self,
        on_screen_views: Vec<(ViewId, PublishedViewInfo)>,
        screenshot: &egui::ColorImage,
        pixels_per_point: f32,
        window_is_minimised: bool,
        now: Instant,
    ) {
        if window_is_minimised && on_screen_views.is_empty() {
            return;
        }
        self.panel_names.assign(&on_screen_views);
        let on_screen_names: Vec<String> = on_screen_views
            .iter()
            .filter_map(|(view_id, _)| self.panel_names.name_of(*view_id).map(str::to_owned))
            .collect();
        for withdrawn in self.withdraw_views_not_on_screen(&on_screen_names) {
            self.announced_views.remove(&withdrawn);
            re_log::info!("panels: {withdrawn} left the layout and is no longer published");
        }
        for (view_id, view_info) in on_screen_views {
            let Some(panel_name) = self.panel_names.name_of(view_id).map(str::to_owned) else {
                continue;
            };
            let Some((width, height, rgba8)) =
                crop_rgba8(screenshot, view_info.rect.shrink(2.5), pixels_per_point)
            else {
                continue;
            };
            let frame = match self.publish_view(&panel_name, width, height, rgba8, now) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(error) => {
                    re_log::warn_once!("panels: cannot publish {panel_name}: {error}");
                    continue;
                }
            };
            if !self.announced_views.insert(panel_name.clone()) {
                continue;
            }
            re_log::info!(
                "panels: {:?} published as {panel_name} at {width}x{height} in a {}x{} frame",
                view_info.name,
                frame.frame_width,
                frame.frame_height
            );
        }
    }
}

/// Panel names that stay the same from run to run, so a restarted viewer serves each view at
/// the URL its reattached QAROS panel already shows. A view's id changes on every run, its
/// name and place on screen do not: the name is the view's name reduced to `[a-z0-9_]`, and
/// views sharing one get `-2`, `-3` and so on in screen order, top to bottom, then left to
/// right. A view keeps its name while it stays on screen; a view that leaves frees its name,
/// and gets it back when it returns while the name is still free, so two same-named views that
/// leave and come back in another order do not swap panels within a run.
#[derive(Default)]
pub struct PanelNames {
    by_view: HashMap<ViewId, String>,
    /// The name each view had when it last left the screen, in this run.
    names_before_leaving: HashMap<ViewId, String>,
}

impl PanelNames {
    pub fn assign(&mut self, on_screen_views: &[(ViewId, PublishedViewInfo)]) {
        let is_on_screen = |view_id: &ViewId| {
            on_screen_views
                .iter()
                .any(|(on_screen, _)| on_screen == view_id)
        };
        let (kept, left): (HashMap<ViewId, String>, HashMap<ViewId, String>) =
            std::mem::take(&mut self.by_view)
                .into_iter()
                .partition(|(view_id, _)| is_on_screen(view_id));
        self.by_view = kept;
        self.names_before_leaving.extend(left);
        let mut unnamed: Vec<&(ViewId, PublishedViewInfo)> = on_screen_views
            .iter()
            .filter(|(view_id, _)| !self.by_view.contains_key(view_id))
            .collect();
        unnamed.sort_by(|(_, first), (_, second)| {
            first
                .rect
                .min
                .y
                .total_cmp(&second.rect.min.y)
                .then(first.rect.min.x.total_cmp(&second.rect.min.x))
        });
        let (returning, new): (Vec<_>, Vec<_>) = unnamed
            .into_iter()
            .partition(|(view_id, _)| self.names_before_leaving.contains_key(view_id));
        for (view_id, view_info) in returning {
            let earlier = self.names_before_leaving.remove(view_id);
            let name = earlier
                .filter(|name| !self.is_taken(name))
                .unwrap_or_else(|| self.first_free_name(&readable_panel_name(&view_info.name)));
            self.by_view.insert(*view_id, name);
        }
        for (view_id, view_info) in new {
            let name = self.first_free_name(&readable_panel_name(&view_info.name));
            self.by_view.insert(*view_id, name);
        }
    }

    fn is_taken(&self, candidate: &str) -> bool {
        self.by_view.values().any(|taken| taken == candidate)
    }

    fn first_free_name(&self, readable: &str) -> String {
        if !self.is_taken(readable) {
            return readable.to_owned();
        }
        let mut ordinal = 2;
        loop {
            let candidate = format!("{readable}-{ordinal}");
            if !self.is_taken(&candidate) {
                return candidate;
            }
            ordinal += 1;
        }
    }

    pub fn name_of(&self, view_id: ViewId) -> Option<&str> {
        self.by_view.get(&view_id).map(String::as_str)
    }
}

/// The view's name reduced to `[a-z0-9_]`, at most 40 characters; `view` when nothing is left.
fn readable_panel_name(view_name: &str) -> String {
    let reduced: String = view_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    match reduced.trim_matches('_') {
        "" => "view".to_owned(),
        readable => readable.to_owned(),
    }
}

/// Crops a screenshot to a rectangle given in UI points, clamped to the image.
pub fn crop_rgba8(
    screenshot: &egui::ColorImage,
    rect_in_points: egui::Rect,
    pixels_per_point: f32,
) -> Option<(u32, u32, Vec<u8>)> {
    let [image_width, image_height] = screenshot.size;
    #[allow(clippy::cast_precision_loss)]
    let image_rect = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(image_width as f32, image_height as f32),
    );
    let pixel_rect = egui::Rect::from_min_max(
        (rect_in_points.min.to_vec2() * pixels_per_point).to_pos2(),
        (rect_in_points.max.to_vec2() * pixels_per_point).to_pos2(),
    )
    .intersect(image_rect);
    if !pixel_rect.is_positive() {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let [min_x, min_y, max_x, max_y] = [
        pixel_rect.min.x.round() as usize,
        pixel_rect.min.y.round() as usize,
        pixel_rect.max.x.round() as usize,
        pixel_rect.max.y.round() as usize,
    ];
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    let mut rgba8 = Vec::with_capacity((max_x - min_x) * (max_y - min_y) * 4);
    for row in min_y..max_y {
        let row_pixels = screenshot
            .pixels
            .get(row * image_width + min_x..row * image_width + max_x)?;
        for pixel in row_pixels {
            rgba8.extend_from_slice(&pixel.to_srgba_unmultiplied());
        }
    }
    Some((
        u32::try_from(max_x - min_x).ok()?,
        u32::try_from(max_y - min_y).ok()?,
        rgba8,
    ))
}

/// The fixed size a view streams at for the rest of the run. The content box is the view's
/// settled size, halved until neither side exceeds [`STREAM_PIXEL_CAP`]; the frame is the
/// content box padded to the mixer multiples ([`MIXER_WIDTH_ALIGNMENT`],
/// [`MIXER_HEIGHT_ALIGNMENT`]). Its sender, and the panel's metres, are sized from the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamFrame {
    pub content_width: u32,
    pub content_height: u32,
    pub frame_width: u32,
    pub frame_height: u32,
}

impl StreamFrame {
    pub fn for_settled_view(width: u32, height: u32) -> Self {
        let (mut content_width, mut content_height) = (width.max(1), height.max(1));
        while (content_width > STREAM_PIXEL_CAP || content_height > STREAM_PIXEL_CAP)
            && content_width >= 2
            && content_height >= 2
        {
            content_width /= 2;
            content_height /= 2;
        }
        Self {
            content_width,
            content_height,
            frame_width: content_width.next_multiple_of(MIXER_WIDTH_ALIGNMENT),
            frame_height: content_height.next_multiple_of(MIXER_HEIGHT_ALIGNMENT),
        }
    }

    /// The largest size of `width` by `height`'s aspect that fits the content box, at least
    /// one pixel a side.
    fn fitted_content_size(&self, width: u32, height: u32) -> (u32, u32) {
        let (width, height) = (u64::from(width.max(1)), u64::from(height.max(1)));
        let (box_width, box_height) = (
            u64::from(self.content_width),
            u64::from(self.content_height),
        );
        let (fitted_width, fitted_height) = if width * box_height <= height * box_width {
            ((width * box_height + height / 2) / height, box_height)
        } else {
            (box_width, (height * box_width + width / 2) / width)
        };
        (
            u32::try_from(fitted_width.clamp(1, box_width)).unwrap_or(self.content_width),
            u32::try_from(fitted_height.clamp(1, box_height)).unwrap_or(self.content_height),
        )
    }
}

/// Each view's stream frame, fixed once per run from the first crop size that holds for
/// [`STREAM_FRAME_SETTLE`] and kept for the rest of the run, also while the view is off screen,
/// because its panel keeps its sender (ADR-0082). A desktop resize then changes only how the
/// view is scaled into its frame, never the size its sender streams at.
#[derive(Default)]
pub struct StreamFrames {
    fixed: HashMap<String, StreamFrame>,
    /// A view without a frame yet: the crop size seen last and since when.
    settling: HashMap<String, SettlingSize>,
}

#[derive(Clone, Copy)]
struct SettlingSize {
    width: u32,
    height: u32,
    since: Instant,
}

impl StreamFrames {
    /// The view's frame, fixing it when the view's crop has kept `width` by `height` for
    /// [`STREAM_FRAME_SETTLE`]; `None` while it has not.
    #[cfg_attr(not(feature = "qaros"), allow(dead_code))]
    pub fn frame_for(
        &mut self,
        name: &str,
        width: u32,
        height: u32,
        now: Instant,
    ) -> Option<StreamFrame> {
        if let Some(frame) = self.fixed.get(name) {
            if (width, height) != (frame.content_width, frame.content_height) {
                re_log::debug!(
                    "panels: {name} at {width}x{height} is scaled into its fixed {}x{} stream frame",
                    frame.frame_width,
                    frame.frame_height
                );
            }
            return Some(*frame);
        }
        let settling = self
            .settling
            .entry(name.to_owned())
            .or_insert(SettlingSize {
                width,
                height,
                since: now,
            });
        if (settling.width, settling.height) != (width, height) {
            *settling = SettlingSize {
                width,
                height,
                since: now,
            };
        }
        if now.saturating_duration_since(settling.since) < STREAM_FRAME_SETTLE {
            return None;
        }
        self.settling.remove(name);
        let frame = StreamFrame::for_settled_view(width, height);
        re_log::info!(
            "panels: {name} streams in a fixed {}x{} frame for this run, from its settled \
             {width}x{height} crop; a later size is scaled into it",
            frame.frame_width,
            frame.frame_height
        );
        self.fixed.insert(name.to_owned(), frame);
        Some(frame)
    }
}

/// Scales `rgba8`, keeping its aspect, to the largest size that fits the frame's content box,
/// and centres it in the frame. The rest of the frame takes the bottom-right pixel's colour,
/// the view's background in a Rerun view, as the mixer padding does: the panel images are
/// opaque (`PanelImages` stores alpha 255), so the bars cannot be transparent. A crop of the
/// settled size is copied unscaled. Shrinking first halves with a 2x2 box filter while the
/// crop is at least twice the target, then bilinear sampling covers the rest, so text in a
/// shrunken plot does not alias.
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub fn letterbox_into_frame(
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
    frame: StreamFrame,
) -> Result<Vec<u8>, String> {
    let expected_length = width as usize * height as usize * 4;
    if width == 0 || height == 0 || rgba8.len() != expected_length {
        return Err(format!("{} bytes for a {width}x{height} crop", rgba8.len()));
    }
    let background: [u8; 4] = rgba8[expected_length - 4..]
        .try_into()
        .map_err(|_| "no bottom-right pixel".to_owned())?;
    let (target_width, target_height) = frame.fitted_content_size(width, height);
    let (mut source_width, mut source_height, mut source) = (width, height, rgba8);
    while source_width >= 2 * target_width && source_height >= 2 * target_height {
        let (half_width, half_height) = (source_width / 2, source_height / 2);
        source = halve_rgba8(source_width, &source, half_width, half_height);
        source_width = half_width;
        source_height = half_height;
    }
    let content = if (source_width, source_height) == (target_width, target_height) {
        source
    } else {
        resample_bilinear(
            (source_width, source_height),
            &source,
            (target_width, target_height),
        )
    };
    let frame_row = frame.frame_width as usize * 4;
    let mut framed = background.repeat(frame.frame_width as usize * frame.frame_height as usize);
    let left = (frame.frame_width - target_width) as usize / 2;
    let top = (frame.frame_height - target_height) as usize / 2;
    let content_row = target_width as usize * 4;
    for (row, content_pixels) in content.chunks_exact(content_row).enumerate() {
        let start = (top + row) * frame_row + left * 4;
        framed[start..start + content_row].copy_from_slice(content_pixels);
    }
    Ok(framed)
}

/// One source sample per output column or row: the two neighbours and the second one's weight
/// out of 256, pixel centres aligned.
fn bilinear_taps(source_length: u32, target_length: u32) -> Vec<(usize, usize, u32)> {
    let last = source_length as usize - 1;
    #[allow(clippy::cast_precision_loss)]
    let step = source_length as f32 / target_length as f32;
    (0..target_length)
        .map(|target| {
            #[allow(clippy::cast_precision_loss)]
            let position = ((target as f32 + 0.5) * step - 0.5).clamp(0.0, last as f32);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let first = position.floor() as usize;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let second_weight = ((position - position.floor()) * 256.0).round() as u32;
            (first, (first + 1).min(last), second_weight)
        })
        .collect()
}

/// Separable: every source row is resampled across once, into weighted sums out of 256 (at most
/// 255 * 256, which fits a `u16`), then each target row blends two of those.
fn resample_bilinear(
    (source_width, source_height): (u32, u32),
    source: &[u8],
    (target_width, target_height): (u32, u32),
) -> Vec<u8> {
    let column_taps = bilinear_taps(source_width, target_width);
    let row_taps = bilinear_taps(source_height, target_height);
    let target_row = target_width as usize * 4;
    let mut widened_rows: Vec<u16> = Vec::with_capacity(target_row * source_height as usize);
    for source_pixels in source.chunks_exact(source_width as usize * 4) {
        for &(left_column, right_column, right_weight) in &column_taps {
            let left_pixel = &source_pixels[left_column * 4..][..4];
            let right_pixel = &source_pixels[right_column * 4..][..4];
            for (&left, &right) in left_pixel.iter().zip(right_pixel) {
                let widened =
                    u32::from(left) * (256 - right_weight) + u32::from(right) * right_weight;
                #[allow(clippy::cast_possible_truncation)]
                widened_rows.push(widened as u16);
            }
        }
    }
    let mut resampled = Vec::with_capacity(target_row * target_height as usize);
    for &(upper_row, lower_row, lower_weight) in &row_taps {
        let upper = &widened_rows[upper_row * target_row..][..target_row];
        let lower = &widened_rows[lower_row * target_row..][..target_row];
        resampled.extend(upper.iter().zip(lower).map(|(&upper_sum, &lower_sum)| {
            let blended =
                u32::from(upper_sum) * (256 - lower_weight) + u32::from(lower_sum) * lower_weight;
            #[allow(clippy::cast_possible_truncation)]
            let rounded = ((blended + (1 << 15)) >> 16) as u8;
            rounded
        }));
    }
    resampled
}

#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
fn halve_rgba8(width: u32, rgba8: &[u8], half_width: u32, half_height: u32) -> Vec<u8> {
    let source_row = width as usize * 4;
    let mut halved = Vec::with_capacity(half_width as usize * half_height as usize * 4);
    for row in 0..half_height as usize {
        let upper = &rgba8[2 * row * source_row..];
        let lower = &rgba8[(2 * row + 1) * source_row..];
        for column in 0..half_width as usize {
            let left = 2 * column * 4;
            for channel in 0..4 {
                let sum = u16::from(upper[left + channel])
                    + u16::from(upper[left + 4 + channel])
                    + u16::from(lower[left + channel])
                    + u16::from(lower[left + 4 + channel]);
                #[allow(clippy::cast_possible_truncation)]
                halved.push(((sum + 2) / 4) as u8);
            }
        }
    }
    halved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_is_padded_to_the_mixer_multiples_with_its_bottom_right_colour() {
        let (width, height) = (1016, 665);
        let mut rgba8 = vec![10_u8; width as usize * height as usize * 4];
        let last = rgba8.len() - 4;
        rgba8[last..].copy_from_slice(&[1, 2, 3, 255]);

        let (padded_width, padded_height, padded) = pad_for_mixer(width, height, rgba8);

        assert_eq!((padded_width, padded_height), (1024, 672));
        assert_eq!(padded.len(), 1024 * 672 * 4);
        let pixel = |x: usize, y: usize| &padded[(y * 1024 + x) * 4..][..4];
        assert_eq!(
            pixel(0, 0),
            [10, 10, 10, 10],
            "the image stays where it was"
        );
        assert_eq!(pixel(1015, 0), [10, 10, 10, 10]);
        assert_eq!(pixel(1016, 0), [1, 2, 3, 255], "padding on the right");
        assert_eq!(pixel(0, 665), [1, 2, 3, 255], "padding at the bottom");
        assert_eq!(pixel(1023, 671), [1, 2, 3, 255]);
    }

    #[test]
    fn an_image_already_on_the_mixer_multiples_is_kept_as_it_is() {
        let rgba8 = vec![7_u8; 960 * 256 * 4];
        let (width, height, kept) = pad_for_mixer(960, 256, rgba8.clone());
        assert_eq!((width, height), (960, 256));
        assert_eq!(kept, rgba8);
    }

    #[test]
    fn a_2600_pixel_crop_is_halved_twice_to_650_with_the_box_averages_and_centred() {
        let width = 2600;
        let height = 4;
        // Columns alternate 0 and 100 in red; rows alternate 0 and 40 in green.
        let mut rgba8 = Vec::with_capacity(width * height * 4);
        for row in 0..height {
            for column in 0..width {
                let red = if column % 2 == 0 { 0 } else { 100 };
                let green = if row % 2 == 0 { 0 } else { 40 };
                rgba8.extend_from_slice(&[red, green, 7, 255]);
            }
        }
        let frame = StreamFrame::for_settled_view(2600, 4);
        assert_eq!(
            frame,
            StreamFrame {
                content_width: 650,
                content_height: 1,
                frame_width: 704,
                frame_height: 8,
            }
        );

        let framed = letterbox_into_frame(2600, 4, rgba8, frame).unwrap();

        assert_eq!(framed.len(), 704 * 8 * 4);
        let pixel = |x: usize, y: usize| &framed[(y * 704 + x) * 4..][..4];
        let (left, top) = ((704 - 650) / 2, (8 - 1) / 2);
        assert_eq!((left, top), (27, 3));
        assert!(
            (left..left + 650).all(|x| pixel(x, top) == [50, 20, 7, 255]),
            "{:?}",
            pixel(left, top)
        );
        let bottom_right_of_the_crop = [100, 40, 7, 255];
        assert_eq!(pixel(left - 1, top), bottom_right_of_the_crop);
        assert_eq!(pixel(left + 650, top), bottom_right_of_the_crop);
        assert_eq!(pixel(left, top - 1), bottom_right_of_the_crop);
        assert_eq!(pixel(703, 7), bottom_right_of_the_crop);
    }

    #[test]
    fn a_settled_crop_within_the_cap_keeps_its_size_padded_for_the_mixer() {
        assert_eq!(
            StreamFrame::for_settled_view(572, 301),
            StreamFrame {
                content_width: 572,
                content_height: 301,
                frame_width: 576,
                frame_height: 304,
            }
        );
        assert_eq!(
            StreamFrame::for_settled_view(1280, 720),
            StreamFrame {
                content_width: 1280,
                content_height: 720,
                frame_width: 1280,
                frame_height: 720,
            }
        );
    }

    /// A `width` by `height` crop whose every pixel is `[x, y, 9, 255]`.
    fn coordinate_crop(width: u32, height: u32) -> Vec<u8> {
        (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    [
                        u8::try_from(x).unwrap(),
                        u8::try_from(y).unwrap(),
                        9,
                        u8::MAX,
                    ]
                })
            })
            .collect()
    }

    #[test]
    fn a_crop_of_the_settled_size_is_copied_unscaled_into_the_frame_centre() {
        let frame = StreamFrame::for_settled_view(60, 5);
        assert_eq!((frame.frame_width, frame.frame_height), (64, 8));

        let framed = letterbox_into_frame(60, 5, coordinate_crop(60, 5), frame).unwrap();

        let pixel = |x: usize, y: usize| &framed[(y * 64 + x) * 4..][..4];
        assert_eq!(pixel(2, 1), [0, 0, 9, 255], "the crop's top-left pixel");
        assert_eq!(
            pixel(61, 5),
            [59, 4, 9, 255],
            "the crop's bottom-right pixel"
        );
        assert_eq!(pixel(33, 3), [31, 2, 9, 255]);
        assert_eq!(pixel(1, 1), [59, 4, 9, 255], "bar left");
        assert_eq!(pixel(62, 1), [59, 4, 9, 255], "bar right");
        assert_eq!(pixel(2, 0), [59, 4, 9, 255], "bar above");
        assert_eq!(pixel(2, 6), [59, 4, 9, 255], "bar below");
    }

    #[test]
    fn a_crop_of_another_aspect_is_letterboxed_at_the_largest_size_that_fits() {
        // Settled wide; the window then makes the view square.
        let frame = StreamFrame::for_settled_view(8, 4);
        assert_eq!((frame.frame_width, frame.frame_height), (64, 8));
        let framed = letterbox_into_frame(4, 4, coordinate_crop(4, 4), frame).unwrap();

        let pixel = |x: usize, y: usize| &framed[(y * 64 + x) * 4..][..4];
        let (left, top) = ((64 - 4) / 2, (8 - 4) / 2);
        assert_eq!((left, top), (30, 2));
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(
                    pixel(left + x, top + y),
                    [u8::try_from(x).unwrap(), u8::try_from(y).unwrap(), 9, 255]
                );
            }
        }
        assert_eq!(pixel(left - 1, top), [3, 3, 9, 255], "bar left");
        assert_eq!(pixel(left + 4, top + 3), [3, 3, 9, 255], "bar right");
        assert_eq!(pixel(0, 0), [3, 3, 9, 255]);
    }

    #[test]
    fn a_larger_crop_is_box_halved_and_a_smaller_one_sampled_up_bilinearly() {
        let frame = StreamFrame::for_settled_view(4, 2);
        let grown = coordinate_crop(8, 4);
        let shrunk_into_the_frame = letterbox_into_frame(8, 4, grown, frame).unwrap();
        let pixel = |framed: &[u8], x: usize, y: usize| framed[(y * 64 + x) * 4..][..4].to_vec();
        let (left, top) = ((64 - 4) / 2, (8 - 2) / 2);
        // A 2x2 box over columns 2 and 3 and rows 2 and 3 averages to 2.5, rounded up.
        assert_eq!(
            pixel(&shrunk_into_the_frame, left + 1, top + 1),
            [3, 3, 9, 255]
        );
        assert_eq!(pixel(&shrunk_into_the_frame, left, top), [1, 1, 9, 255]);

        let black_then_white = vec![0, 0, 0, 255, 255, 255, 255, 255];
        let stretched = letterbox_into_frame(2, 1, black_then_white, frame).unwrap();
        let reds: Vec<u8> = (0..4)
            .map(|x| pixel(&stretched, left + x, top)[0])
            .collect();
        assert_eq!(reds, [0, 64, 191, 255]);
        assert_eq!(
            pixel(&stretched, left + 1, top + 1)[0],
            64,
            "both rows sample row 0"
        );
    }

    #[test]
    fn a_crop_whose_bytes_do_not_match_its_size_is_refused() {
        let frame = StreamFrame::for_settled_view(4, 2);
        assert!(letterbox_into_frame(4, 2, vec![0; 12], frame).is_err());
        assert!(letterbox_into_frame(0, 2, Vec::new(), frame).is_err());
    }

    #[test]
    fn a_frame_is_fixed_once_the_crop_size_holds_for_the_settle_time_and_kept_after() {
        let mut frames = StreamFrames::default();
        let start = Instant::now();
        let at = |millis: u64| start + Duration::from_millis(millis);
        assert_eq!(frames.frame_for("camera", 441, 238, at(0)), None);
        assert_eq!(
            frames.frame_for("camera", 572, 301, at(250)),
            None,
            "a new size starts the settle time again"
        );
        assert_eq!(frames.frame_for("camera", 572, 301, at(1_249)), None);
        let fixed = frames.frame_for("camera", 572, 301, at(1_250));
        assert_eq!(fixed, Some(StreamFrame::for_settled_view(572, 301)));

        for (width, height) in [(666, 317), (783, 450), (729, 382), (635, 315), (899, 470)] {
            assert_eq!(frames.frame_for("camera", width, height, at(1_500)), fixed);
        }
        assert_eq!(
            frames.frame_for("plot", 899, 470, at(1_500)),
            None,
            "each view settles on its own"
        );
    }

    fn checker_image() -> egui::ColorImage {
        let mut image = egui::ColorImage::filled([8, 6], egui::Color32::BLACK);
        for row in 2..4 {
            for column in 4..8 {
                image.pixels[row * 8 + column] = egui::Color32::RED;
            }
        }
        image
    }

    #[cfg(feature = "qaros")]
    #[test]
    fn a_capture_is_due_at_once_then_one_interval_after_it_is_taken() {
        let outlet = PanelOutlet {
            images: PanelImages::default(),
            gestures: PanelGestures::default(),
        };
        let interval = Duration::from_mins(1);
        let mut capture = PanelCapture::new(outlet, interval, SharedViewDirectory::default());
        assert_eq!(capture.until_due(), Duration::ZERO);
        assert!(capture.take_due());
        let until_due = capture.until_due();
        assert!(
            (Duration::from_secs(59)..=interval).contains(&until_due),
            "{until_due:?}"
        );
        assert!(!capture.take_due());
    }

    #[cfg(feature = "qaros")]
    fn capture_into(images: &PanelImages) -> PanelCapture {
        let outlet = PanelOutlet {
            images: images.clone(),
            gestures: PanelGestures::default(),
        };
        PanelCapture::new(
            outlet,
            Duration::from_secs(1),
            SharedViewDirectory::default(),
        )
    }

    /// Two captures of `layout` a settle time apart, so every view in it is published.
    #[cfg(feature = "qaros")]
    fn publish_settled(
        capture: &mut PanelCapture,
        layout: &[(ViewId, PublishedViewInfo)],
        screenshot: &egui::ColorImage,
        start: Instant,
    ) {
        capture.publish_on_screen_views(layout.to_vec(), screenshot, 1.0, false, start);
        capture.publish_on_screen_views(
            layout.to_vec(),
            screenshot,
            1.0,
            false,
            start + STREAM_FRAME_SETTLE,
        );
    }

    /// The window resize of QAROS 3184 (log of 2026-11-02 12:55:32 on): the camera view's crop
    /// sizes at successive captures, in pixels.
    const RESIZE_DRAG: [(f32, f32); 7] = [
        (666.0, 317.0),
        (783.0, 450.0),
        (729.0, 382.0),
        (635.0, 315.0),
        (834.0, 456.0),
        (899.0, 470.0),
        (441.0, 238.0),
    ];

    #[cfg(feature = "qaros")]
    #[test]
    fn a_desktop_resize_keeps_the_published_size_and_scales_the_view_into_it() {
        let images = PanelImages::default();
        let mut capture = capture_into(&images);
        let mut screenshot = egui::ColorImage::filled([1000, 600], egui::Color32::BLACK);
        let camera_id = ViewId::random();
        let camera_of_size = |width: f32, height: f32| {
            // The capture shrinks each view's rectangle by 2.5 points a side.
            let rect =
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width + 5.0, height + 5.0));
            vec![(
                camera_id,
                PublishedViewInfo {
                    name: "Camera".to_owned(),
                    rect,
                },
            )]
        };
        let start = Instant::now();
        publish_settled(
            &mut capture,
            &camera_of_size(572.0, 301.0),
            &screenshot,
            start,
        );
        let settled = images.latest("camera").unwrap();
        assert_eq!((settled.width, settled.height), (576, 304));

        let mut captured_at = start + STREAM_FRAME_SETTLE;
        for (step, (width, height)) in RESIZE_DRAG.into_iter().enumerate() {
            let shade = u8::try_from(step + 1).unwrap() * 20;
            screenshot.pixels.fill(egui::Color32::from_rgb(shade, 0, 0));
            captured_at += Duration::from_millis(250);
            capture.publish_on_screen_views(
                camera_of_size(width, height),
                &screenshot,
                1.0,
                false,
                captured_at,
            );
            let published = images.latest("camera").unwrap();
            assert_eq!(
                (published.width, published.height),
                (576, 304),
                "at {width}x{height}"
            );
            assert_eq!(
                published.bgra8[(152 * 576 + 288) * 4 + 2],
                shade,
                "the frame centre shows this capture's pixels"
            );
        }
        assert_eq!(
            images.latest("camera").unwrap().sequence,
            u64::try_from(RESIZE_DRAG.len()).unwrap(),
            "one new image per capture, all of the one size"
        );
    }

    #[cfg(feature = "qaros")]
    #[test]
    fn a_recording_switch_withdraws_the_views_that_left_and_publishes_the_new_ones() {
        let images = PanelImages::default();
        let mut capture = capture_into(&images);
        images
            .publish_rgba8(qaros_sink::TIMELINE_IMAGE_KEY, 1, 1, vec![0; 4])
            .unwrap();
        let screenshot = egui::ColorImage::filled([400, 400], egui::Color32::BLACK);
        let demo_layout = vec![view_at("Camera", 0.0, 0.0), view_at("Plot", 200.0, 0.0)];
        let start = Instant::now();
        publish_settled(&mut capture, &demo_layout, &screenshot, start);
        assert_eq!(images.view_names(), ["camera", "plot"]);

        // The opened recording's blueprint: new view ids, a plot of the same name, no camera.
        let opened_layout = vec![view_at("Plot", 0.0, 0.0), view_at("Image", 200.0, 0.0)];
        publish_settled(
            &mut capture,
            &opened_layout,
            &screenshot,
            start + Duration::from_secs(5),
        );

        assert_eq!(images.view_names(), ["image", "plot"]);
        assert!(images.latest("camera").is_none());
        assert!(
            images.latest(qaros_sink::TIMELINE_IMAGE_KEY).is_some(),
            "the timeline is no view and stays"
        );
    }

    #[cfg(feature = "qaros")]
    #[test]
    fn a_capture_with_no_view_on_screen_withdraws_every_view() {
        let images = PanelImages::default();
        let mut capture = capture_into(&images);
        let screenshot = egui::ColorImage::filled([400, 400], egui::Color32::BLACK);
        let start = Instant::now();
        publish_settled(
            &mut capture,
            &[view_at("Camera", 0.0, 0.0)],
            &screenshot,
            start,
        );
        assert_eq!(images.view_names(), ["camera"]);

        capture.publish_on_screen_views(
            Vec::new(),
            &screenshot,
            1.0,
            false,
            start + Duration::from_secs(5),
        );

        assert!(images.view_names().is_empty());
    }

    #[cfg(feature = "qaros")]
    #[test]
    fn a_minimised_capture_with_no_view_on_screen_keeps_every_view() {
        let images = PanelImages::default();
        let mut capture = capture_into(&images);
        let screenshot = egui::ColorImage::filled([400, 400], egui::Color32::BLACK);
        let start = Instant::now();
        publish_settled(
            &mut capture,
            &[view_at("Camera", 0.0, 0.0)],
            &screenshot,
            start,
        );

        capture.publish_on_screen_views(
            Vec::new(),
            &screenshot,
            1.0,
            true,
            start + Duration::from_secs(5),
        );

        assert_eq!(images.view_names(), ["camera"]);
    }

    #[test]
    fn crops_in_points_with_pixel_scaling() {
        let image = checker_image();
        let rect = egui::Rect::from_min_max(egui::pos2(2.0, 1.0), egui::pos2(4.0, 2.0));
        let (width, height, rgba8) = crop_rgba8(&image, rect, 2.0).unwrap();
        assert_eq!((width, height), (4, 2));
        assert!(rgba8.chunks_exact(4).all(|pixel| pixel == [255, 0, 0, 255]));
    }

    #[test]
    fn clamps_to_the_image_and_rejects_empty_crops() {
        let image = checker_image();
        let overhanging = egui::Rect::from_min_max(egui::pos2(6.0, 4.0), egui::pos2(20.0, 20.0));
        let (width, height, _) = crop_rgba8(&image, overhanging, 1.0).unwrap();
        assert_eq!((width, height), (2, 2));
        let outside = egui::Rect::from_min_max(egui::pos2(30.0, 30.0), egui::pos2(40.0, 40.0));
        assert!(crop_rgba8(&image, outside, 1.0).is_none());
    }

    fn view_at(name: &str, left: f32, top: f32) -> (ViewId, PublishedViewInfo) {
        (
            ViewId::random(),
            PublishedViewInfo {
                name: name.to_owned(),
                rect: egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(100.0, 80.0)),
            },
        )
    }

    /// The same layout with fresh view ids, listed in the given order, as a run sees it.
    fn names_of_one_run(layout: &[(&str, f32, f32)]) -> Vec<String> {
        let views: Vec<(ViewId, PublishedViewInfo)> = layout
            .iter()
            .map(|(name, left, top)| view_at(name, *left, *top))
            .collect();
        let mut names = PanelNames::default();
        names.assign(&views);
        views
            .iter()
            .map(|(view_id, _)| names.name_of(*view_id).expect("named").to_owned())
            .collect()
    }

    #[test]
    fn panel_names_are_the_same_on_every_run_and_distinct_within_one() {
        let layout = [
            ("Time series / speed", 0.0, 0.0),
            ("Camera", 400.0, 300.0),
            ("Camera", 0.0, 300.0),
        ];
        let expected = ["time_series___speed", "camera-2", "camera"];
        assert_eq!(names_of_one_run(&layout), expected);
        assert_eq!(names_of_one_run(&layout), expected);
        // The view directory comes from a HashMap, so the listing order differs between runs.
        let reversed: Vec<(&str, f32, f32)> = layout.iter().rev().copied().collect();
        assert_eq!(
            names_of_one_run(&reversed),
            ["camera", "camera-2", "time_series___speed"]
        );
    }

    #[test]
    fn a_view_keeps_its_name_while_on_screen_and_frees_it_when_gone() {
        let upper = view_at("Plot", 0.0, 0.0);
        let lower = view_at("Plot", 0.0, 300.0);
        let mut names = PanelNames::default();
        names.assign(&[upper.clone(), lower.clone()]);
        assert_eq!(names.name_of(upper.0), Some("plot"));
        assert_eq!(names.name_of(lower.0), Some("plot-2"));

        let moved_above = (upper.0, view_at("Plot", 0.0, 600.0).1);
        names.assign(&[moved_above, lower.clone()]);
        assert_eq!(
            names.name_of(upper.0),
            Some("plot"),
            "moving keeps the name"
        );

        let replacement = view_at("Plot", 0.0, 0.0);
        names.assign(&[lower.clone(), replacement.clone()]);
        assert_eq!(names.name_of(upper.0), None);
        assert_eq!(names.name_of(lower.0), Some("plot-2"));
        assert_eq!(names.name_of(replacement.0), Some("plot"));
    }

    #[test]
    fn same_named_views_that_leave_and_return_in_another_order_keep_their_names() {
        let upper = view_at("Plot", 0.0, 0.0);
        let lower = view_at("Plot", 0.0, 300.0);
        let mut names = PanelNames::default();
        names.assign(&[upper.clone(), lower.clone()]);
        names.assign(&[]);

        names.assign(std::slice::from_ref(&lower));
        assert_eq!(
            names.name_of(lower.0),
            Some("plot-2"),
            "back alone, it does not take the upper view's name"
        );
        names.assign(&[lower.clone(), upper.clone()]);
        assert_eq!(names.name_of(upper.0), Some("plot"));
        assert_eq!(names.name_of(lower.0), Some("plot-2"));
    }

    #[test]
    fn panel_names_are_url_safe() {
        assert_eq!(
            readable_panel_name("Time series / speed"),
            "time_series___speed"
        );
        assert_eq!(readable_panel_name("/// ///"), "view");
        assert_eq!(readable_panel_name(&"Ä".repeat(60)), "view");
        assert_eq!(readable_panel_name(&"a".repeat(60)), "a".repeat(40));
    }
}
