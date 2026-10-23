use std::collections::{BTreeMap, BTreeSet};
use std::f32::consts::PI;
use std::rc::Rc;
use std::time::{Duration, Instant};

use glam::{Quat, Vec3};
use qaros::{AppVolumeSize, GuiPanelConfig, GuiPanelId, GuiPanelInfo, GuiPanelSize, GuiPanelState};
use xr_math::Pose;

use crate::connection::{QarosConnection, from_qaros_pose, to_qaros_pose};
use crate::stop_watchdog::{TeardownStep, enter_teardown_step};

/// A 2D view published by the panel server: its URL-safe name and current size in pixels.
/// The name is the same on every run (no `.`, unique within a run): it names the view's QAROS
/// panel and its URL, so a restart finds the panel and leaves its page in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelView {
    pub name: String,
    pub width_in_pixel: u32,
    pub height_in_pixel: u32,
}

const PANEL_WIDTH_METERS: f32 = 0.6;
const PANEL_GAP_METERS: f32 = 0.1;

/// What the viewer's panels become when [`QarosPanels`] drops at Stop. Neither closes them:
/// they stay in the session for the next run to claim by common name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelsAtStop {
    /// `QAR_GUI_PANEL_STATE_HIDDEN`, so no frozen page hangs in the room while the app is
    /// not running; the next run shows them again when it reattaches.
    #[default]
    Hide,
    /// Left as they are, with no panel call at all.
    Keep,
}

/// What becomes of a panel reattached without navigation whose page has asked this run's panel
/// server for nothing [`PAGE_SILENCE_LIMIT`] after the browser was seen running (see
/// [`PanelActivity`]): a page whose document failed to load while no panel server ran has none
/// of the page's script to retry with, so the viewer does not count on it recovering without a
/// navigation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeadPages {
    /// Navigated once to the URL it already shows.
    #[default]
    Reload,
    /// Left as it is, with no navigation.
    Leave,
}

/// How long a reattached panel's page has to ask for its page or a frame, once the browser is
/// seen running, before it counts as dead. A page that loaded asks for a missing frame at least
/// every 4 s (`page.html`).
pub const PAGE_SILENCE_LIMIT: Duration = Duration::from_secs(10);

/// What this run has seen of the browser that shows the panels. A browser that runs no timers
/// (the headset off or asleep, the mixer not rendering panels) asks for nothing from live pages
/// either, so silence counts only once the browser is seen running.
pub trait PanelActivity {
    /// When this run's panel server last got a request for the view's page or one of its
    /// frames, at any time since it started.
    fn last_page_request(&self, view_name: &str) -> Option<Instant>;

    /// Whether any view page has asked this run's panel server for anything: the browser runs
    /// its pages' timers.
    fn any_page_request(&self) -> bool;

    /// Whether a user stream has started in this run: the viewer is watched, so the mixer
    /// renders its panels.
    fn user_stream_started(&self) -> bool;
}

/// One QAROS GUI panel per published 2D view, pointed at the view's page on the panel server
/// and lined up to the right of the app volume, top edges level with the volume's top. When
/// this value drops the panels stay in the session, hidden or kept as [`PanelsAtStop`] says.
pub struct QarosPanels {
    connection: Rc<QarosConnection>,
    base_url: String,
    panels: BTreeMap<String, GuiPanelId>,
    failed: BTreeSet<String>,
    at_stop: PanelsAtStop,
    dead_pages: DeadPages,
    /// Panels reattached without navigation and visible, by view name, with when they were
    /// claimed: their pages have not been seen asking for anything yet.
    kept_pages: BTreeMap<String, Instant>,
    /// When this run first saw the panels' browser running, which starts the silence clock of
    /// the kept pages claimed before.
    browser_seen_running: Option<Instant>,
}

/// A claimed panel, and whether it kept the page an earlier run loaded and is visible.
struct ClaimedPanel {
    id: GuiPanelId,
    kept_visible_page: bool,
}

