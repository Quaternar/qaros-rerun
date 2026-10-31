use std::collections::{BTreeMap, BTreeSet};
use std::f32::consts::PI;
use std::rc::Rc;
use std::time::{Duration, Instant};

use frame_sink::UserId;
use glam::{Quat, Vec3};
use qaros::{AppVolumeSize, GuiPanelConfig, GuiPanelId, GuiPanelSize, GuiPanelState, PeerId};
use xr_math::Pose;

use crate::connection::{QarosConnection, from_qaros_pose, to_qaros_pose};
use crate::panel_gestures::PanelGestures;
use crate::panel_images::{PanelImages, TIMELINE_IMAGE_KEY};
use crate::panel_listing::{ListedPanel, list_panels};
use crate::peer_devices::PeerDevice;
use crate::stop_watchdog::{TeardownStep, enter_teardown_step};
use crate::stream_panels::{QarosStreamBackend, StreamPanels};

/// A 2D view published for its panel: its name and current size in pixels. The name is the
/// same on every run (no `.`, unique within a run): it names the view's QAROS panel, so a
/// restart finds the panel and gives it the new stream.
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
/// How long new panels wait for a headset's head once a head near the app volume streams. A
/// Visualizer receiver (the Hub Display on the PC) streams too and its camera can stand near
/// the volume, but nobody can reach panels placed by it; after this wait, with no headset
/// seen, any head near the volume places them, so a run without glasses still gets panels.
pub const HEADSET_WAIT: Duration = Duration::from_mins(1);
/// How often a viewer's device is asked of QAROS again while panels wait for a headset: a
/// target refines its device identity once the device reports its model.
const DEVICE_LOOKUP_INTERVAL: Duration = Duration::from_secs(1);

/// The timeline panel's key in [`QarosPanels::panels`]. It holds a dot, which no view name
/// holds, so it never passes for a view's panel. Panels of earlier builds that showed web
/// pages carry it as their name before the volume suffix.
const TIMELINE_PANEL_NAME: &str = "timeline.panel";
/// The timeline panel's common name before the volume suffix.
const STREAM_TIMELINE_PANEL_NAME: &str = "timeline.panel-stream";
/// A panel listing slower than this is logged: on QAROS 0.9.0-dev.3177 each gui panel query
/// blocks its thread for about 2 s, and the listing runs on the QAROS thread.
const SLOW_LISTING: Duration = Duration::from_millis(100);
/// The timeline panel's nominal size, wide and short for a slider; its first image sets the
/// height it is created with ([`stream_panel_size`]).
const TIMELINE_PANEL_WIDTH_METERS: f32 = 0.45;
const TIMELINE_PANEL_HEIGHT_METERS: f32 = 0.12;
/// The wait before retrying a failed panel listing, the first time.
const LISTING_RETRY_FAST: Duration = Duration::from_millis(500);
/// The wait before every later retry.
const LISTING_RETRY_SLOW: Duration = Duration::from_secs(30);
/// A listing serves every claim for this long: each one blocks the QAROS thread for about 4 s
/// on 3177 (QAROS #345), so the views that settle at the start share one, and a view that
/// settles later lists again at most this often.
const LISTING_REUSE: Duration = Duration::from_secs(30);
/// How far in front of the user's head the timeline panel's centre stands, along the gaze on
/// the floor plane.
const TIMELINE_FORWARD_METERS: f32 = 0.5;
/// How far below the user's eyes the timeline panel's centre sits.
const TIMELINE_BELOW_EYES_METERS: f32 = 0.2;

/// What the viewer's panels become when [`QarosPanels`] drops at Stop. Neither closes them:
/// they stay in the session for the next run to claim by common name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelsAtStop {
    /// `QAR_GUI_PANEL_STATE_HIDDEN`, so no frozen image hangs in the room while the app is
    /// not running; the next run shows them again when it reattaches.
    #[default]
    Hide,
    /// Left as they are, with no panel call at all.
    Keep,
}

