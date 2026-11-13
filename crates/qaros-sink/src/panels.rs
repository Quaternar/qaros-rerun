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
use crate::panel_ids::panel_id_of_common_name;
use crate::panel_images::{PanelImages, TIMELINE_IMAGE_KEY};
use crate::panel_listing::{ListedPanel, PanelContent, list_panels};
use crate::peer_devices::PeerDevice;
use crate::stop_watchdog::{TeardownStep, enter_teardown_step};
use crate::stream_panels::{PanelPlace, QarosStreamBackend, StreamPanels};

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
/// Seated: how far from the user's head the view panels stand, measured on the floor plane.
const REACH_DISTANCE_METERS: f32 = 0.8;
/// Seated: how far below the user's eyes the view panels' top edges sit.
const REACH_TOP_BELOW_EYES_METERS: f32 = 0.1;
/// Seated: where the row starts, measured along the arc from the user's gaze to the first
/// panel's left edge, so the panels stand beside what the user looks at rather than in front
/// of it.
const REACH_ROW_START_METERS: f32 = 0.25;
/// Standing: no panel's top edge sits higher than this below the eyes, so the hands work below
/// the chest instead of raised in front of the face (ADR-0083). A view panel hangs from this
/// line as if upright; tilting it toward the eyes lowers its top edge further.
const STANDING_TOP_BELOW_EYES_METERS: f32 = 0.30;
/// Standing: the timeline panel's centre, straight ahead along the gaze on the floor plane.
/// Its slider is the control a user touches most, so it stands nearest. AR glasses see a
/// narrow field of view, so new panels open a step out (ADR-0100), where they are seen at
/// once; the user steps forward to touch them.
const STANDING_TIMELINE_DISTANCE_METERS: f32 = 0.90;
/// Standing: the timeline panel's centre below the eyes, just above elbow height (ANSUR II,
/// elbow 0.52 m below the eyes for women and 0.57 m for men, on average).
const STANDING_TIMELINE_CENTRE_BELOW_EYES_METERS: f32 = 0.50;
/// Standing: the view panels' centres from the eyes on the floor plane, a step out as the
/// timeline (ADR-0100) and a little farther than it: views take no touch, only the occasional
/// grab of a title bar.
const STANDING_VIEW_DISTANCE_METERS: f32 = 0.95;
/// Standing: the space between neighbouring panels, measured along the view panels' arc.
const STANDING_PANEL_GAP_METERS: f32 = 0.05;
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
/// With `--move-far-panels`, a held panel whose centre is farther than this from the eyes of the
/// head that places panels is moved to where this run places new panels (ADR-0090, ADR-0095);
/// without it such a panel stays where it is held. It is a little more than half a step
/// beyond the about 1.05 m from the eyes at which a standing user's new panels open
/// (ADR-0100): a panel a user moved a little within the space they work in stays; one held in
/// another part of the room, or placed in another tracking origin (another headset, or a
/// headset started at another spot), is moved.
const HELD_PLACE_FARTHEST_METERS: f32 = 1.5;
/// A held panel whose centre is nearer than this to the eyes is moved too: it sits at the head,
/// in front of the eyes or behind them, and cannot be read or grabbed.
const HELD_PLACE_NEAREST_METERS: f32 = 0.2;
/// A held panel whose centre is lower than this below the eyes is moved too, with or without
/// `--move-far-panels` (ADR-0101). The tracking origin's y = 0 is no floor (heads stream at
/// y -0.51 to 0.99, ADR-0090), so the floor is told from the eyes: an average standing adult's
/// eyes are 1.51 m (women) to 1.64 m (men) above it (ANSUR II, ADR-0083), so a centre this low
/// is at the floor or under it, and under it for a seated user.
const HELD_PLACE_LOWEST_BELOW_EYES_METERS: f32 = 1.5;
/// A held panel whose width or height differs from this run's by more than this is moved and
/// resized: its place was chosen for another frame (ADR-0087), and a standing view hangs from
/// its top edge by its height (ADR-0083).
const HELD_SIZE_TOLERANCE_METERS: f32 = 0.005;
/// `--reset-panels`: how long after closing a panel the next listing is made, to see the close.
const RESET_CONFIRM_DELAY: Duration = Duration::from_secs(1);
/// `--reset-panels`: listings after the close in which a panel may still be listed open before
/// it is claimed as it is.
const RESET_CONFIRM_LISTINGS: u32 = 3;

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

/// Who new panels are placed for (ADR-0083). Panels that exist keep their place either way.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelPosture {
    /// The panels between chest and waist, tilted toward the eyes: the timeline straight ahead
    /// and nearest, the views beside it, alternately right and left.
    #[default]
    Standing,
    /// At a desk, where panels that low would stand in the desk: the views on an arc 0.8 m
    /// away to the right of the gaze, top edges 0.1 m below the eyes, upright; the timeline
    /// 0.5 m ahead, 0.2 m below the eyes (ADR-0070, ADR-0072).
    Sitting,
}

impl PanelPosture {
    /// For logs: "standing" or "seated".
    fn user_description(self) -> &'static str {
        match self {
            Self::Standing => "standing",
            Self::Sitting => "seated",
        }
    }
}