impl QarosPanels {
    /// `base_url` is how the panel's browser reaches the panel server, for example
    /// `http://127.0.0.1:9877`.
    pub fn new(connection: Rc<QarosConnection>, base_url: &str) -> Self {
        Self {
            connection,
            base_url: base_url.trim_end_matches('/').to_owned(),
            panels: BTreeMap::new(),
            failed: BTreeSet::new(),
            at_stop: PanelsAtStop::default(),
            dead_pages: DeadPages::default(),
            kept_pages: BTreeMap::new(),
            browser_seen_running: None,
        }
    }

    #[must_use]
    pub fn with_dead_pages(mut self, dead_pages: DeadPages) -> Self {
        self.dead_pages = dead_pages;
        self
    }

    #[must_use]
    pub fn with_panels_at_stop(mut self, at_stop: PanelsAtStop) -> Self {
        self.at_stop = at_stop;
        self
    }

    pub fn panels(&self) -> &BTreeMap<String, GuiPanelId> {
        &self.panels
    }

    pub fn view_url(&self, view_name: &str) -> String {
        format!("{}/views/{view_name}", self.base_url)
    }

    /// Opens or reattaches a panel for every settled view not seen before. A view whose panel
    /// cannot be created is logged once and not retried. Once a panel opens, visible panels
    /// that earlier runs left in this app volume are hidden, except those of views still among
    /// `published_view_names`: a view published but not settled yet claims its panel later, and
    /// a view published later still finds its hidden panel by name.
    /// While the panel list cannot be read nothing is opened, so the next sync retries:
    /// without the list a reattached panel would pass for a new one, navigated and left hidden.
    pub fn sync(&mut self, views: &[PanelView], published_view_names: &[String]) {
        let new_views: Vec<&PanelView> = views
            .iter()
            .filter(|view| {
                !self.panels.contains_key(&view.name) && !self.failed.contains(&view.name)
            })
            .collect();
        if new_views.is_empty() {
            return;
        }
        let listed = match self.connection.session().gui_panels().list() {
            Ok(listed) => listed,
            Err(error) => {
                log::warn!("qaros: cannot list panels ({error}); opening none until the next sync");
                return;
            }
        };
        let mut opened_any = false;
        for view in new_views {
            let slot = self.panels.len() + self.failed.len();
            match self.open_panel(view, slot, &listed) {
                Ok(panel) => {
                    if panel.kept_visible_page {
                        self.kept_pages.insert(view.name.clone(), Instant::now());
                    }
                    self.panels.insert(view.name.clone(), panel.id);
                    opened_any = true;
                }
                Err(error) => {
                    log::warn!("qaros: no panel for view {}: {error}", view.name);
                    self.failed.insert(view.name.clone());
                }
            }
        }
        if opened_any {
            self.hide_panels_left_by_earlier_runs(&listed, published_view_names);
        }
    }