/// One QAROS GUI panel per published 2D view, each showing a CPU video sender of this viewer
/// fed from the view's image, and with [`QarosPanels::with_timeline_panel`] one timeline panel
/// for the run. Panels go by common names `<view>.stream.<volume>.rerun` and
/// `timeline.panel-stream.<volume>.rerun`.
///
/// The Hub owns a panel's place once it exists: it keeps the pose the panel was created with,
/// or wherever a user moved it, across runs, and ignores later pose calls, so users find their
/// panels where they left them. This type therefore never sets the pose of a panel that
/// exists. A new panel is created at its final place, within reach of the first headset user
/// whose head is near the app volume, or of any viewer near it once none came for
/// [`HEADSET_WAIT`] ([`QarosPanels::consider_head`]); until such a head is known, views without
/// a panel wait. A panel an earlier run left is reattached by its common name
/// as it is. A panel closed in the Hub Display is claimed again by its common name, never
/// replaced under a new one (ADR-0075).
/// When this value drops the panels stay in the session, hidden or kept as [`PanelsAtStop`]
/// says.
pub struct QarosPanels {
    connection: Rc<QarosConnection>,
    streams: StreamPanels<QarosStreamBackend>,
    images: PanelImages,
    /// `Some` when this run opens a timeline panel: where its gestures go.
    timeline_gestures: Option<PanelGestures>,
    /// Panels of earlier builds that showed web pages and are left visible were logged, once
    /// per run.
    web_leftovers_logged: bool,
    panels: BTreeMap<String, GuiPanelId>,
    failed: BTreeSet<String>,
    at_stop: PanelsAtStop,
    /// The head that places new panels, gravity-level: they open within reach of it. Set once.
    reach_anchor: Option<Pose>,
    /// When a head near the app volume first streamed, which starts [`HEADSET_WAIT`].
    first_near_head: Option<Instant>,
    /// What each viewer near the volume is, as last asked of QAROS, and when it was asked.
    viewer_devices: BTreeMap<UserId, (DeviceLookup, Instant)>,
    /// Viewers near the volume that are not headsets, logged once each while panels wait.
    not_headset_logged: BTreeSet<UserId>,
    /// Places in the within-reach row taken by panels this run created.
    reach_slots_used: usize,
    /// Views waiting for a head before their new panel opens, already logged as such.
    waiting_logged: BTreeSet<String>,
    /// A head too far from the app volume was logged, once per run.
    far_head_logged: bool,
    /// Panel listings that failed in a row, and when the next one may be tried.
    listing_failures: u32,
    next_listing: Option<Instant>,
    /// The last listing, kept up to date with the panels this run hid, and when it was made.
    listing: Option<(Vec<ListedPanel>, Instant)>,
    /// Listings made this run, for the log.
    listings_made: u32,
}

/// A panel this run opens: one per settled view, and the timeline panel.
enum WantedPanel<'view> {
    View(&'view PanelView),
    Timeline,
}

impl WantedPanel<'_> {
    /// The key in [`QarosPanels::panels`] and the start of the panel's common name.
    fn name(&self) -> &str {
        match self {
            Self::View(view) => &view.name,
            Self::Timeline => TIMELINE_PANEL_NAME,
        }
    }
}

impl QarosPanels {
    /// Panels for the views whose images are published in `images`.
    pub fn new(connection: Rc<QarosConnection>, images: PanelImages) -> Self {
        Self {
            streams: StreamPanels::new(QarosStreamBackend {
                connection: Rc::clone(&connection),
            }),
            connection,
            images,
            timeline_gestures: None,
            web_leftovers_logged: false,
            panels: BTreeMap::new(),
            failed: BTreeSet::new(),
            at_stop: PanelsAtStop::default(),
            reach_anchor: None,
            first_near_head: None,
            viewer_devices: BTreeMap::new(),
            not_headset_logged: BTreeSet::new(),
            reach_slots_used: 0,
            waiting_logged: BTreeSet::new(),
            far_head_logged: false,
            listing_failures: 0,
            next_listing: None,
            listing: None,
            listings_made: 0,
        }
    }

    /// Shows at most one due image on its panel's sender, and retires replaced senders.
    /// Called on every viewer frame (every eye frame with `--decoupled-render`) on the QAROS
    /// thread.
    pub fn pump_streams(&mut self, now: Instant) {
        self.streams.pump(&self.images, now);
    }

