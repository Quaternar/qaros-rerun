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

/// 0.45 m reads well at arm's length and keeps a second panel well in front of the user.
const PANEL_WIDTH_METERS: f32 = 0.45;
const PANEL_GAP_METERS: f32 = 0.1;
/// Within reach: how far from the user's head the panels stand, measured on the floor plane.
const REACH_DISTANCE_METERS: f32 = 0.8;
/// How far below the user's eyes the panels' top edges sit.
const REACH_TOP_BELOW_EYES_METERS: f32 = 0.1;
/// Where the row starts, measured along the arc from the user's gaze to the first panel's
/// left edge, so the panels stand beside what the user looks at rather than in front of it.
const REACH_ROW_START_METERS: f32 = 0.25;
/// A head farther than this from the app volume's centre, on the floor plane, does not anchor
/// new panels: a desktop receiver's camera watching from across the room is nobody who could
/// reach them, and a headset user stands near the content.
const REACH_ANCHOR_MAX_DISTANCE_METERS: f32 = 4.0;

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

/// One QAROS GUI panel per published 2D view, pointed at the view's page on the panel server.
///
/// The Hub owns a panel's place once it exists: it keeps the pose the panel was created with,
/// or wherever a user moved it, across runs, and ignores later pose calls, so users find their
/// panels where they left them. This type therefore never sets the pose of a panel that
/// exists. A new panel is created at its final place, within reach of the first user whose
/// head is near the app volume ([`QarosPanels::consider_head`]); until such a head is known,
/// views without a panel wait. A panel an earlier run left is reattached by its common name
/// as it is. Closing the panels in the Hub Display makes the next run create them afresh.
/// When this value drops the panels stay in the session, hidden or kept as [`PanelsAtStop`]
/// says.
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
    /// The head of the first user streaming near the app volume, gravity-level: new panels
    /// open within reach of it. Set once.
    reach_anchor: Option<Pose>,
    /// Places in the within-reach row taken by panels this run created.
    reach_slots_used: usize,
    /// Views waiting for a head before their new panel opens, already logged as such.
    waiting_logged: BTreeSet<String>,
    /// A head too far from the app volume was logged, once per run.
    far_head_logged: bool,
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
            reach_anchor: None,
            reach_slots_used: 0,
            waiting_logged: BTreeSet::new(),
            far_head_logged: false,
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
    /// exists is reattached at once; a view that needs a new panel waits until
    /// [`QarosPanels::consider_head`] has a head to place it by, and is retried each sync. A
    /// view whose panel cannot be created is logged once and not retried. Once a panel opens, visible panels
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
            let exists = self.existing_panel(&listed, &view.name).is_some();
            if !exists && self.reach_anchor.is_none() {
                if self.waiting_logged.insert(view.name.clone()) {
                    log::info!(
                        "qaros: the panel for {} opens once a viewer near the app volume streams",
                        view.name
                    );
                }
                continue;
            }
            let slot = self.reach_slots_used;
            match self.open_panel(view, slot, &listed) {
                Ok(panel) => {
                    if panel.kept_visible_page {
                        self.kept_pages.insert(view.name.clone(), Instant::now());
                    }
                    if !exists {
                        self.reach_slots_used += 1;
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

    /// Takes `head`, a streaming user's head in room space, as the place new panels open by,
    /// if no head was taken yet and this one is within [`REACH_ANCHOR_MAX_DISTANCE_METERS`] of
    /// the app volume's centre on the floor plane. New panels then open in a row within reach
    /// of it, at the next [`QarosPanels::sync`]. Existing panels are not touched: the Hub keeps
    /// their place.
    pub fn consider_head(&mut self, head: Pose) {
        if self.reach_anchor.is_some() {
            return;
        }
        let volume_centre = from_qaros_pose(self.connection.volume_config().pose).position;
        if !is_near_the_volume(head.position, volume_centre) {
            if !self.far_head_logged {
                self.far_head_logged = true;
                log::info!(
                    "qaros: a viewer at {} streams, too far from the app volume at {volume_centre} \
                     to place panels by; waiting for one nearer",
                    head.position
                );
            }
            return;
        }
        let anchor = gravity_level(head);
        self.reach_anchor = Some(anchor);
        log::info!(
            "qaros: new panels open within reach of the viewer at {}",
            anchor.position
        );
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
        // get_or_create returns an existing panel unchanged. Its place belongs to the Hub and
        // the user, so no pose is set; the size follows this run's view.
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

    /// The open panel of `view_name` in this app volume, if the session has one.
    fn existing_panel<'listed>(
        &self,
        listed: &'listed [GuiPanelInfo],
        view_name: &str,
    ) -> Option<&'listed GuiPanelInfo> {
        let common_name = format!("{view_name}{}", self.common_name_suffix());
        listed
            .iter()
            .find(|panel| panel.display_name == common_name && panel.state != GuiPanelState::Closed)
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
        if let Some(anchor) = self.reach_anchor {
            return panel_pose_within_reach(anchor, slot);
        }
        let volume = self.connection.volume_config();
        panel_pose_beside_volume(from_qaros_pose(volume.pose), volume.size, slot)
    }
}

/// Whether a head at `head_position` stands within [`REACH_ANCHOR_MAX_DISTANCE_METERS`] of the
/// app volume's centre, measured on the floor plane.
fn is_near_the_volume(head_position: Vec3, volume_centre: Vec3) -> bool {
    let offset = head_position - volume_centre;
    Vec3::new(offset.x, 0.0, offset.z).length() <= REACH_ANCHOR_MAX_DISTANCE_METERS
}

/// `head` turned about room +Y only: its forward projected on the floor plane, so a tilted
/// head neither tilts nor raises the panels. A head looking straight up or down keeps room -Z.
fn gravity_level(head: Pose) -> Pose {
    let forward = head.transform_vector(Vec3::NEG_Z);
    let level_forward = Vec3::new(forward.x, 0.0, forward.z)
        .try_normalize()
        .unwrap_or(Vec3::NEG_Z);
    let yaw = (-level_forward.x).atan2(-level_forward.z);
    Pose::new(Quat::from_rotation_y(yaw), head.position)
}

/// Room pose of the panel in `slot` within reach of `head` (gravity-level, see
/// [`gravity_level`]), in the same convention as [`panel_pose_beside_volume`].
///
/// The panels stand on an arc of [`REACH_DISTANCE_METERS`] around the head, starting
/// [`REACH_ROW_START_METERS`] to the right of the gaze and going right, [`PANEL_GAP_METERS`]
/// apart along the arc, top edges [`REACH_TOP_BELOW_EYES_METERS`] below the eyes. Each one
/// turns about +Y only, so it stays upright, with its front facing the head.
fn panel_pose_within_reach(head: Pose, slot: usize) -> Pose {
    #[allow(clippy::cast_precision_loss)]
    let arc_to_centre = REACH_ROW_START_METERS
        + PANEL_WIDTH_METERS * 0.5
        + slot as f32 * (PANEL_WIDTH_METERS + PANEL_GAP_METERS);
    let angle_to_the_right = arc_to_centre / REACH_DISTANCE_METERS;
    let forward = head.transform_vector(Vec3::NEG_Z);
    let right = head.transform_vector(Vec3::X);
    let away_from_head = forward * angle_to_the_right.cos() + right * angle_to_the_right.sin();
    let position = head.position + away_from_head * REACH_DISTANCE_METERS
        - Vec3::Y * REACH_TOP_BELOW_EYES_METERS;
    // Panel +Z points away from the head, so its -Z front faces it.
    let yaw = away_from_head.x.atan2(away_from_head.z);
    Pose::new(Quat::from_rotation_y(yaw), position)
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
    fn a_headset_near_the_volume_anchors_panels_and_a_distant_receiver_camera_does_not() {
        // A measured session: the structure-from-motion volume at room (0, 0.5, -1.7), the
        // glasses' head at (-0.28, 0.43, 0.58) and the first head the viewer saw, a Display
        // Receiver's camera, at (1.21, 2.06, 6.99).
        let volume_centre = Vec3::new(0.0, 0.5, -1.7);
        assert!(is_near_the_volume(
            Vec3::new(-0.277, 0.428, 0.577),
            volume_centre
        ));
        assert!(!is_near_the_volume(
            Vec3::new(1.207, 2.061, 6.989),
            volume_centre
        ));
        // Height does not count: someone on a ladder over the volume is near it.
        assert!(is_near_the_volume(Vec3::new(0.0, 9.0, -1.7), volume_centre));
    }

    #[test]
    fn panels_hang_right_of_the_default_volume_facing_a_viewer_at_the_origin() {
        let default_volume = Pose::from_translation(Vec3::new(0.0, 0.5, -1.6));
        let half_turn_about_up = Quat::from_xyzw(0.0, 1.0, 0.0, 0.0);
        // Volume half width 1.0 + gap 0.1 + half panel 0.225; the next slot is 0.55 further.
        assert_pose(
            panel_pose_beside_volume(default_volume, two_metre_cube(), 0),
            half_turn_about_up,
            Vec3::new(1.325, 1.5, -1.6),
        );
        assert_pose(
            panel_pose_beside_volume(default_volume, two_metre_cube(), 1),
            half_turn_about_up,
            Vec3::new(1.875, 1.5, -1.6),
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
            Vec3::new(0.0, 1.5, -3.475),
        );
    }

    #[test]
    fn panel_height_follows_the_view_aspect() {
        let wide = panel_size(&PanelView {
            name: "plot".to_owned(),
            width_in_pixel: 800,
            height_in_pixel: 400,
        });
        assert!((wide.width - 0.45).abs() < 1e-6);
        assert!((wide.height - 0.225).abs() < 1e-6);
        let empty = panel_size(&PanelView {
            name: "empty".to_owned(),
            width_in_pixel: 0,
            height_in_pixel: 0,
        });
        assert!((empty.height - 0.45).abs() < 1e-6);
    }

    /// Checks one within-reach panel against `head`: on the arc around the head, its top edge
    /// below the eyes, upright, its front facing the head, and to the right of the gaze.
    fn assert_within_reach(head: Pose, panel: Pose) {
        let to_panel = panel.position - head.position;
        let horizontal = Vec3::new(to_panel.x, 0.0, to_panel.z);
        assert!(
            (horizontal.length() - REACH_DISTANCE_METERS).abs() < EPSILON,
            "{} m from the head",
            horizontal.length()
        );
        assert!(
            (to_panel.y + REACH_TOP_BELOW_EYES_METERS).abs() < EPSILON,
            "top edge {} m from eye height",
            to_panel.y
        );
        assert!(
            panel
                .transform_vector(Vec3::Y)
                .abs_diff_eq(Vec3::Y, EPSILON),
            "upright"
        );
        let front_normal = panel.transform_vector(Vec3::NEG_Z);
        assert!(
            front_normal.abs_diff_eq(-horizontal.normalize(), EPSILON),
            "front {front_normal} faces the head"
        );
        let right_of_gaze = head.transform_vector(Vec3::X);
        assert!(horizontal.dot(right_of_gaze) > 0.0, "right of the gaze");
    }

    #[test]
    fn panels_stand_within_reach_right_of_a_head_at_the_origin() {
        let head = Pose::from_translation(Vec3::new(0.0, 1.6, 0.0));
        let first = panel_pose_within_reach(head, 0);
        let second = panel_pose_within_reach(head, 1);
        assert_within_reach(head, first);
        assert_within_reach(head, second);
        // Centres one panel width plus the gap apart along the arc.
        let angle_between = Vec3::new(first.position.x, 0.0, first.position.z)
            .angle_between(Vec3::new(second.position.x, 0.0, second.position.z));
        assert!(
            (angle_between * REACH_DISTANCE_METERS - (PANEL_WIDTH_METERS + PANEL_GAP_METERS)).abs()
                < EPSILON
        );
        // The first panel's left edge sits REACH_ROW_START_METERS along the arc from the gaze.
        let first_angle =
            Vec3::new(first.position.x, 0.0, first.position.z).angle_between(Vec3::NEG_Z);
        assert!(
            (first_angle * REACH_DISTANCE_METERS
                - (REACH_ROW_START_METERS + PANEL_WIDTH_METERS * 0.5))
                .abs()
                < EPSILON
        );
        // +X along the top edge runs to the viewer's left: room -X, roughly, for the first panel.
        assert!(first.transform_vector(Vec3::X).x < 0.0);
    }

    #[test]
    fn panels_follow_a_turned_and_moved_head() {
        let quarter_turn_left = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let head = Pose::new(quarter_turn_left, Vec3::new(1.0, 1.7, -2.0));
        // Turned a quarter to the left, the head looks along room -X; its right is room -Z.
        assert!(
            head.transform_vector(Vec3::NEG_Z)
                .abs_diff_eq(Vec3::NEG_X, EPSILON)
        );
        for slot in 0..2 {
            let panel = panel_pose_within_reach(head, slot);
            assert_within_reach(head, panel);
            assert!(panel.position.x < head.position.x, "in front of the head");
            assert!(panel.position.z < head.position.z, "to the head's right");
        }
    }

    #[test]
    fn a_tilted_head_places_the_panels_by_its_heading_alone() {
        let heading = Quat::from_rotation_y(0.4);
        let tilted = Pose::new(
            heading * Quat::from_rotation_x(-0.5) * Quat::from_rotation_z(0.3),
            Vec3::new(0.2, 1.6, 0.4),
        );
        let level = gravity_level(tilted);
        assert!(level.orientation.dot(heading).abs() > 1.0 - EPSILON);
        assert_eq!(level.position, tilted.position);
        assert_within_reach(level, panel_pose_within_reach(level, 0));
        let straight_down = Pose::new(
            Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            Vec3::ZERO,
        );
        assert!(
            gravity_level(straight_down)
                .transform_vector(Vec3::NEG_Z)
                .abs_diff_eq(Vec3::NEG_Z, EPSILON)
        );
    }
}
