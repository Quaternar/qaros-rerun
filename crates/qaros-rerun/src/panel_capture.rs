//! 2D views as QAROS panels, route B: the viewer's own pixels for each non-3D view, cropped
//! from a window screenshot and served by the panel server.
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
use re_viewer_context::{PublishedViewInfo, ViewId, ViewRectPublisher};

/// Views seen by the 3D observer this frame, so panel capture can skip them.
#[derive(Default)]
pub struct ViewDirectory {
    pub views_3d: HashSet<ViewId>,
    pub all_views: Vec<ViewId>,
}

pub type SharedViewDirectory = Arc<Mutex<ViewDirectory>>;

pub struct PanelCapture {
    server: PanelServer,
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
        Self {
            server,
            capture_interval,
            last_request: None,
            arrived_screenshots: Arc::default(),
            view_directory,
            announced_views: HashSet::new(),
            panel_names: PanelNames::default(),
        }
    }

    /// The published views, shared with the HTTP thread.
    pub fn frames(&self) -> ViewFrames {
        self.server.frames().clone()
    }

    /// The timeline panel's slider requests, shared with the HTTP thread.
    pub fn timeline_requests(&self) -> TimelineRequests {
        self.server.timeline_requests().clone()
    }

    /// Where the viewer's time cursor stands, read by the HTTP thread for the pages.
    pub fn timeline_position(&self) -> TimelinePosition {
        self.server.timeline_position().clone()
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
            if let Err(error) =
                self.server
                    .frames()
                    .publish_rgba8(&panel_name, width, height, &rgba8)
            {
                re_log::warn_once!("panels: cannot publish {panel_name}: {error}");
                continue;
            }
            if self.announced_views.insert(panel_name.clone())
                && let Some(url) = self.server.view_url(&panel_name)
            {
                re_log::info!("panels: {:?} at {url}", view_info.name);
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

#[cfg(test)]
mod tests {
    use super::*;

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