    /// Also opens one timeline panel, whether or not any view is published, showing the image
    /// published under [`TIMELINE_IMAGE_KEY`] once there is one, with its gestures going to
    /// `gestures`. A new one opens once a head places panels ([`QarosPanels::consider_head`]),
    /// [`TIMELINE_FORWARD_METERS`] in front of that head and [`TIMELINE_BELOW_EYES_METERS`]
    /// below its eyes, facing them; one an earlier run left is reattached, kept and hidden at
    /// Stop like the views' panels.
    #[must_use]
    pub fn with_timeline_panel(mut self, gestures: PanelGestures) -> Self {
        self.timeline_gestures = Some(gestures);
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

    /// Opens or reattaches a panel for every settled view not seen before. A view whose panel
    /// exists is reattached at once; a view that needs a new panel waits until
    /// [`QarosPanels::consider_head`] has a head to place it by, and is retried each sync. A
    /// view whose panel cannot be created is logged once and not retried. Once a panel opens,
    /// visible panels that earlier runs left in this app volume are hidden, except those of
    /// views still among `published_view_names`: a view published but not settled yet claims
    /// its panel later, and a view published later still finds its hidden panel by name.
    /// While the panel list cannot be read nothing is opened, so the next sync retries:
    /// without the list a reattached panel would pass for a new one.
    ///
    /// Each sync also replaces senders whose image settled on a new size or that failed. The
    /// claims share one listing for [`LISTING_REUSE`], which also checks
    /// that the held panels are still listed and not `Closed`; no listing is made for that
    /// check alone, because a listing blocks the QAROS thread, and with it the eye frames, for
    /// seconds on 3177. A panel
    /// closed while held stays closed until the next run (ADR-0075): it is still counted as
    /// held, so it is not claimed again.
    pub fn sync(&mut self, views: &[PanelView], published_view_names: &[String]) {
        let now = Instant::now();
        self.streams.maintain(&self.images, now, stream_panel_size);
        let mut wanted: Vec<WantedPanel<'_>> = views.iter().map(WantedPanel::View).collect();
        if self.timeline_gestures.is_some() {
            wanted.push(WantedPanel::Timeline);
        }
        wanted.retain(|panel| {
            !self.panels.contains_key(panel.name())
                && !self.failed.contains(panel.name())
                && !self.waits_for_its_timeline_image(panel)
        });
        if wanted.is_empty() {
            return;
        }
        let Some(mut listed) = self.listing_for_claims(now) else {
            return;
        };
        let mut opened_any = false;
        for panel in wanted {
            let name = panel.name().to_owned();
            let exists = self.existing_panel(&listed, &name).is_some();
            if !exists && self.reach_anchor.is_none() {
                if self.waiting_logged.insert(name.clone()) {
                    log::info!(
                        "qaros: the panel for {name} opens once a viewer near the app volume streams"
                    );
                }
                continue;
            }
            let slot = self.reach_slots_used;
            match self.open_stream_panel(&panel, slot, &listed) {
                Ok(id) => {
                    if !exists && matches!(panel, WantedPanel::View(_)) {
                        self.reach_slots_used += 1;
                    }
                    self.panels.insert(name, id);
                    opened_any = true;
                }
                Err(error) => {
                    log::warn!("qaros: no panel for {name}: {error}");
                    self.failed.insert(name);
                }
            }
        }
        if opened_any {
            let hidden = self.hide_panels_left_by_earlier_runs(&listed, published_view_names);
            for panel in listed.iter_mut().filter(|panel| hidden.contains(&panel.id)) {
                panel.state = GuiPanelState::Hidden;
            }
        }
        if let Some((kept, _)) = &mut self.listing {
            *kept = listed;
        }
    }

    /// The listing the claims of this sync use: the last one while it is younger than
    /// [`LISTING_REUSE`], else a new one, which also checks the held panels (#344). `None`
    /// while the panels cannot be listed.
    fn listing_for_claims(&mut self, now: Instant) -> Option<Vec<ListedPanel>> {
        if let Some((listed, made)) = &self.listing
            && now.saturating_duration_since(*made) < LISTING_REUSE
        {
            return Some(listed.clone());
        }
        if self.next_listing.is_some_and(|next| now < next) {
            return None;
        }
        self.listings_made += 1;
        match timed_list_panels(&self.connection, self.listings_made) {
            Ok(listed) => {
                self.listing_failures = 0;
                self.next_listing = None;
                self.streams.note_listing(&listed);
                self.listing = Some((listed.clone(), now));
                Some(listed)
            }
            Err(error) => {
                self.listing_failures += 1;
                let retry_in = listing_retry_delay(self.listing_failures);
                self.next_listing = Some(Instant::now() + retry_in);
                log::warn!(
                    "qaros: cannot list panels ({error}); opening none, trying again in {retry_in:?}"
                );
                None
            }
        }
    }

    /// The timeline panel takes its sender's size from its first image, so it waits until
    /// there is one.
    fn waits_for_its_timeline_image(&mut self, wanted: &WantedPanel<'_>) -> bool {
        let waits = matches!(wanted, WantedPanel::Timeline)
            && self.images.latest(TIMELINE_IMAGE_KEY).is_none();
        if waits && self.waiting_logged.insert(TIMELINE_IMAGE_KEY.to_owned()) {
            log::info!("qaros: the timeline panel opens once its first image is published");
        }
        waits
    }

    /// Claims the stream panel of a view at the image size the view was published at, or the
    /// timeline's at its latest image's size.
    fn open_stream_panel(
        &mut self,
        wanted: &WantedPanel<'_>,
        slot: usize,
        listed: &[ListedPanel],
    ) -> Result<GuiPanelId, String> {
        let config = self.panel_config(wanted, slot);
        let (image_key, pixels, gestures) = match wanted {
            WantedPanel::View(view) => (
                view.name.as_str(),
                (view.width_in_pixel, view.height_in_pixel),
                None,
            ),
            WantedPanel::Timeline => {
                let image = self
                    .images
                    .latest(TIMELINE_IMAGE_KEY)
                    .ok_or("no timeline image published yet")?;
                (
                    TIMELINE_IMAGE_KEY,
                    (image.width, image.height),
                    self.timeline_gestures.clone(),
                )
            }
        };
        self.streams
            .claim(image_key, &config, listed, pixels, gestures)
    }

    /// Takes `head`, the head of the streaming `user` in room space at `now`, as the place new
    /// panels open by, if no head was taken yet, this one is within
    /// [`REACH_ANCHOR_MAX_DISTANCE_METERS`] of the app volume's centre on the floor plane, and
    /// the user's device places panels: a headset's head at once, any other
    /// once no headset came for [`HEADSET_WAIT`] after the first head near the volume. New
    /// panels then open within reach of it, at the next [`QarosPanels::sync`]. Existing panels
    /// are not touched: the Hub keeps their place.
    pub fn consider_head(&mut self, user: UserId, head: Pose, now: Instant) {
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
        let waited_for_a_headset =
            now.saturating_duration_since(*self.first_near_head.get_or_insert(now));
        let lookup = self.viewer_device(user, now);
        let device = lookup.as_ref().ok().and_then(Option::as_ref);
        let viewer = describe_viewer(user, self.connection.peer_of_user(user), &lookup);
        if !places_panels(device, waited_for_a_headset) {
            if self.not_headset_logged.insert(user) {
                log::info!(
                    "qaros: {viewer} near the app volume is no headset; new panels wait up to \
                     {HEADSET_WAIT:?} for a headset's head"
                );
            }
            return;
        }
        let anchor = gravity_level(head);
        self.reach_anchor = Some(anchor);
        if device.is_some_and(PeerDevice::is_headset) {
            log::info!(
                "qaros: new panels open within reach of {viewer}, at {}",
                anchor.position
            );
        } else {
            log::info!(
                "qaros: no headset near the app volume for {HEADSET_WAIT:?}; new panels open \
                 within reach of {viewer}, at {}",
                anchor.position
            );
        }
    }

    /// What QAROS says `user`'s device is, asked again at most every
    /// [`DEVICE_LOOKUP_INTERVAL`] until it says headset. `Ok(None)` while the user's peer is
    /// not known or not listed.
    fn viewer_device(&mut self, user: UserId, now: Instant) -> DeviceLookup {
        if let Some((lookup, asked)) = self.viewer_devices.get(&user) {
            let is_headset = lookup
                .as_ref()
                .is_ok_and(|device| device.as_ref().is_some_and(PeerDevice::is_headset));
            if is_headset || now.saturating_duration_since(*asked) < DEVICE_LOOKUP_INTERVAL {
                return lookup.clone();
            }
        }
        let lookup = match self.connection.peer_of_user(user) {
            Some(peer) => self.connection.peer_device(peer),
            None => Ok(None),
        };
        self.viewer_devices.insert(user, (lookup.clone(), now));
        lookup
    }

    /// The common name claims the panel: a restart reattaches to the panel of the same view
    /// in the same app volume, as QAROS intends for common names. The display name repeats it,
    /// because a panel listing shows display names and not common names, and that is how a
    /// claim finds the panel and how [`classify_leftovers`] tells this volume's panels apart.
    /// A view's panel is as tall as its content area needs to keep the view's aspect.
    fn panel_config(&self, wanted: &WantedPanel<'_>, slot: usize) -> GuiPanelConfig {
        let common_name = self.common_name(wanted.name());
        let (pose, size) = match wanted {
            WantedPanel::View(view) => (
                self.panel_pose(slot),
                stream_panel_size(view.width_in_pixel, view.height_in_pixel),
            ),
            WantedPanel::Timeline => (
                self.timeline_panel_pose(),
                self.images
                    .latest(TIMELINE_IMAGE_KEY)
                    .map_or_else(timeline_panel_size, |image| {
                        stream_panel_size(image.width, image.height)
                    }),
            ),
        };
        GuiPanelConfig {
            display_name: Some(common_name.clone()),
            pose: to_qaros_pose(pose),
            size,
            ..GuiPanelConfig::new(common_name)
        }
    }

    /// The open panel of `name` in this app volume, if the session has one.
    fn existing_panel<'listed>(
        &self,
        listed: &'listed [ListedPanel],
        name: &str,
    ) -> Option<&'listed ListedPanel> {
        let common_name = self.common_name(name);
        listed
            .iter()
            .find(|panel| panel.display_name == common_name && panel.state != GuiPanelState::Closed)
    }

    fn common_name_suffix(&self) -> String {
        format!(".{}.rerun", self.connection.volume_config().common_name)
    }

    /// `<view>.stream.<volume>.rerun` and `timeline.panel-stream.<volume>.rerun`. A panel's
    /// content type is fixed at creation, so the names differ from those of earlier builds'
    /// web page panels, which could never show a stream.
    fn common_name(&self, name: &str) -> String {
        let suffix = self.common_name_suffix();
        if name == TIMELINE_PANEL_NAME {
            format!("{STREAM_TIMELINE_PANEL_NAME}{suffix}")
        } else {
            format!("{name}.stream{suffix}")
        }
    }

    /// Hides visible `<view>.stream.<volume>.rerun` panels that this run does not hold, for
    /// views it does not publish: earlier runs whose views differ (another blueprint, a renamed
    /// view) left them, and they share the slot poses of this run's panels. A leftover may be
    /// the panel of a view this run publishes later (a background tab, data that arrives late),
    /// so it is hidden, never closed: a later claim by name shows it again. One a user
    /// minimised stays minimised. Panels of earlier builds that showed web pages get no call
    /// at all, because hiding web panels coincided with mixer crashes on 3137 (#278); visible
    /// ones are logged once.
    fn hide_panels_left_by_earlier_runs(
        &mut self,
        listed: &[ListedPanel],
        published_view_names: &[String],
    ) -> Vec<GuiPanelId> {
        let held: Vec<GuiPanelId> = self.panels.values().copied().collect();
        let leftovers = classify_leftovers(
            listed,
            &self.common_name_suffix(),
            &held,
            published_view_names,
        );
        if !leftovers.web_panels_visible.is_empty() && !self.web_leftovers_logged {
            self.web_leftovers_logged = true;
            log::warn!(
                "qaros: panels of an earlier build that showed web pages are left visible: {}; \
                 close them in the Hub Display",
                leftovers.web_panels_visible.join(", ")
            );
        }
        let panels = self.connection.session().gui_panels();
        let mut hidden = Vec::new();
        for panel in leftovers.stream_panels_to_hide {
            match panels.set_state(panel.id, GuiPanelState::Hidden) {
                Ok(()) => {
                    hidden.push(panel.id);
                    log::info!(
                        "qaros: hid panel {} ({}) left by an earlier run",
                        panel.id,
                        panel.display_name
                    );
                }
                Err(error) => log::warn!(
                    "qaros: cannot hide panel {} left by an earlier run: {error}",
                    panel.id
                ),
            }
        }
        hidden
    }

    fn panel_pose(&self, slot: usize) -> Pose {
        if let Some(anchor) = self.reach_anchor {
            return panel_pose_within_reach(anchor, slot);
        }
        let volume = self.connection.volume_config();
        panel_pose_beside_volume(from_qaros_pose(volume.pose), volume.size, slot)
    }

    /// Without a head a new panel does not open, and an existing panel keeps its place
    /// whatever pose it is given, so the fallback is never seen.
    fn timeline_panel_pose(&self) -> Pose {
        self.reach_anchor
            .map_or_else(|| self.panel_pose(0), timeline_panel_pose_facing)
    }
}

