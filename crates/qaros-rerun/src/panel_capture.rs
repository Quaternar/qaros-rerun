//! 2D views as QAROS panels, route B: the viewer's own pixels for each non-3D view, cropped
//! from a window screenshot and served by the panel server (`--panel-route web`), or handed to
//! the stream panels' video senders (`--panel-route stream`).
//!
//! This is the on-screen variant: egui's `Context::request_screenshot` captures the whole
//! window, and each view's rectangle comes from Rerun's `ViewRectPublisher` (the same source
//! Rerun's "screenshot view" command uses). It covers every view type, time series and bar
//! charts included, and follows the desktop time cursor by construction. Its limit is that a
//! view must be visible on the desktop to reach a panel.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use panel_server::{PanelServer, TimelinePosition, TimelineRequests, ViewFrames};
#[cfg(feature = "qaros")]
use qaros_sink::{PanelGestures, PanelImages};
use re_viewer_context::{PublishedViewInfo, ViewId, ViewRectPublisher};

/// The largest side of a streamed view, in pixels; a larger crop is halved until it fits.
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub const STREAM_PIXEL_CAP: u32 = 1280;

/// QAROS issue #343 test: every mixer `VulkanDeviceLost` on 3177 came as a mixer connected
/// one of our stream panel streams, at 1016x665 and 960x256. The mixer's panel copy has no
/// size-dependent arithmetic, so aligned sizes are not expected to avoid the fault by
/// themselves; they rule out a size mismatch between the exported destination and its alias.
/// Every stream panel image is padded to these multiples; setting both to 1 turns it off.
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub const MIXER_WIDTH_ALIGNMENT: u32 = 64;
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub const MIXER_HEIGHT_ALIGNMENT: u32 = 8;

/// Where the captured views go.
pub trait PanelPublisher {
    fn publish_view(
        &self,
        name: &str,
        width: u32,
        height: u32,
        rgba8: Vec<u8>,
    ) -> Result<(), String>;
}