/// One QAROS GUI panel per published 2D view, each showing a CPU video sender of this viewer
/// fed from the view's image, and with [`QarosPanels::with_timeline_panel`] one timeline panel
/// for the run. Panels go by common names `<view>.stream.<volume>.rerun` and
/// `timeline.panel-stream.<volume>.rerun`.
///
/// The Hub keeps a panel's place across runs and Hub sessions: the pose it was created with, or
/// wherever a user moved it. Every claim waits for a head to place panels by: the first headset
/// user whose head is near the app volume, or any viewer near it once none came for
/// [`HEADSET_WAIT`] ([`QarosPanels::consider_head`]). A new panel is created at its place within
/// reach of that head. A panel an earlier run left is reattached by its common name and stays
/// where the Hub holds it while that place is of use to the user: no nearer than
/// [`HELD_PLACE_NEAREST_METERS`] to the eyes, no lower than
/// [`HELD_PLACE_LOWEST_BELOW_EYES_METERS`] below them, at this run's size, and with
/// [`QarosPanels::with_move_far_panels`] within [`HELD_PLACE_FARTHEST_METERS`] of them.
/// Otherwise it is moved to where a new panel would open (ADR-0090, ADR-0095). A panel
/// listed `Closed` is claimed again by its common name, never replaced under a new one
/// (ADR-0075): QAROS creates it afresh, and the claim shows it at its held place by the same
/// rule (ADR-0091). Nothing else in a run closes a panel: only with
/// [`QarosPanels::with_reset_panels`] does the run close every panel it claims first, which then
/// opens where new panels open. A panel closed while held is claimed again once a target's head
/// streams after the close. When this value drops the panels stay in the session, hidden or kept
/// as [`PanelsAtStop`] says, except those closed while held, which the next run reopens.
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
    posture: PanelPosture,
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
    /// Listings made this run, for the log and to tell a listing made after a reset's close.
    listings_made: u32,
    /// `Some` with `--reset-panels`.
    reset: Option<PanelReset>,
    /// `--move-far-panels`: a held panel farther than [`HELD_PLACE_FARTHEST_METERS`] from the
    /// eyes is moved too.
    move_far_panels: bool,
}

/// What a claim does about `--reset-panels` for one panel.
#[derive(Debug, PartialEq, Eq)]
enum ResetStep {
    Claim,
    WaitForTheClose,
}

/// `--reset-panels`: each panel an earlier run left is closed before its claim, and its claim
/// waits for a listing made after the close. Once one shows the panel closed, or absent, the
/// claim creates it afresh. A panel still listed open after [`RESET_CONFIRM_LISTINGS`] such
/// listings, or one whose close fails, is claimed as it is, with the place check every claim
/// makes.
#[derive(Debug, Default)]
struct PanelReset {
    /// Panel names the reset is over for: closed and seen closed, found absent, or given up on.
    done: BTreeSet<String>,
    /// Panel names closed by the reset, with the count of listings made before the close.
    closes: BTreeMap<String, u32>,
    /// When the listing that is to show the closes is due.
    listing_due: Option<Instant>,
}

impl PanelReset {
    /// `held` is the id of the open stream panel listed for `name`, if any; `listings_made`
    /// counts the listings made so far; `close` closes a panel.
    fn step(
        &mut self,
        name: &str,
        held: Option<GuiPanelId>,
        listings_made: u32,
        now: Instant,
        close: impl FnOnce(GuiPanelId) -> Result<(), String>,
    ) -> ResetStep {
        if self.done.contains(name) {
            return ResetStep::Claim;
        }
        let Some(&listings_at_close) = self.closes.get(name) else {
            let Some(held) = held else {
                self.done.insert(name.to_owned());
                return ResetStep::Claim;
            };
            return match close(held) {
                Ok(()) => {
                    log::info!(
                        "qaros: --reset-panels: closed panel {held} of {name}; it is created \
                         afresh once a listing shows it closed"
                    );
                    self.closes.insert(name.to_owned(), listings_made);
                    self.listing_due = Some(now + RESET_CONFIRM_DELAY);
                    ResetStep::WaitForTheClose
                }
                Err(error) => {
                    log::warn!(
                        "qaros: --reset-panels: cannot close panel {held} of {name}: {error}; it \
                         is claimed as it is"
                    );
                    self.done.insert(name.to_owned());
                    ResetStep::Claim
                }
            };
        };
        let listings_since_close = listings_made.saturating_sub(listings_at_close);
        if listings_since_close == 0 {
            return ResetStep::WaitForTheClose;
        }
        if held.is_none() {
            log::info!("qaros: --reset-panels: the panel of {name} is listed closed; creating it");
            self.done.insert(name.to_owned());
            return ResetStep::Claim;
        }
        if listings_since_close >= RESET_CONFIRM_LISTINGS {
            log::warn!(
                "qaros: --reset-panels: the panel of {name} is still listed open \
                 {listings_since_close} listings after its close; it is claimed as it is"
            );
            self.done.insert(name.to_owned());
            return ResetStep::Claim;
        }
        self.listing_due = Some(now + RESET_CONFIRM_DELAY);
        ResetStep::WaitForTheClose
    }