/// Room pose of the timeline panel for `head` (gravity-level, see [`gravity_level`]), in the
/// convention of [`panel_pose_beside_volume`] (the midpoint of the top edge, front along
/// panel -Z). The panel's centre stands [`TIMELINE_FORWARD_METERS`] in front of the head along
/// its gaze and [`TIMELINE_BELOW_EYES_METERS`] below the eyes; the panel turns about +Y to the
/// head and tilts back about its top edge's axis until its front faces the eyes, so its top
/// edge stays level.
fn timeline_panel_pose_facing(head: Pose) -> Pose {
    let forward = head.transform_vector(Vec3::NEG_Z);
    let centre =
        head.position + forward * TIMELINE_FORWARD_METERS - Vec3::Y * TIMELINE_BELOW_EYES_METERS;
    let away_from_head = centre - head.position;
    let level_distance = Vec3::new(away_from_head.x, 0.0, away_from_head.z).length();
    let yaw = away_from_head.x.atan2(away_from_head.z);
    let tilt = (-away_from_head.y).atan2(level_distance);
    // Panel +Z points away from the eyes, so its -Z front faces them.
    let orientation = Quat::from_rotation_y(yaw) * Quat::from_rotation_x(tilt);
    let top_centre = centre + orientation * Vec3::Y * (TIMELINE_PANEL_HEIGHT_METERS * 0.5);
    Pose::new(orientation, top_centre)
}