/// The web route: a PNG per view on the panel server.
impl PanelPublisher for ViewFrames {
    fn publish_view(
        &self,
        name: &str,
        width: u32,
        height: u32,
        rgba8: Vec<u8>,
    ) -> Result<(), String> {
        self.publish_rgba8(name, width, height, &rgba8)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// The stream route: BGRA images for the video senders, at most [`STREAM_PIXEL_CAP`] a side.
#[cfg(feature = "qaros")]
impl PanelPublisher for PanelImages {
    fn publish_view(
        &self,
        name: &str,
        width: u32,
        height: u32,
        rgba8: Vec<u8>,
    ) -> Result<(), String> {
        let (width, height, rgba8) = fit_within_cap(width, height, rgba8, STREAM_PIXEL_CAP);
        publish_padded_for_mixer(self, name, width, height, rgba8)
    }
}

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

/// What the QAROS panel hook reads the published views from, and on the stream route where
/// the timeline panel's gestures arrive.
#[derive(Clone)]
pub enum PanelOutlet {
    Web(ViewFrames),
    #[cfg(feature = "qaros")]
    Stream {
        images: PanelImages,
        gestures: PanelGestures,
    },
}

/// Views seen by the 3D observer this frame, so panel capture can skip them.
#[derive(Default)]
pub struct ViewDirectory {
    pub views_3d: HashSet<ViewId>,
    pub all_views: Vec<ViewId>,
}

pub type SharedViewDirectory = Arc<Mutex<ViewDirectory>>;

pub struct PanelCapture {
    /// The web route's panel server; `None` on the stream route.
    server: Option<PanelServer>,
    outlet: PanelOutlet,
    /// The panel server's on the web route; the viewer's own on the stream route, where the
    /// timeline panel is drawn by the viewer.
    timeline_requests: TimelineRequests,
    timeline_position: TimelinePosition,
    capture_interval: Duration,
    last_request: Option<Instant>,
    arrived_screenshots: Arc<Mutex<Vec<Arc<egui::ColorImage>>>>,
    view_directory: SharedViewDirectory,
    announced_views: HashSet<String>,
    panel_names: PanelNames,
}

impl PanelCapture {
    pub fn new(
        server: PanelServer,
        capture_interval: Duration,
        view_directory: SharedViewDirectory,
    ) -> Self {
        let outlet = PanelOutlet::Web(server.frames().clone());
        let timeline = (
            server.timeline_requests().clone(),
            server.timeline_position().clone(),
        );
        Self::with_outlet(
            Some(server),
            outlet,
            timeline,
            capture_interval,
            view_directory,
        )
    }

    /// The stream route: no panel server; the views go to `images`, and the timeline panel's
    /// gestures arrive in `gestures`.
    #[cfg(feature = "qaros")]
    pub fn streaming(
        images: PanelImages,
        gestures: PanelGestures,
        capture_interval: Duration,
        view_directory: SharedViewDirectory,
    ) -> Self {
        Self::with_outlet(
            None,
            PanelOutlet::Stream { images, gestures },
            (TimelineRequests::default(), TimelinePosition::default()),
            capture_interval,
            view_directory,
        )
    }

    fn with_outlet(
        server: Option<PanelServer>,
        outlet: PanelOutlet,
        (timeline_requests, timeline_position): (TimelineRequests, TimelinePosition),
        capture_interval: Duration,
        view_directory: SharedViewDirectory,
    ) -> Self {
        Self {
            server,
            outlet,
            timeline_requests,
            timeline_position,
            capture_interval,
            last_request: None,
            arrived_screenshots: Arc::default(),
            view_directory,
            announced_views: HashSet::new(),
            panel_names: PanelNames::default(),
        }
    }

    /// The published views, shared with the HTTP thread or the QAROS thread.
    pub fn outlet(&self) -> PanelOutlet {
        self.outlet.clone()
    }

    /// The timeline panel's requests and where the viewer's time cursor stands, shared with
    /// the HTTP thread on the web route and with the viewer's own timeline panel on the stream
    /// route.
    pub fn timeline_link(&self) -> (TimelineRequests, TimelinePosition) {
        (
            self.timeline_requests.clone(),
            self.timeline_position.clone(),
        )
    }

    fn publisher(&self) -> &dyn PanelPublisher {
        match &self.outlet {
            PanelOutlet::Web(frames) => frames,
            #[cfg(feature = "qaros")]
            PanelOutlet::Stream { images, .. } => images,
        }
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
        self.panel_names.assign(&on_screen_views);
        for (view_id, view_info) in on_screen_views {
            let Some(panel_name) = self.panel_names.name_of(view_id).map(str::to_owned) else {
                continue;
            };
            let Some((width, height, rgba8)) =
                crop_rgba8(screenshot, view_info.rect.shrink(2.5), pixels_per_point)
            else {
                continue;
            };
            if let Err(error) = self
                .publisher()
                .publish_view(&panel_name, width, height, rgba8)
            {
                re_log::warn_once!("panels: cannot publish {panel_name}: {error}");
                continue;
            }
            if !self.announced_views.insert(panel_name.clone()) {
                continue;
            }
            if let Some(url) = self
                .server
                .as_ref()
                .and_then(|server| server.view_url(&panel_name))
            {
                re_log::info!("panels: {:?} at {url}", view_info.name);
            } else {
                re_log::info!(
                    "panels: {:?} published as {panel_name} at {width}x{height}",
                    view_info.name
                );
            }
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

/// Halves `rgba8` with a 2x2 box filter until neither side exceeds `cap`. An odd last row or
/// column is dropped at each halving.
#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub fn fit_within_cap(
    mut width: u32,
    mut height: u32,
    mut rgba8: Vec<u8>,
    cap: u32,
) -> (u32, u32, Vec<u8>) {
    while (width > cap || height > cap) && width >= 2 && height >= 2 {
        let (half_width, half_height) = (width / 2, height / 2);
        rgba8 = halve_rgba8(width, &rgba8, half_width, half_height);
        width = half_width;
        height = half_height;
    }
    (width, height, rgba8)
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
    fn a_2600_pixel_crop_is_halved_twice_to_650_with_the_box_averages() {
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
        let (fitted_width, fitted_height, fitted) =
            fit_within_cap(2600, 4, rgba8, STREAM_PIXEL_CAP);
        assert_eq!((fitted_width, fitted_height), (650, 1));
        assert_eq!(fitted.len(), 650 * 4);
        assert!(
            fitted
                .chunks_exact(4)
                .all(|pixel| pixel == [50, 20, 7, 255]),
            "{:?}",
            &fitted[..8]
        );
        let (kept_width, kept_height, kept) =
            fit_within_cap(1280, 720, vec![9; 1280 * 720 * 4], 1280);
        assert_eq!(
            (kept_width, kept_height, kept.len()),
            (1280, 720, 1280 * 720 * 4)
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

    #[test]
    fn a_capture_is_due_at_once_then_one_interval_after_it_is_taken() {
        let server = PanelServer::start("127.0.0.1:0").unwrap();
        let interval = Duration::from_mins(1);
        let mut capture = PanelCapture::new(server, interval, SharedViewDirectory::default());
        assert_eq!(capture.until_due(), Duration::ZERO);
        assert!(capture.take_due());
        let until_due = capture.until_due();
        assert!(
            (Duration::from_secs(59)..=interval).contains(&until_due),
            "{until_due:?}"
        );
        assert!(!capture.take_due());
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