    /// Whether the claims wait for the listing that is to show the closes (`Some(true)`), make
    /// it now (`Some(false)`), or have none to wait for (`None`).
    fn listing_wait(&mut self, now: Instant) -> Option<bool> {
        let due = self.listing_due?;
        if now < due {
            return Some(true);
        }
        self.listing_due = None;
        Some(false)
    }
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
            posture: PanelPosture::default(),
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
            reset: None,
            move_far_panels: false,
        }
    }

    /// Shows at most one due image on its panel's sender, and retires replaced senders.
    /// Called on every viewer frame (every eye frame with `--decoupled-render`) on the QAROS
    /// thread.
    pub fn pump_streams(&mut self, now: Instant) {
        self.streams.pump(&self.images, now);
    }

    /// Whether a sender put in place has its first frame due at `now`, which holds back every
    /// claim (ADR-0096); the caller syncs again as soon as it is not (ADR-0101).
    pub fn awaits_a_first_frame(&self, now: Instant) -> bool {
        self.streams.awaits_a_first_frame(now)
    }

    /// Also opens one timeline panel, whether or not any view is published, showing the image
    /// published under [`TIMELINE_IMAGE_KEY`] once there is one, with its gestures going to
    /// `gestures`. A new one opens once a head places panels ([`QarosPanels::consider_head`]),
    /// straight ahead of that head and below its eyes, facing them, as [`PanelPosture`] says;
    /// one an earlier run left is reattached, kept and hidden at Stop like the views' panels.
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

    #[must_use]
    pub fn with_panel_posture(mut self, posture: PanelPosture) -> Self {
        self.posture = posture;
        self
    }

    /// `--reset-panels`: before claiming a panel an earlier run left, the run closes it
    /// (`qar_gui_panels_close_panel`, `QAR_GUI_PANEL_STATE_CLOSED`) and waits for a listing that
    /// shows it closed, so that the claim creates it afresh where this run places new panels, at
    /// this run's size. The close comes at the panel's first claim, before the panel shows
    /// anything this run: the run knows a view's common name only once the view settles.
    #[must_use]
    pub fn with_reset_panels(mut self, reset_panels: bool) -> Self {
        self.reset = reset_panels.then(PanelReset::default);
        self
    }

    /// `--move-far-panels`: a panel an earlier run left whose centre is farther than
    /// [`HELD_PLACE_FARTHEST_METERS`] from the eyes of the head that places panels is moved to
    /// where new panels open, as one held at the head or at another frame's size is. Without it
    /// such a panel stays where the Hub holds it, wherever the user stands (ADR-0095).
    #[must_use]
    pub fn with_move_far_panels(mut self, move_far_panels: bool) -> Self {
        self.move_far_panels = move_far_panels;
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
    /// closed while held is released and claimed again once a target's head streams after the
    /// close, with a new listing (ADR-0091).
    ///
    /// Held view panels follow the layout (ADR-0082): one whose view is no longer among
    /// `published_view_names`, because another recording became active or the blueprint
    /// dropped or hid the view, is hidden, and shown again once the view is back among `views`.
    pub fn sync(&mut self, views: &[PanelView], published_view_names: &[String]) {
        let now = Instant::now();
        self.streams.maintain(&self.images, now, stream_panel_size);
        self.release_panels_to_reopen();
        let settled_view_names: Vec<String> = views.iter().map(|view| view.name.clone()).collect();
        self.streams
            .follow_published_views(published_view_names, &settled_view_names);
        let mut wanted: Vec<WantedPanel<'_>> = views.iter().map(WantedPanel::View).collect();
        if self.timeline_gestures.is_some() {
            wanted.push(WantedPanel::Timeline);
        }
        wanted.retain(|panel| {
            !self.panels.contains_key(panel.name())
                && !self.failed.contains(panel.name())
                && !self.waits_for_its_timeline_image(panel)
        });
        // A claim (and a listing) blocks this thread for seconds, and with it the first frame
        // of a sender just put in place, which the mixer's graphics queue waits for (ADR-0096).
        if wanted.is_empty() || self.streams.awaits_a_first_frame(now) {
            return;
        }
        let Some(mut listed) = self.listing_for_claims(now) else {
            return;
        };
        let mut opened_any = false;
        for panel in wanted {
            let name = panel.name().to_owned();
            let Some(anchor) = self.reach_anchor else {
                if self.waiting_logged.insert(name.clone()) {
                    log::info!(
                        "qaros: the panel for {name} opens once a viewer near the app volume \
                         streams: a new panel opens within reach of that viewer, and a held one \
                         is checked against it"
                    );
                }
                continue;
            };
            let held = self.existing_panel(&listed, &name).cloned();
            let held_open = held
                .as_ref()
                .filter(|held| held.state != GuiPanelState::Closed);
            if self.reset_step(&name, held_open, now) == ResetStep::WaitForTheClose {
                continue;
            }
            let slot = self.reach_slots_used;
            let config = self.panel_config(&panel, slot);
            let place = if held_open.is_none() && self.closed_by_reset(&name) {
                PanelPlace::MovedBecause(
                    "--reset-panels closed it to create it afresh where new panels open".to_owned(),
                )
            } else {
                place_for_claim(
                    held.as_ref(),
                    anchor.position,
                    config.size,
                    self.move_far_panels,
                )
            };
            let takes_a_slot = matches!(panel, WantedPanel::View(_))
                && (held.is_none() || matches!(place, PanelPlace::MovedBecause(_)));
            match self.open_stream_panel(&panel, &config, &listed, &place) {
                Ok(id) => {
                    if takes_a_slot {
                        self.reach_slots_used += 1;
                    }
                    self.panels.insert(name, id);
                    opened_any = true;
                    // The next claim waits for this panel's first frame, at a later sync.
                    break;
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
        match self
            .reset
            .as_mut()
            .and_then(|reset| reset.listing_wait(now))
        {
            Some(true) => return None,
            Some(false) => self.listing = None,
            None => {}
        }
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
                self.streams.note_listing(&listed, now);
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

    /// Forgets the claims of the panels closed while held that are due to be reopened, so this
    /// sync claims them again with a new listing, which tells whether each is still closed and
    /// where it is held (ADR-0091).
    fn release_panels_to_reopen(&mut self) {
        let reopen = self.streams.panels_to_reopen();
        if reopen.is_empty() {
            return;
        }
        self.panels.retain(|_, panel| !reopen.contains(panel));
        self.listing = None;
    }

    /// Whether `--reset-panels` closed the panel of `name` this run.
    fn closed_by_reset(&self, name: &str) -> bool {
        self.reset
            .as_ref()
            .is_some_and(|reset| reset.closes.contains_key(name))
    }

    /// `--reset-panels` for the panel of `name`, which `held` is as listed (open), if listed.
    /// Without the flag every panel is claimed at once.
    fn reset_step(&mut self, name: &str, held: Option<&ListedPanel>, now: Instant) -> ResetStep {
        let Some(reset) = &mut self.reset else {
            return ResetStep::Claim;
        };
        let held_stream_panel = held.filter(|held| !matches!(held.content, PanelContent::Uri(_)));
        let streams = &self.streams;
        reset.step(
            name,
            held_stream_panel.map(|held| held.id),
            self.listings_made,
            now,
            |panel| streams.close_for_reset(panel),
        )
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
        config: &GuiPanelConfig,
        listed: &[ListedPanel],
        place: &PanelPlace,
    ) -> Result<GuiPanelId, String> {
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
            .claim(image_key, config, listed, pixels, gestures, place)
    }

    /// Takes `head`, the head of the streaming `user` in room space at `now`, as the place new
    /// panels open by, if no head was taken yet, this one is within
    /// [`REACH_ANCHOR_MAX_DISTANCE_METERS`] of the app volume's centre on the floor plane, and
    /// the user's device places panels: a headset's head at once, any other
    /// once no headset came for [`HEADSET_WAIT`] after the first head near the volume. New
    /// panels then open within reach of it, at the next [`QarosPanels::sync`]. Existing panels
    /// are not touched: the Hub keeps their place.
    pub fn consider_head(&mut self, user: UserId, head: Pose, now: Instant) {
        self.streams.note_target_head(now);
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
                "qaros: new panels open within reach of {viewer}, at {}, placed for a {} user",
                anchor.position,
                self.posture.user_description()
            );
        } else {
            log::info!(
                "qaros: no headset near the app volume for {HEADSET_WAIT:?}; new panels open \
                 within reach of {viewer}, at {}, placed for a {} user",
                anchor.position,
                self.posture.user_description()
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
            WantedPanel::View(view) => {
                let size = stream_panel_size(view.width_in_pixel, view.height_in_pixel);
                (self.panel_pose(slot, size.height), size)
            }
            WantedPanel::Timeline => {
                let size = self
                    .images
                    .latest(TIMELINE_IMAGE_KEY)
                    .map_or_else(timeline_panel_size, |image| {
                        stream_panel_size(image.width, image.height)
                    });
                (self.timeline_panel_pose(size.height), size)
            }
        };
        GuiPanelConfig {
            display_name: Some(common_name.clone()),
            pose: to_qaros_pose(pose),
            size,
            ..GuiPanelConfig::new(common_name)
        }
    }

    /// The panel of `name` in this app volume, if the session lists one, `Closed` included: found
    /// by the id QAROS derives from the common name, since a panel the Hub carried into a new
    /// session is listed with an empty display name, or by its display name. A closed panel's
    /// held place decides where its claim reopens it (ADR-0091).
    fn existing_panel<'listed>(
        &self,
        listed: &'listed [ListedPanel],
        name: &str,
    ) -> Option<&'listed ListedPanel> {
        let common_name = self.common_name(name);
        let id = panel_id_of_common_name(&common_name);
        listed
            .iter()
            .find(|panel| panel.id == id || panel.display_name == common_name)
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

    fn panel_pose(&self, slot: usize, panel_height: f32) -> Pose {
        if let Some(anchor) = self.reach_anchor {
            return match self.posture {
                PanelPosture::Standing => standing_view_panel_pose(anchor, slot, panel_height),
                PanelPosture::Sitting => panel_pose_within_reach(anchor, slot),
            };
        }
        let volume = self.connection.volume_config();
        panel_pose_beside_volume(from_qaros_pose(volume.pose), volume.size, slot)
    }

    /// Without a head a new panel does not open, and an existing panel keeps its place
    /// whatever pose it is given, so the fallback is never seen.
    fn timeline_panel_pose(&self, panel_height: f32) -> Pose {
        match (self.reach_anchor, self.posture) {
            (Some(anchor), PanelPosture::Standing) => {
                standing_timeline_panel_pose(anchor, panel_height)
            }
            (Some(anchor), PanelPosture::Sitting) => timeline_panel_pose_facing(anchor),
            (None, _) => self.panel_pose(0, panel_height),
        }
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
    pose_facing_the_eyes(head.position, centre, TIMELINE_PANEL_HEIGHT_METERS)
}

/// Room pose, by its top edge's midpoint, of a panel `panel_height` tall whose centre stands at
/// `centre` and whose front faces `eyes`: it turns about +Y to the eyes and tilts back about
/// its top edge's axis, so the top edge stays level. The mixer turns a panel to face the
/// viewing head once the head leaves a 15 degree cone about its front, so a panel created
/// facing the eyes keeps its pose instead of swinging at its first frame.
fn pose_facing_the_eyes(eyes: Vec3, centre: Vec3, panel_height: f32) -> Pose {
    let away_from_eyes = centre - eyes;
    let level_distance = Vec3::new(away_from_eyes.x, 0.0, away_from_eyes.z).length();
    let yaw = away_from_eyes.x.atan2(away_from_eyes.z);
    let tilt = (-away_from_eyes.y).atan2(level_distance);
    // Panel +Z points away from the eyes, so its -Z front faces them.
    let orientation = Quat::from_rotation_y(yaw) * Quat::from_rotation_x(tilt);
    let top_centre = centre + orientation * Vec3::Y * (panel_height * 0.5);
    Pose::new(orientation, top_centre)
}

/// Standing: the timeline panel's room pose for `head` (gravity-level), its centre
/// [`STANDING_TIMELINE_DISTANCE_METERS`] straight ahead and
/// [`STANDING_TIMELINE_CENTRE_BELOW_EYES_METERS`] below the eyes, facing them.
fn standing_timeline_panel_pose(head: Pose, panel_height: f32) -> Pose {
    let forward = head.transform_vector(Vec3::NEG_Z);
    let centre = head.position + forward * STANDING_TIMELINE_DISTANCE_METERS
        - Vec3::Y * STANDING_TIMELINE_CENTRE_BELOW_EYES_METERS;
    pose_facing_the_eyes(head.position, centre, panel_height)
}

/// Standing: the room pose of the view panel in `slot`, `panel_height` tall, for `head`
/// (gravity-level). Its centre stands [`STANDING_VIEW_DISTANCE_METERS`] from the eyes on the
/// floor plane and [`STANDING_TOP_BELOW_EYES_METERS`] plus half its height below them, and it
/// faces the eyes. Even slots go right of the timeline and odd ones left, each pair one place
/// farther out, [`STANDING_PANEL_GAP_METERS`] apart along the arc, so the panels a run opens
/// first stand nearest the gaze.
fn standing_view_panel_pose(head: Pose, slot: usize, panel_height: f32) -> Pose {
    let half_width = PANEL_WIDTH_METERS * 0.5;
    let timeline_half_angle = half_width.atan2(STANDING_TIMELINE_DISTANCE_METERS);
    let view_half_angle = half_width.atan2(STANDING_VIEW_DISTANCE_METERS);
    let gap_angle = STANDING_PANEL_GAP_METERS / STANDING_VIEW_DISTANCE_METERS;
    #[allow(clippy::cast_precision_loss)]
    let places_out = (slot / 2) as f32;
    let angle_from_gaze = timeline_half_angle
        + gap_angle
        + view_half_angle
        + places_out * (2.0 * view_half_angle + gap_angle);
    let toward_the_right = if slot.is_multiple_of(2) { 1.0 } else { -1.0 };
    let angle_to_the_right = toward_the_right * angle_from_gaze;
    let forward = head.transform_vector(Vec3::NEG_Z);
    let right = head.transform_vector(Vec3::X);
    let away_from_head = forward * angle_to_the_right.cos() + right * angle_to_the_right.sin();
    let centre = head.position + away_from_head * STANDING_VIEW_DISTANCE_METERS
        - Vec3::Y * (STANDING_TOP_BELOW_EYES_METERS + panel_height * 0.5);
    pose_facing_the_eyes(head.position, centre, panel_height)
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

/// Where a claim leaves the panel `held` is, as listed, for the head whose eyes are at `eyes`
/// and at `wanted_size`: a new panel (`None`) opens at its config's pose, a held one stays
/// unless [`unusable_held_place`] has a reason to move it.
fn place_for_claim(
    held: Option<&ListedPanel>,
    eyes: Vec3,
    wanted_size: GuiPanelSize,
    move_far_panels: bool,
) -> PanelPlace {
    let Some(held) = held else {
        return PanelPlace::AsHeld;
    };
    if let Some(reason) = unusable_held_place(
        from_qaros_pose(held.pose),
        held.size,
        eyes,
        wanted_size,
        move_far_panels,
    ) {
        return PanelPlace::MovedBecause(reason);
    }
    log::info!(
        "qaros: panel {} stays where it is held: {}",
        held.id,
        describe_held_place(from_qaros_pose(held.pose), held.size, eyes)
    );
    PanelPlace::AsHeld
}

/// Why a panel held at `held_pose` (its top edge's midpoint) and `held_size` is no use to the
/// user whose eyes are at `eyes`, or `None` when it is: its centre nearer to the eyes than
/// [`HELD_PLACE_NEAREST_METERS`], with `move_far_panels` farther than
/// [`HELD_PLACE_FARTHEST_METERS`], lower than [`HELD_PLACE_LOWEST_BELOW_EYES_METERS`] below
/// them, or its width or height more than [`HELD_SIZE_TOLERANCE_METERS`] off `wanted_size`.
fn unusable_held_place(
    held_pose: Pose,
    held_size: GuiPanelSize,
    eyes: Vec3,
    wanted_size: GuiPanelSize,
    move_far_panels: bool,
) -> Option<String> {
    let distance = panel_centre(held_pose, held_size.height).distance(eyes);
    let held_place = describe_held_place(held_pose, held_size, eyes);
    if move_far_panels && distance > HELD_PLACE_FARTHEST_METERS {
        return Some(format!(
            "{held_place}, farther than {HELD_PLACE_FARTHEST_METERS} m from the eyes of the head \
             that places panels"
        ));
    }
    if distance < HELD_PLACE_NEAREST_METERS {
        return Some(format!(
            "{held_place}, nearer than {HELD_PLACE_NEAREST_METERS} m to the eyes of the head that \
             places panels"
        ));
    }
    let centre_below_eyes = eyes.y - panel_centre(held_pose, held_size.height).y;
    if centre_below_eyes > HELD_PLACE_LOWEST_BELOW_EYES_METERS {
        return Some(format!(
            "{held_place}, its centre {centre_below_eyes:.2} m below the eyes, lower than \
             {HELD_PLACE_LOWEST_BELOW_EYES_METERS} m: at the floor or under it"
        ));
    }
    let size_differs = (held_size.width - wanted_size.width).abs() > HELD_SIZE_TOLERANCE_METERS
        || (held_size.height - wanted_size.height).abs() > HELD_SIZE_TOLERANCE_METERS;
    size_differs.then(|| {
        format!(
            "{held_place}, sized for another frame than this run's {:.3}x{:.3} m",
            wanted_size.width, wanted_size.height
        )
    })
}

/// The centre of a panel `height` tall whose top edge's midpoint is at `pose`: the surface hangs
/// below its pose along panel -Y (header `QarGuiPanelSize`).
fn panel_centre(pose: Pose, height: f32) -> Vec3 {
    pose.position + pose.orientation * Vec3::new(0.0, -height * 0.5, 0.0)
}

/// For logs: "held at [x, y, z] with its top edge, 0.450x0.287 m, its centre 0.65 m from the
/// eyes".
fn describe_held_place(held_pose: Pose, held_size: GuiPanelSize, eyes: Vec3) -> String {
    format!(
        "held at {} with its top edge, {:.3}x{:.3} m, its centre {:.2} m from the eyes",
        held_pose.position,
        held_size.width,
        held_size.height,
        panel_centre(held_pose, held_size.height).distance(eyes)
    )
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

    /// ANSUR II (2012, U.S. Army), means below standing eye height: chest height 0.35 m for both
    /// sexes, waist height at the navel 0.54 m for women and 0.59 m for men.
    const CHEST_BELOW_EYES_METERS: f32 = 0.35;
    const WAIST_BELOW_EYES_METERS: f32 = 0.56;
    /// The 1016x665 view of the measured sessions, and the timeline's 960x256 image.
    const VIEW_HEIGHT_METERS: f32 = 0.105 + 0.37 * 665.0 / 1016.0;
    const TIMELINE_HEIGHT_METERS: f32 = 0.105 + 0.37 * 256.0 / 960.0;

    struct PlacedPanel {
        centre: Vec3,
        top_below_eyes: f32,
        centre_below_eyes: f32,
        distance_on_the_floor: f32,
        /// Angle of the panel's front above the horizon, at most 60 degrees in the mixer.
        front_elevation_degrees: f32,
        /// Positive right of the gaze.
        degrees_right_of_gaze: f32,
    }

    fn measure(head: Pose, panel: Pose, panel_height: f32) -> PlacedPanel {
        let centre = panel.position - panel.transform_vector(Vec3::Y) * (panel_height * 0.5);
        let to_centre = centre - head.position;
        let on_the_floor = Vec3::new(to_centre.x, 0.0, to_centre.z);
        let front_normal = panel.transform_vector(Vec3::NEG_Z);
        let to_the_eyes = (head.position - centre).normalize();
        assert!(
            front_normal.abs_diff_eq(to_the_eyes, EPSILON),
            "front {front_normal} faces the eyes along {to_the_eyes}"
        );
        assert!(
            panel.transform_vector(Vec3::X).y.abs() < EPSILON,
            "the top edge level"
        );
        let forward = head.transform_vector(Vec3::NEG_Z);
        let right = head.transform_vector(Vec3::X);
        PlacedPanel {
            centre,
            top_below_eyes: head.position.y - panel.position.y,
            centre_below_eyes: -to_centre.y,
            distance_on_the_floor: on_the_floor.length(),
            front_elevation_degrees: front_normal.y.asin().to_degrees(),
            degrees_right_of_gaze: on_the_floor
                .dot(right)
                .atan2(on_the_floor.dot(forward))
                .to_degrees(),
        }
    }

    fn assert_standing_ergonomics(placed: &PlacedPanel) {
        assert!(
            placed.top_below_eyes >= STANDING_TOP_BELOW_EYES_METERS - EPSILON,
            "top edge {} m below the eyes",
            placed.top_below_eyes
        );
        assert!(
            (CHEST_BELOW_EYES_METERS..=WAIST_BELOW_EYES_METERS).contains(&placed.centre_below_eyes),
            "centre {} m below the eyes, outside chest to waist",
            placed.centre_below_eyes
        );
        assert!(
            (0.90 - EPSILON..=0.95 + EPSILON).contains(&placed.distance_on_the_floor),
            "{} m from the eyes on the floor plane",
            placed.distance_on_the_floor
        );
        assert!(
            placed.front_elevation_degrees <= 60.0,
            "front {} degrees above the horizon, past the mixer's tilt limit",
            placed.front_elevation_degrees
        );
    }

    #[test]
    fn standing_the_timeline_stands_straight_ahead_just_above_elbow_height() {
        let head = Pose::from_translation(Vec3::new(0.0, 1.6, 0.0));
        let timeline = measure(
            head,
            standing_timeline_panel_pose(head, TIMELINE_HEIGHT_METERS),
            TIMELINE_HEIGHT_METERS,
        );
        assert_standing_ergonomics(&timeline);
        assert!(
            timeline
                .centre
                .abs_diff_eq(Vec3::new(0.0, 1.1, -0.9), EPSILON),
            "centre {}",
            timeline.centre
        );
        // Tilted back by atan(0.5 / 0.9), about 29 degrees, the top edge rises
        // 0.102 * cos(29.1) m above the centre: 0.411 m below the eyes.
        assert!(
            (timeline.top_below_eyes - 0.411).abs() < 1e-3,
            "{}",
            timeline.top_below_eyes
        );
        assert!((timeline.front_elevation_degrees - 29.05).abs() < 0.01);
    }

    #[test]
    fn standing_one_view_hangs_from_thirty_centimetres_below_the_eyes_right_of_the_timeline() {
        let head = Pose::from_translation(Vec3::new(0.0, 1.6, 0.0));
        let view = measure(
            head,
            standing_view_panel_pose(head, 0, VIEW_HEIGHT_METERS),
            VIEW_HEIGHT_METERS,
        );
        assert_standing_ergonomics(&view);
        // 0.30 m plus half of a 0.347 m panel.
        assert!(
            (view.centre_below_eyes - 0.474).abs() < 1e-3,
            "{}",
            view.centre_below_eyes
        );
        assert!((view.distance_on_the_floor - 0.95).abs() < EPSILON);
        // Clear of the timeline: atan(0.225 / 0.9) + 0.05 / 0.95 + atan(0.225 / 0.95), 30 degrees.
        assert!(
            (view.degrees_right_of_gaze - 30.4).abs() < 0.1,
            "{}",
            view.degrees_right_of_gaze
        );
    }

    #[test]
    fn standing_several_views_alternate_right_and_left_of_the_timeline_without_touching() {
        let head = Pose::from_translation(Vec3::new(0.0, 1.6, 0.0));
        let timeline_pose = standing_timeline_panel_pose(head, TIMELINE_HEIGHT_METERS);
        let timeline = measure(head, timeline_pose, TIMELINE_HEIGHT_METERS);
        assert_standing_ergonomics(&timeline);
        let view_poses: Vec<Pose> = (0..4)
            .map(|slot| standing_view_panel_pose(head, slot, VIEW_HEIGHT_METERS))
            .collect();
        let views: Vec<PlacedPanel> = view_poses
            .iter()
            .map(|pose| measure(head, *pose, VIEW_HEIGHT_METERS))
            .collect();
        for view in &views {
            assert_standing_ergonomics(view);
        }
        let sides: Vec<bool> = views
            .iter()
            .map(|view| view.degrees_right_of_gaze > 0.0)
            .collect();
        assert_eq!(
            sides,
            [true, false, true, false],
            "right, left, right, left"
        );
        assert!(
            (views[0].degrees_right_of_gaze + views[1].degrees_right_of_gaze).abs() < 0.01,
            "the first two mirror each other"
        );
        assert!(views[2].degrees_right_of_gaze > views[0].degrees_right_of_gaze);
        // Neighbours stand the gap apart at their nearest edges: the timeline's ends against
        // the first two views', and each view's outer end against the next one out. Panel +X
        // runs along the top edge to the viewer's left.
        let half_width = PANEL_WIDTH_METERS * 0.5;
        let end = |pose: Pose, toward_viewer_left: f32| {
            pose.position + pose.transform_vector(Vec3::X) * (half_width * toward_viewer_left)
        };
        let nearest_ends = [
            (end(timeline_pose, -1.0), end(view_poses[0], 1.0)),
            (end(timeline_pose, 1.0), end(view_poses[1], -1.0)),
            (end(view_poses[0], -1.0), end(view_poses[2], 1.0)),
            (end(view_poses[1], 1.0), end(view_poses[3], -1.0)),
        ];
        for (inner, outer) in nearest_ends {
            let apart = Vec3::new(outer.x - inner.x, 0.0, outer.z - inner.z).length();
            assert!(apart >= STANDING_PANEL_GAP_METERS, "{apart} m apart");
        }
    }

    #[test]
    fn standing_a_square_view_still_centres_above_the_waist() {
        let head = Pose::from_translation(Vec3::new(0.0, 1.6, 0.0));
        let square_height = stream_panel_size(800, 800).height;
        let view = measure(
            head,
            standing_view_panel_pose(head, 1, square_height),
            square_height,
        );
        assert_standing_ergonomics(&view);
    }

    #[test]
    fn standing_panels_follow_a_turned_tilted_moved_head_by_its_heading() {
        let quarter_turn_left = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let looking_down = Pose::new(
            quarter_turn_left * Quat::from_rotation_x(-0.6),
            Vec3::new(1.0, 1.7, -2.0),
        );
        let head = gravity_level(looking_down);
        let timeline = measure(
            head,
            standing_timeline_panel_pose(head, TIMELINE_HEIGHT_METERS),
            TIMELINE_HEIGHT_METERS,
        );
        assert_standing_ergonomics(&timeline);
        // The head looks along room -X: the centre 0.9 m that way and 0.5 m below the eyes.
        assert!(
            timeline
                .centre
                .abs_diff_eq(Vec3::new(0.1, 1.2, -2.0), EPSILON),
            "centre {}",
            timeline.centre
        );
        let view = measure(
            head,
            standing_view_panel_pose(head, 0, VIEW_HEIGHT_METERS),
            VIEW_HEIGHT_METERS,
        );
        assert_standing_ergonomics(&view);
        assert!(view.centre.x < head.position.x, "in front of the head");
        assert!(view.centre.z < head.position.z, "to the head's right");
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

    /// The METALENSE head that placed panels on 2026-11-02 at 16:11:53 UTC, and the size the
    /// camera view asked for on its 960x472 frame.
    fn metalense_head() -> Pose {
        Pose::new(Quat::IDENTITY, Vec3::new(-0.151, -0.514, 0.493))
    }

    fn camera_frame_size() -> GuiPanelSize {
        stream_panel_size(960, 472)
    }

    #[test]
    fn a_panel_placed_for_this_head_or_moved_within_a_step_stays_where_it_is_held() {
        let head = metalense_head();
        let size = camera_frame_size();
        let placed_for_this_head = standing_view_panel_pose(head, 0, size.height);
        assert_eq!(
            unusable_held_place(placed_for_this_head, size, head.position, size, true),
            None
        );
        let moved_by_the_user = Pose::new(
            placed_for_this_head.orientation,
            placed_for_this_head.position + Vec3::new(0.3, 0.1, 0.2),
        );
        let distance = panel_centre(moved_by_the_user, size.height).distance(head.position);
        assert!(
            (1.0..HELD_PLACE_FARTHEST_METERS).contains(&distance),
            "{distance}"
        );
        assert_eq!(
            unusable_held_place(moved_by_the_user, size, head.position, size, true),
            None
        );
    }

    /// Placed at 11:02 UTC on 2026-11-02 by a METALENSE head at (-1.77, 0.42, 0.33) in another
    /// tracking origin: its centre 1.59 m from the eyes of [`metalense_head`].
    fn placed_in_another_origin() -> Pose {
        panel_pose_within_reach(
            gravity_level(Pose::new(Quat::IDENTITY, Vec3::new(-1.774, 0.417, 0.334))),
            0,
        )
    }

    #[test]
    fn with_move_far_panels_a_panel_held_beyond_a_step_and_a_reach_is_moved() {
        let head = metalense_head();
        let size = camera_frame_size();
        let far = unusable_held_place(placed_in_another_origin(), size, head.position, size, true)
            .expect("moved");
        assert!(far.contains("farther than 1.5 m"), "{far}");
        assert!(far.contains("its centre 1.59 m from the eyes"), "{far}");
    }

    #[test]
    fn without_move_far_panels_a_panel_held_beyond_a_step_and_a_reach_stays() {
        let head = metalense_head();
        let size = camera_frame_size();
        let distance =
            panel_centre(placed_in_another_origin(), size.height).distance(head.position);
        assert!(distance > HELD_PLACE_FARTHEST_METERS, "{distance}");
        assert_eq!(
            unusable_held_place(placed_in_another_origin(), size, head.position, size, false),
            None
        );
    }

    #[test]
    fn a_panel_held_at_the_head_is_moved_with_or_without_move_far_panels() {
        let head = metalense_head();
        let size = camera_frame_size();
        let at_the_head = Pose::new(Quat::IDENTITY, head.position + Vec3::new(0.0, 0.1, -0.05));
        for move_far_panels in [false, true] {
            let near = unusable_held_place(at_the_head, size, head.position, size, move_far_panels)
                .expect("moved");
            assert!(near.contains("nearer than 0.2 m"), "{near}");
        }
    }

    /// The drone lidar run of 2026-11-03 at 19:56:07 UTC: the head that placed panels, and the
    /// timeline panel an earlier run left 4.37 m away, under the floor.
    fn drone_lidar_head() -> Vec3 {
        Vec3::new(-0.084_732_41, 0.825_657_5, 2.262_562_8)
    }

    /// The timeline panel's size in that run's log, 0.450x0.204 m.
    fn logged_timeline_size() -> GuiPanelSize {
        GuiPanelSize {
            width: 0.45,
            height: 0.204,
            content_scale: 1.0,
        }
    }

    fn held_under_the_floor() -> Pose {
        Pose::new(
            Quat::IDENTITY,
            Vec3::new(1.352_494_5, -0.975_105_3, -1.467_721_5),
        )
    }

    #[test]
    fn a_panel_held_under_the_floor_is_moved_with_or_without_move_far_panels() {
        let size = logged_timeline_size();
        let without_the_flag = unusable_held_place(
            held_under_the_floor(),
            size,
            drone_lidar_head(),
            size,
            false,
        )
        .expect("moved");
        assert!(
            without_the_flag.contains("its centre 1.90 m below the eyes, lower than 1.5 m"),
            "{without_the_flag}"
        );
        assert!(
            without_the_flag.contains("at the floor or under it"),
            "{without_the_flag}"
        );
        let with_the_flag =
            unusable_held_place(held_under_the_floor(), size, drone_lidar_head(), size, true);
        assert!(with_the_flag.is_some());
    }

    #[test]
    fn a_panel_held_low_but_above_the_floor_stays() {
        let size = logged_timeline_size();
        let eyes = drone_lidar_head();
        let centre_below_eyes = 1.45;
        let top_edge = Vec3::new(
            eyes.x,
            eyes.y - centre_below_eyes + size.height * 0.5,
            eyes.z - 0.8,
        );
        let low = Pose::new(Quat::IDENTITY, top_edge);
        assert!((eyes.y - panel_centre(low, size.height).y - centre_below_eyes).abs() < EPSILON);
        assert_eq!(unusable_held_place(low, size, eyes, size, false), None);
    }

    #[test]
    fn a_panel_held_at_another_frame_s_size_is_moved_and_resized() {
        let head = metalense_head();
        let wanted = camera_frame_size();
        let held_size = stream_panel_size(1016, 665);
        let placed = standing_view_panel_pose(head, 0, held_size.height);
        let reason =
            unusable_held_place(placed, held_size, head.position, wanted, false).expect("moved");
        assert!(reason.contains("0.450x0.287 m"), "{reason}");
        let within_tolerance = GuiPanelSize {
            height: wanted.height + 0.004,
            ..wanted
        };
        assert_eq!(
            unusable_held_place(placed, within_tolerance, head.position, wanted, false),
            None
        );
    }

    #[test]
    fn a_claim_moves_only_a_held_panel_whose_place_is_of_no_use() {
        let head = metalense_head();
        let size = camera_frame_size();
        assert_eq!(
            place_for_claim(None, head.position, size, false),
            PanelPlace::AsHeld,
            "a new panel opens at its config's pose"
        );
        let mut earlier_panel = listed(1, "", GuiPanelState::Hidden, true);
        earlier_panel.size = size;
        earlier_panel.pose = to_qaros_pose(standing_view_panel_pose(head, 0, size.height));
        assert_eq!(
            place_for_claim(Some(&earlier_panel), head.position, size, true),
            PanelPlace::AsHeld
        );
        earlier_panel.pose = to_qaros_pose(Pose::from_translation(Vec3::new(3.0, 0.0, -4.0)));
        assert_eq!(
            place_for_claim(Some(&earlier_panel), head.position, size, false),
            PanelPlace::AsHeld,
            "a far panel stays without --move-far-panels"
        );
        assert!(matches!(
            place_for_claim(Some(&earlier_panel), head.position, size, true),
            PanelPlace::MovedBecause(reason) if reason.contains("farther than 1.5 m")
        ));
        earlier_panel.pose = to_qaros_pose(Pose::IDENTITY);
        earlier_panel.size = GuiPanelSize {
            width: 0.0,
            height: 0.0,
            ..size
        };
        assert!(
            matches!(
                place_for_claim(Some(&earlier_panel), head.position, size, false),
                PanelPlace::MovedBecause(reason) if reason.contains("sized for another frame")
            ),
            "a panel listed Closed with no place left is moved without --move-far-panels"
        );
    }

    #[test]
    fn the_camera_frame_keeps_its_aspect_on_the_panel_s_content_area() {
        let size = camera_frame_size();
        assert!((size.width - 0.45).abs() < 1e-6);
        assert!((size.height - 0.287).abs() < 0.0005, "{}", size.height);
        let content_width = size.width - STREAM_PANEL_HORIZONTAL_CHROME_METERS;
        let content_height = size.height - STREAM_PANEL_VERTICAL_CHROME_METERS;
        let frame_aspect = 960.0 / 472.0;
        assert!(
            (content_width / content_height - frame_aspect).abs() < 1e-3,
            "{} against {frame_aspect}",
            content_width / content_height
        );
    }

    const CAMERA_PANEL: GuiPanelId = GuiPanelId([0x64; 16]);

    #[test]
    fn a_reset_closes_a_held_panel_then_claims_once_a_later_listing_shows_it_closed() {
        let mut reset = PanelReset::default();
        let start = Instant::now();
        let mut closed = Vec::new();
        let step = reset.step("camera", Some(CAMERA_PANEL), 1, start, |panel| {
            closed.push(panel);
            Ok(())
        });
        assert_eq!(step, ResetStep::WaitForTheClose);
        assert_eq!(closed, [CAMERA_PANEL]);
        assert_eq!(reset.listing_wait(start), Some(true));

        let unexpected_close = |_| -> Result<(), String> { panic!("closed twice") };
        assert_eq!(
            reset.step("camera", Some(CAMERA_PANEL), 1, start, unexpected_close),
            ResetStep::WaitForTheClose,
            "the listing made before the close does not count"
        );
        assert_eq!(reset.listing_wait(start + RESET_CONFIRM_DELAY), Some(false));
        assert_eq!(reset.listing_wait(start + RESET_CONFIRM_DELAY), None);
        assert_eq!(
            reset.step(
                "camera",
                None,
                2,
                start + RESET_CONFIRM_DELAY,
                unexpected_close
            ),
            ResetStep::Claim,
            "listed closed after the close: created afresh"
        );
        assert_eq!(
            reset.step("camera", Some(CAMERA_PANEL), 3, start, unexpected_close),
            ResetStep::Claim,
            "once per run"
        );
    }

    #[test]
    fn a_reset_claims_an_absent_panel_at_once_and_gives_up_on_one_that_stays_open() {
        let mut reset = PanelReset::default();
        let now = Instant::now();
        let unexpected_close = |_| -> Result<(), String> { panic!("nothing to close") };
        assert_eq!(
            reset.step("plot", None, 1, now, unexpected_close),
            ResetStep::Claim
        );

        reset.step("camera", Some(CAMERA_PANEL), 1, now, |_| Ok(()));
        for listings_made in 2..=3 {
            assert_eq!(
                reset.step(
                    "camera",
                    Some(CAMERA_PANEL),
                    listings_made,
                    now,
                    unexpected_close
                ),
                ResetStep::WaitForTheClose
            );
        }
        assert_eq!(
            reset.step(
                "camera",
                Some(CAMERA_PANEL),
                1 + RESET_CONFIRM_LISTINGS,
                now,
                unexpected_close
            ),
            ResetStep::Claim
        );
    }

    #[test]
    fn a_reset_whose_close_fails_claims_the_panel_as_it_is() {
        let mut reset = PanelReset::default();
        let step = reset.step("camera", Some(CAMERA_PANEL), 1, Instant::now(), |_| {
            Err("QAR_STATUS_GUI_PANEL_INVALID_ID".to_owned())
        });
        assert_eq!(step, ResetStep::Claim);
        assert_eq!(reset.listing_wait(Instant::now()), None);
    }
}