/// What a sync does with the visible panels of earlier runs in its listing.
struct Leftovers<'listed> {
    /// `<view>.stream.<volume>.rerun` panels not held, for views not published.
    stream_panels_to_hide: Vec<&'listed ListedPanel>,
    /// Display names of earlier builds' web page panels, `<view>.<volume>.rerun` and
    /// `timeline.panel.<volume>.rerun` panels; they get no call.
    web_panels_visible: Vec<&'listed str>,
}

/// View names hold no dot, so a panel of another volume whose name ends like this one's, and
/// the stream timeline (`timeline.panel-stream`), match neither list.
fn classify_leftovers<'listed>(
    listed: &'listed [ListedPanel],
    volume_suffix: &str,
    held: &[GuiPanelId],
    published_view_names: &[String],
) -> Leftovers<'listed> {
    let mut leftovers = Leftovers {
        stream_panels_to_hide: Vec::new(),
        web_panels_visible: Vec::new(),
    };
    let is_view_name = |name: &str| !name.is_empty() && !name.contains('.');
    for panel in listed {
        if panel.state != GuiPanelState::Visible || held.contains(&panel.id) {
            continue;
        }
        let Some(name) = panel.display_name.strip_suffix(volume_suffix) else {
            continue;
        };
        if let Some(view_name) = name.strip_suffix(".stream") {
            let published = published_view_names
                .iter()
                .any(|published| published == view_name);
            if is_view_name(view_name) && !published {
                leftovers.stream_panels_to_hide.push(panel);
            }
        } else if is_view_name(name) || name == TIMELINE_PANEL_NAME {
            leftovers
                .web_panels_visible
                .push(panel.display_name.as_str());
        }
    }
    leftovers
}

fn timeline_panel_size() -> GuiPanelSize {
    GuiPanelSize {
        width: TIMELINE_PANEL_WIDTH_METERS,
        height: TIMELINE_PANEL_HEIGHT_METERS,
        content_scale: 1.0,
    }
}

/// A viewer's device as QAROS reports it: `Ok(None)` when its peer is not known or not listed,
/// `Err` when QAROS cannot say.
type DeviceLookup = Result<Option<PeerDevice>, String>;