    /// Navigates, once, each panel reattached without navigation whose page has asked for
    /// nothing [`PAGE_SILENCE_LIMIT`] after both its claim and the first time this run saw the
    /// browser running. A page with any request since the panel server started is alive and
    /// never navigated. The URL stays the one the panel shows, so the next run finds it
    /// unchanged. With [`DeadPages::Leave`] the page is only logged. Returns the views whose
    /// panels were navigated.
    pub fn reload_dead_pages(
        &mut self,
        activity: &impl PanelActivity,
        now: Instant,
    ) -> Vec<String> {
        self.kept_pages
            .retain(|view_name, _| activity.last_page_request(view_name).is_none());
        if self.browser_seen_running.is_none()
            && (activity.user_stream_started() || activity.any_page_request())
        {
            self.browser_seen_running = Some(now);
        }
        let Some(browser_seen_running) = self.browser_seen_running else {
            return Vec::new();
        };
        let mut reloaded = Vec::new();
        let due: Vec<String> = self
            .kept_pages
            .iter()
            .filter(|(_, claimed)| {
                let silence_started = (**claimed).max(browser_seen_running);
                now.saturating_duration_since(silence_started) >= PAGE_SILENCE_LIMIT
            })
            .map(|(view_name, _)| view_name.clone())
            .collect();
        for view_name in due {
            self.kept_pages.remove(&view_name);
            let Some(panel) = self.panels.get(&view_name).copied() else {
                continue;
            };
            let url = self.view_url(&view_name);
            if self.dead_pages == DeadPages::Leave {
                log::warn!(
                    "qaros: panel {panel} kept its page but asked for nothing {PAGE_SILENCE_LIMIT:?} \
                     after the browser was seen running; leaving it (--dead-panels leave)"
                );
                continue;
            }
            match self
                .connection
                .session()
                .gui_panels()
                .navigate_to_uri(panel, &url)
            {
                Ok(()) => {
                    log::info!(
                        "qaros: panel {panel} kept its page but asked for nothing \
                         {PAGE_SILENCE_LIMIT:?} after the browser was seen running; loading \
                         {url} again"
                    );
                    reloaded.push(view_name);
                }
                Err(error) => log::warn!("qaros: reloading the page of panel {panel}: {error}"),
            }
        }
        reloaded
    }

    /// Navigating a panel makes the mixer replace its web page, so it is done only when
    /// needed. A panel this view claimed in an earlier run already shows its URL when the panel server address is
    /// unchanged, so it is navigated only when its URL differs. A panel an earlier run hid at
    /// Stop is shown again once its page is in place; one a user minimised stays minimised.
    fn open_panel(
        &self,
        view: &PanelView,
        slot: usize,
        listed: &[GuiPanelInfo],
    ) -> qaros::Result<ClaimedPanel> {
        let panels = self.connection.session().gui_panels();
        let config = self.panel_config(view, slot);
        let panel = panels.get_or_create(&config)?;
        // get_or_create returns an existing panel unchanged; pose and size are set explicitly
        // so a panel left over from an earlier run follows this one.
        panels.set_pose(panel, config.pose)?;
        panels.set_size(panel, config.size)?;
        let url = self.view_url(&view.name);
        let earlier_run_panel = listed.iter().find(|listed_panel| {
            listed_panel.id == panel && listed_panel.state != GuiPanelState::Closed
        });
        let keeps_page = earlier_run_panel.is_some_and(|earlier| earlier.content_uri == url);
        if keeps_page {
            log::info!("qaros: panel {panel} reattached, already showing {url}");
        } else {
            panels.navigate_to_uri(panel, &url)?;
            log::info!("qaros: panel {panel} shows {url}");
        }
        let was_hidden =
            earlier_run_panel.is_some_and(|earlier| earlier.state == GuiPanelState::Hidden);
        if was_hidden {
            panels.set_state(panel, GuiPanelState::Visible)?;
            log::info!("qaros: panel {panel} shown again after the earlier run hid it");
        }
        let is_visible = was_hidden
            || earlier_run_panel.is_some_and(|earlier| earlier.state == GuiPanelState::Visible);
        Ok(ClaimedPanel {
            id: panel,
            kept_visible_page: keeps_page && is_visible,
        })
    }

    /// The common name claims the panel: a restart reattaches to the panel of the same view
    /// in the same app volume, as QAROS intends for common names, whatever address the panel
    /// server has. The display name repeats it, because a panel listing shows display names
    /// and not common names, and that is how [`QarosPanels::is_left_by_earlier_run`] tells
    /// this volume's panels apart.
    fn panel_config(&self, view: &PanelView, slot: usize) -> GuiPanelConfig {
        let common_name = format!("{}{}", view.name, self.common_name_suffix());
        GuiPanelConfig {
            display_name: Some(common_name.clone()),
            pose: to_qaros_pose(self.panel_pose(slot)),
            size: panel_size(view),
            ..GuiPanelConfig::new(common_name)
        }
    }

    fn common_name_suffix(&self) -> String {
        format!(".{}.rerun", self.connection.volume_config().common_name)
    }