/// Whether the head of a viewer near the app volume places new panels: a headset's at once
/// (`QAR_DEVICE_CATEGORY_HEADSET`), any other viewer's (a Visualizer receiver, or a device
/// QAROS does not name) only after [`HEADSET_WAIT`] without a headset.
fn places_panels(device: Option<&PeerDevice>, waited_for_a_headset: Duration) -> bool {
    device.is_some_and(PeerDevice::is_headset) || waited_for_a_headset >= HEADSET_WAIT
}

/// For logs: "user-2 (peer 01:02:..., headset "METALENSE2")".
fn describe_viewer(user: UserId, peer: Option<PeerId>, lookup: &DeviceLookup) -> String {
    let peer = peer.map_or_else(|| "peer unknown".to_owned(), |peer| format!("peer {peer}"));
    let device = match lookup {
        Ok(Some(device)) if device.name.is_empty() => device.category_name().to_owned(),
        Ok(Some(device)) => format!("{} {:?}", device.category_name(), device.name),
        Ok(None) => "device not listed".to_owned(),
        Err(error) => format!("device unreadable: {error}"),
    };
    format!("{user} ({peer}, {device})")
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

/// Lists the panels under the perf event `panel_listing`, and logs each listing with its
/// number in this run, and one slower than [`SLOW_LISTING`] with its time.
fn timed_list_panels(
    connection: &QarosConnection,
    number_in_run: u32,
) -> Result<Vec<ListedPanel>, String> {
    let started = Instant::now();
    let listed = list_panels(connection);
    let took = started.elapsed();
    frame_sink::perf::record("panel_listing", None, took.as_micros());
    if took > SLOW_LISTING {
        log::info!(
            "qaros: panel listing {number_in_run} of this run took {took:?} on the QAROS thread"
        );
    } else {
        log::info!("qaros: panel listing {number_in_run} of this run");
    }
    listed
}

/// A listing can fail on a panel the Hub cannot describe, and each try blocks the UI thread
/// while it runs, so after the first quick retry the viewer waits [`LISTING_RETRY_SLOW`].
fn listing_retry_delay(failures_in_a_row: u32) -> Duration {
    if failures_in_a_row <= 1 {
        LISTING_RETRY_FAST
    } else {
        LISTING_RETRY_SLOW
    }
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

/// Of a stream panel's size, what the 3137 mixer leaves for the video (`Browser.hpp` at
/// `c6e052d3`): the window's UI padding of 0.005 m on each side (L460, `BrowserUiPadding`), then
/// a 0.04 m margin from the window's left, right and bottom edges (L465, `ContentEdgeMargin`,
/// `ContentBoundsWithinMargin` L267-277), and above the content the title bar row: 0.05 m
/// buttons (L90) plus StereoKit's 0.01 m gutter under them (L1493-1547).
const STREAM_PANEL_HORIZONTAL_CHROME_METERS: f32 = 2.0 * 0.04;
const STREAM_PANEL_VERTICAL_CHROME_METERS: f32 = 0.005 + 0.05 + 0.01 + 0.04;

/// A stream panel tall enough that its content area has the view's aspect. The mixer fits the
/// video to the content width and scrolls it when it is more than 1/0.9 of the content height
/// (`FitWholeMinimumVisibleFraction`, `GuiPanelScrollbar.hpp` L62-72), so a panel sized by the
/// image's aspect alone shows a scroll bar: 0.45 m wide for a 1016x665 view leaves
/// 0.37 m x 0.19 m for an image that needs 0.37 m x 0.24 m.
fn stream_panel_size(width_in_pixel: u32, height_in_pixel: u32) -> GuiPanelSize {
    #[allow(clippy::cast_precision_loss)]
    let aspect = if width_in_pixel == 0 {
        1.0
    } else {
        height_in_pixel as f32 / width_in_pixel as f32
    };
    let content_width = PANEL_WIDTH_METERS - STREAM_PANEL_HORIZONTAL_CHROME_METERS;
    GuiPanelSize {
        width: PANEL_WIDTH_METERS,
        height: content_width * aspect.clamp(0.1, 4.0) + STREAM_PANEL_VERTICAL_CHROME_METERS,
        content_scale: 1.0,
    }
}

/// Stop, still on the thread that owns the session (ADR-0060): the panels hidden, then the
/// gesture subscriptions cancelled and the senders destroyed, retiring ones included, all
/// before the session goes. A sender that outlives its session can crash the
/// application on 3137. Each step logs its duration as the next one is entered, against the
/// 600 ms [`STOP_DEADLINE`](crate::STOP_DEADLINE).
impl Drop for QarosPanels {
    fn drop(&mut self) {
        enter_teardown_step(TeardownStep::HidePanels);
        if self.at_stop == PanelsAtStop::Keep {
            log::info!(
                "qaros: keeping {} panel(s) for the next run",
                self.panels.len()
            );
        } else {
            self.streams.hide_all();
        }
        enter_teardown_step(TeardownStep::ClosePanelStreams);
        self.streams.close();
    }
}

#[cfg(test)]
mod tests {
    use qaros::qaros_sys::QarDeviceCategory;

    use crate::panel_listing::PanelContent;

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

    fn device(category: QarDeviceCategory, name: &str) -> PeerDevice {
        PeerDevice {
            category,
            name: name.to_owned(),
        }
    }

    #[test]
    fn a_headset_places_panels_at_once_and_a_display_receiver_only_after_the_wait() {
        let glasses = device(QarDeviceCategory::QAR_DEVICE_CATEGORY_HEADSET, "METALENSE2");
        let display_receiver = device(QarDeviceCategory::QAR_DEVICE_CATEGORY_VISUALIZER, "");
        let unnamed = device(QarDeviceCategory::QAR_DEVICE_CATEGORY_UNKNOWN, "");
        assert_eq!(HEADSET_WAIT, Duration::from_mins(1));
        let just_short = Duration::from_millis(19_999);

        assert!(places_panels(Some(&glasses), Duration::ZERO));
        assert!(!places_panels(Some(&display_receiver), Duration::ZERO));
        assert!(!places_panels(Some(&display_receiver), just_short));
        assert!(places_panels(Some(&display_receiver), HEADSET_WAIT));
        assert!(!places_panels(Some(&unnamed), just_short));
        assert!(places_panels(Some(&unnamed), HEADSET_WAIT));
        assert!(
            !places_panels(None, just_short),
            "a viewer whose device QAROS cannot say waits like a receiver"
        );
        assert!(places_panels(None, HEADSET_WAIT));
    }

    #[test]
    fn the_log_names_the_viewer_its_peer_and_its_device() {
        let peer = PeerId([0xAB; 16]);
        let peer_text = "AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB:AB";
        assert_eq!(
            describe_viewer(
                UserId(2),
                Some(peer),
                &Ok(Some(device(
                    QarDeviceCategory::QAR_DEVICE_CATEGORY_HEADSET,
                    "METALENSE2"
                )))
            ),
            format!("user-2 (peer {peer_text}, headset \"METALENSE2\")")
        );
        assert_eq!(
            describe_viewer(
                UserId(1),
                Some(peer),
                &Ok(Some(device(
                    QarDeviceCategory::QAR_DEVICE_CATEGORY_VISUALIZER,
                    ""
                )))
            ),
            format!("user-1 (peer {peer_text}, Visualizer receiver)")
        );
        assert_eq!(
            describe_viewer(UserId(3), None, &Ok(None)),
            "user-3 (peer unknown, device not listed)"
        );
        assert_eq!(
            describe_viewer(UserId(4), Some(peer), &Err("not implemented".to_owned())),
            format!("user-4 (peer {peer_text}, device unreadable: not implemented)")
        );
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
    fn a_stream_panel_leaves_its_content_area_the_views_aspect() {
        let size = stream_panel_size(1016, 665);
        assert!((size.width - 0.45).abs() < 1e-6);
        let content_width = size.width - 0.08;
        let content_height = size.height - 0.105;
        let content_aspect = content_height / content_width;
        assert!(
            (content_aspect - 665.0 / 1016.0).abs() < 1e-4,
            "content {content_width} x {content_height}"
        );
        assert!((size.height - (0.37 * 665.0 / 1016.0 + 0.105)).abs() < 1e-5);
        let empty = stream_panel_size(0, 0);
        assert!(
            (empty.height - (0.37 + 0.105)).abs() < 1e-5,
            "an empty view gets a square content area: {}",
            empty.height
        );
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
    fn the_timeline_panel_stands_half_a_metre_ahead_and_a_fifth_below_the_eyes_facing_them() {
        let head = Pose::from_translation(Vec3::new(0.0, 1.6, 0.0));
        let panel = timeline_panel_pose_facing(head);
        let half_height = TIMELINE_PANEL_HEIGHT_METERS * 0.5;
        // Tilted back by atan(0.2 / 0.5), about 21.8 degrees, the top edge sits 0.06 m up the
        // panel from its centre at (0, 1.4, -0.5).
        let tilt = 0.2_f32.atan2(0.5);
        assert_pose(
            panel,
            Quat::from_rotation_y(PI) * Quat::from_rotation_x(tilt),
            Vec3::new(
                0.0,
                1.4 + half_height * tilt.cos(),
                -0.5 - half_height * tilt.sin(),
            ),
        );
        let centre = panel.position - panel.transform_vector(Vec3::Y) * half_height;
        assert!(
            centre.abs_diff_eq(Vec3::new(0.0, 1.4, -0.5), EPSILON),
            "centre {centre}"
        );
        let front_normal = panel.transform_vector(Vec3::NEG_Z);
        let to_the_eyes = (head.position - centre).normalize();
        assert!(
            front_normal.abs_diff_eq(to_the_eyes, EPSILON),
            "front {front_normal} faces the eyes along {to_the_eyes}"
        );
        assert!(
            front_normal.abs_diff_eq(Vec3::new(0.0, 0.2, 0.5).normalize(), EPSILON),
            "{front_normal}"
        );
        assert!(
            panel
                .transform_vector(Vec3::X)
                .abs_diff_eq(Vec3::NEG_X, EPSILON),
            "the top edge level, +X to the viewer's left"
        );
    }

    #[test]
    fn the_timeline_panel_follows_a_turned_tilted_head_by_its_heading() {
        let quarter_turn_left = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let looking_down = Pose::new(
            quarter_turn_left * Quat::from_rotation_x(-0.6),
            Vec3::new(1.0, 1.7, -2.0),
        );
        let head = gravity_level(looking_down);
        let panel = timeline_panel_pose_facing(head);
        let centre =
            panel.position - panel.transform_vector(Vec3::Y) * (TIMELINE_PANEL_HEIGHT_METERS * 0.5);
        // The head looks along room -X: the centre 0.5 m that way and 0.2 m below the eyes.
        assert!(
            centre.abs_diff_eq(Vec3::new(0.5, 1.5, -2.0), EPSILON),
            "centre {centre}"
        );
        let to_the_eyes = (head.position - centre).normalize();
        assert!(
            panel
                .transform_vector(Vec3::NEG_Z)
                .abs_diff_eq(to_the_eyes, EPSILON)
        );
        assert!(
            panel.transform_vector(Vec3::X).y.abs() < EPSILON,
            "the top edge level"
        );
    }

    #[test]
    fn a_failed_panel_listing_is_retried_after_half_a_second_then_every_half_minute() {
        assert_eq!(listing_retry_delay(1), Duration::from_millis(500));
        assert_eq!(listing_retry_delay(2), Duration::from_secs(30));
        assert_eq!(listing_retry_delay(9), Duration::from_secs(30));
    }

    fn listed(id: u8, display_name: &str, state: GuiPanelState, stream: bool) -> ListedPanel {
        ListedPanel {
            id: GuiPanelId([id; 16]),
            display_name: display_name.to_owned(),
            pose: qaros::Pose {
                orientation: qaros::Quat::IDENTITY,
                position: qaros::Vec3::new(0.0, 0.0, 0.0),
            },
            size: GuiPanelSize::default(),
            state,
            content: if stream {
                PanelContent::Stream(qaros::StreamId([id; 16]))
            } else {
                PanelContent::Uri(format!("http://127.0.0.1:9877/views/{id}"))
            },
        }
    }

    #[test]
    fn a_sync_hides_only_its_own_leftovers_and_names_visible_web_page_panels() {
        use GuiPanelState::{Closed, Hidden, Visible};
        let suffix = ".rerun-sfm.rerun";
        let listed = [
            listed(1, "camera.stream.rerun-sfm.rerun", Visible, true),
            listed(2, "plot.stream.rerun-sfm.rerun", Visible, true),
            listed(3, "chart.stream.rerun-sfm.rerun", Visible, true),
            listed(4, "image.stream.rerun-sfm.rerun", Hidden, true),
            listed(5, "old.stream.rerun-sfm.rerun", Closed, true),
            listed(6, "camera.rerun-sfm.rerun", Visible, false),
            listed(7, "timeline.panel.rerun-sfm.rerun", Visible, false),
            listed(8, "map.rerun-sfm.rerun", Hidden, false),
            listed(9, "timeline.panel-stream.rerun-sfm.rerun", Visible, true),
            listed(10, "camera.stream.other-sfm.rerun", Visible, true),
            listed(11, "a.b.stream.rerun-sfm.rerun", Visible, true),
        ];
        let held = [GuiPanelId([1; 16])];
        let published = ["plot".to_owned()];

        let leftovers = classify_leftovers(&listed, suffix, &held, &published);

        let hidden: Vec<&str> = leftovers
            .stream_panels_to_hide
            .iter()
            .map(|panel| panel.display_name.as_str())
            .collect();
        assert_eq!(hidden, ["chart.stream.rerun-sfm.rerun"]);
        assert_eq!(
            leftovers.web_panels_visible,
            ["camera.rerun-sfm.rerun", "timeline.panel.rerun-sfm.rerun"]
        );
    }

    #[test]
    fn a_mixed_listing_with_web_page_panels_gives_nothing_held_to_hide() {
        use GuiPanelState::Visible;
        let suffix = ".rerun-sfm.rerun";
        let listed = [
            listed(1, "camera.stream.rerun-sfm.rerun", Visible, true),
            listed(2, "camera.rerun-sfm.rerun", Visible, false),
        ];
        let held = [GuiPanelId([1; 16])];
        let leftovers = classify_leftovers(&listed, suffix, &held, &["camera".to_owned()]);
        assert!(leftovers.stream_panels_to_hide.is_empty());
        assert_eq!(leftovers.web_panels_visible, ["camera.rerun-sfm.rerun"]);
    }

    #[test]
    fn the_stream_timeline_panel_leaves_its_image_the_content_area() {
        let size = stream_panel_size(960, 256);
        let content_aspect = (size.height - 0.105) / (size.width - 0.08);
        assert!(
            (content_aspect - 256.0 / 960.0).abs() < 1e-4,
            "{}",
            size.height
        );
        assert!(size.height > timeline_panel_size().height);
    }

    #[test]
    fn the_timeline_panel_is_wide_and_short() {
        let size = timeline_panel_size();
        assert!((size.width - 0.45).abs() < 1e-6);
        assert!((size.height - 0.12).abs() < 1e-6);
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