    /// A panel of this app volume that this process does not hold, for a view it does not
    /// publish, was left by an earlier run whose views differ (another blueprint, a renamed
    /// view). Such panels share the slot poses of this run's panels and would be drawn in the
    /// same plane. View names hold no dot, so a panel of a volume whose name ends like this
    /// one's does not match.
    fn is_left_by_earlier_run(
        &self,
        panel: &GuiPanelInfo,
        published_view_names: &[String],
    ) -> bool {
        let held = self.panels.values().any(|held| *held == panel.id);
        let view_name = panel.display_name.strip_suffix(&self.common_name_suffix());
        !held
            && panel.state != GuiPanelState::Closed
            && view_name.is_some_and(|view_name| {
                !view_name.is_empty()
                    && !view_name.contains('.')
                    && !published_view_names
                        .iter()
                        .any(|published| published == view_name)
            })
    }

    /// Panels are never closed, and a leftover may be the panel of a view this run publishes later (a background tab, data that arrives late), so
    /// a visible leftover is hidden, never closed: a later claim by name shows it again with
    /// its page in place. One a user minimised stays minimised.
    fn hide_panels_left_by_earlier_runs(
        &self,
        listed: &[GuiPanelInfo],
        published_view_names: &[String],
    ) {
        let panels = self.connection.session().gui_panels();
        for panel in listed {
            if panel.state != GuiPanelState::Visible
                || !self.is_left_by_earlier_run(panel, published_view_names)
            {
                continue;
            }
            match panels.set_state(panel.id, GuiPanelState::Hidden) {
                Ok(()) => log::info!(
                    "qaros: hid panel {} left by an earlier run, showing {}",
                    panel.id,
                    panel.content_uri
                ),
                Err(error) => log::warn!(
                    "qaros: cannot hide panel {} left by an earlier run: {error}",
                    panel.id
                ),
            }
        }
    }

    fn panel_pose(&self, slot: usize) -> Pose {
        let volume = self.connection.volume_config();
        panel_pose_beside_volume(from_qaros_pose(volume.pose), volume.size, slot)
    }
}

/// Room pose of the panel in `slot`, in the convention of the `b1338df` header: the midpoint
/// of the panel's top edge, with panel-space -Z as the front normal pointing at the viewer
/// and +X along the top edge to the viewer's left.
///
/// The viewer stands on the volume's +Z side looking along -Z: the volume sits at room -Z in
/// front of a viewer at the room origin, who looks along OpenXR's -Z forward. Panels turn half
/// a turn about the volume's up axis so their front points along the volume's +Z at that
/// viewer. They form a row to the viewer's right (the volume's +X side), top edges level with
/// the volume's top, at the volume's centre depth.
fn panel_pose_beside_volume(
    room_from_volume: Pose,
    volume_size: AppVolumeSize,
    slot: usize,
) -> Pose {
    #[allow(clippy::cast_precision_loss)]
    let top_centre_in_volume = Vec3::new(
        volume_size.width * 0.5
            + PANEL_GAP_METERS
            + PANEL_WIDTH_METERS * 0.5
            + slot as f32 * (PANEL_WIDTH_METERS + PANEL_GAP_METERS),
        volume_size.height * 0.5,
        0.0,
    );
    let volume_from_panel = Pose::new(Quat::from_rotation_y(PI), top_centre_in_volume);
    room_from_volume * volume_from_panel
}

fn panel_size(view: &PanelView) -> GuiPanelSize {
    #[allow(clippy::cast_precision_loss)]
    let aspect = if view.width_in_pixel == 0 {
        1.0
    } else {
        view.height_in_pixel as f32 / view.width_in_pixel as f32
    };
    GuiPanelSize {
        width: PANEL_WIDTH_METERS,
        height: PANEL_WIDTH_METERS * aspect.clamp(0.1, 4.0),
        content_scale: 1.0,
    }
}

impl Drop for QarosPanels {
    fn drop(&mut self) {
        enter_teardown_step(TeardownStep::HidePanels);
        if self.at_stop == PanelsAtStop::Keep {
            log::info!(
                "qaros: keeping {} panel(s) for the next run",
                self.panels.len()
            );
            return;
        }
        let panels = self.connection.session().gui_panels();
        for (view_name, panel) in &self.panels {
            match panels.set_state(*panel, GuiPanelState::Hidden) {
                Ok(()) => {
                    log::info!("qaros: panel {panel} for {view_name} hidden until the next run");
                }
                Err(error) => log::warn!("qaros: hiding the panel for {view_name}: {error}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-5;

    fn two_metre_cube() -> AppVolumeSize {
        AppVolumeSize {
            width: 2.0,
            length: 2.0,
            height: 2.0,
        }
    }

    fn assert_pose(actual: Pose, expected_orientation: Quat, expected_position: Vec3) {
        assert!(
            actual.position.abs_diff_eq(expected_position, EPSILON),
            "position {} expected {expected_position}",
            actual.position
        );
        // q and -q are the same rotation.
        let same_rotation = actual.orientation.dot(expected_orientation).abs();
        assert!(
            (same_rotation - 1.0).abs() < EPSILON,
            "orientation {} expected {expected_orientation}",
            actual.orientation
        );
    }

    #[test]
    fn panels_hang_right_of_the_default_volume_facing_a_viewer_at_the_origin() {
        let default_volume = Pose::from_translation(Vec3::new(0.0, 0.5, -1.6));
        let half_turn_about_up = Quat::from_xyzw(0.0, 1.0, 0.0, 0.0);
        // Volume half width 1.0 + gap 0.1 + half panel 0.3; the next slot is 0.7 further.
        assert_pose(
            panel_pose_beside_volume(default_volume, two_metre_cube(), 0),
            half_turn_about_up,
            Vec3::new(1.4, 1.5, -1.6),
        );
        assert_pose(
            panel_pose_beside_volume(default_volume, two_metre_cube(), 1),
            half_turn_about_up,
            Vec3::new(2.1, 1.5, -1.6),
        );
    }

    #[test]
    fn panel_front_points_at_the_viewer_and_its_x_runs_to_the_viewer_left() {
        let panel = panel_pose_beside_volume(
            Pose::from_translation(Vec3::new(0.0, 0.5, -1.6)),
            two_metre_cube(),
            0,
        );
        let front_normal = panel.transform_vector(Vec3::NEG_Z);
        let towards_viewer_at_origin = (Vec3::ZERO - panel.position).normalize();
        assert!(front_normal.dot(towards_viewer_at_origin) > 0.5);
        assert!(front_normal.abs_diff_eq(Vec3::Z, EPSILON));
        // The viewer looks along -Z, so their left is room -X.
        assert!(
            panel
                .transform_vector(Vec3::X)
                .abs_diff_eq(Vec3::NEG_X, EPSILON)
        );
    }

    #[test]
    fn panels_follow_a_turned_volume() {
        let quarter_turn = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let turned_volume = Pose::new(quarter_turn, Vec3::new(0.0, 0.5, -1.6));
        // The volume's +X points along room -Z; its +Z front along room +X.
        assert_pose(
            panel_pose_beside_volume(turned_volume, two_metre_cube(), 1),
            quarter_turn * Quat::from_rotation_y(PI),
            Vec3::new(0.0, 1.5, -3.7),
        );
    }

    #[test]
    fn panel_height_follows_the_view_aspect() {
        let wide = panel_size(&PanelView {
            name: "plot".to_owned(),
            width_in_pixel: 800,
            height_in_pixel: 400,
        });
        assert!((wide.width - 0.6).abs() < 1e-6);
        assert!((wide.height - 0.3).abs() < 1e-6);
        let empty = panel_size(&PanelView {
            name: "empty".to_owned(),
            width_in_pixel: 0,
            height_in_pixel: 0,
        });
        assert!((empty.height - 0.6).abs() < 1e-6);
    }
}
