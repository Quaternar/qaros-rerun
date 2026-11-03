//! The stream panels: one video sender per claimed panel, fed from [`PanelImages`] on
//! the QAROS thread, a replacement sender when the image's size settles on a new value or the
//! sender fails, and the timeline panel's gesture subscription.
//!
//! Every QAROS call goes through [`StreamBackend`], so the claim, the shows, the replacements
//! and the Stop order are tested with a fake on hosts without a mixer (`qaros-stub` implements
//! none of the video sender, `set_stream` or gesture calls).

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use qaros::{GuiPanelConfig, GuiPanelId, GuiPanelSize, GuiPanelState, Pose, StreamId};

use crate::connection::QarosConnection;
use crate::panel_gestures::{PanelGestureSubscription, PanelGestures};
use crate::panel_images::{PanelImage, PanelImages, TIMELINE_IMAGE_KEY};
use crate::panel_listing::{ListedPanel, PanelContent};
use crate::panel_state_watch::PanelStateWatch;
use crate::stream_panel_calls;
use crate::video_sender::VideoSender;

/// An unchanged image is shown again this long after its last show: `try_begin_frame` reserves
/// only receivers ready at that moment (header L4170-4178), so a receiver that connects later
/// (a mixer restart, a second headset) gets the image within this time.
pub const STREAM_REFRESH: Duration = Duration::from_secs(1);

/// How long a replaced sender stays alive at most when its successor never has a ready
/// begin. The mixer erases the old receiver only once the new stream's layout is in the
/// register (`EnsureStreamReceiver`, `BrowserControl.hpp` L793-829 at `c6e052d3`), and an old
/// sender that dies first queues a close of the panel (L1093-1101).
pub const SENDER_RETIRE_LIMIT: Duration = Duration::from_secs(10);

/// The least time between two replacements of one panel's sender.
const REPLACEMENT_SPACING: Duration = Duration::from_secs(2);

/// How long a sender just put in place holds back the next claim or replacement, until its
/// first frame is shown (ADR-0096). Once the mixer connects a panel's new stream, its graphics
/// queue waits for that stream's first frame, and a wait past Windows' two-second GPU timeout
/// resets the mixer's device; a claim blocks this thread for about 2 s, so it must not start
/// before the first frame is out. A sender with no ready begin by then (no mixer connected
/// it) stops holding the others back.
pub const FIRST_FRAME_WAIT: Duration = Duration::from_secs(3);

/// How often per run a failed sender is replaced before the panel is left as it is.
const FAILURE_REPLACEMENT_LIMIT: u32 = 3;

/// How often per run a panel closed while held is reopened. A mixer that fails on a panel at
/// every connect closes it again each time; past this the panel is left closed until the next
/// run, which reopens it at its claim.
const REOPEN_LIMIT: u32 = 3;

/// Listings in a row a held panel must be missing from before it counts as gone: a listing
/// that drops a live panel once does not stop its stream.
const ABSENT_LISTINGS_BEFORE_GONE: u32 = 2;

/// A panel's video sender as the panels use it.
pub(crate) trait StreamSender {
    fn stream_id(&self) -> StreamId;
    fn size(&self) -> (u32, u32);
    /// One frame cycle with `bgra8`, which has the sender's size. `Ok(false)` when no receiver
    /// is ready now.
    fn show_bgra8(&mut self, bgra8: &[u8], width: u32, height: u32) -> Result<bool, String>;
}

/// Whether a panel was closed by someone else after this run claimed it.
pub(crate) trait ClosedElsewhere {
    fn closed_after_open(&self) -> bool;
}

/// Every QAROS call of the stream panels.
pub(crate) trait StreamBackend {
    type Sender: StreamSender;
    type Subscription;
    type StateWatch: ClosedElsewhere;

    fn create_sender(&self, width: u32, height: u32) -> Result<Self::Sender, String>;
    fn get_or_create_stream(
        &self,
        config: &GuiPanelConfig,
        stream: StreamId,
    ) -> Result<GuiPanelId, String>;
    fn set_stream(&self, panel: GuiPanelId, stream: StreamId) -> Result<(), String>;
    fn set_size(&self, panel: GuiPanelId, size: GuiPanelSize) -> Result<(), String>;
    fn set_pose(&self, panel: GuiPanelId, pose: Pose) -> Result<(), String>;
    fn set_state(&self, panel: GuiPanelId, state: GuiPanelState) -> Result<(), String>;
    /// `qar_gui_panels_close_panel`: `QAR_GUI_PANEL_STATE_CLOSED`, after which
    /// `get_or_create` builds the panel afresh from its init.
    fn close_panel(&self, panel: GuiPanelId) -> Result<(), String>;
    fn subscribe_gestures(
        &self,
        panel: GuiPanelId,
        inbox: PanelGestures,
    ) -> Result<Self::Subscription, String>;
    fn watch_state(&self, panel: GuiPanelId) -> Result<Self::StateWatch, String>;
}

/// The backend of a real session.
pub(crate) struct QarosStreamBackend {
    pub(crate) connection: Rc<QarosConnection>,
}

impl StreamSender for VideoSender {
    fn stream_id(&self) -> StreamId {
        VideoSender::stream_id(self)
    }

    fn size(&self) -> (u32, u32) {
        VideoSender::size(self)
    }

    fn show_bgra8(&mut self, bgra8: &[u8], width: u32, height: u32) -> Result<bool, String> {
        let stream_id = VideoSender::stream_id(self);
        let Some(mut cycle) = self.try_begin()? else {
            return Ok(false);
        };
        let started = Instant::now();
        // A failed copy still shows: a begun cycle cannot be abandoned. The size is checked
        // before the begin, so this is a bug, and logged as one.
        if let Err(error) = cycle.write_bgra8(bgra8, width, height) {
            log::error!("qaros: video sender {stream_id}: {error}");
        }
        cycle.show()?;
        frame_sink::perf::record(
            "panel_copy_and_show",
            Some(&stream_id),
            started.elapsed().as_micros(),
        );
        Ok(true)
    }
}

impl ClosedElsewhere for PanelStateWatch {
    fn closed_after_open(&self) -> bool {
        PanelStateWatch::closed_after_open(self)
    }
}

impl StreamBackend for QarosStreamBackend {
    type Sender = VideoSender;
    type Subscription = PanelGestureSubscription;
    type StateWatch = PanelStateWatch;

    fn create_sender(&self, width: u32, height: u32) -> Result<VideoSender, String> {
        VideoSender::create_cpu(&self.connection, width, height)
    }

    fn get_or_create_stream(
        &self,
        config: &GuiPanelConfig,
        stream: StreamId,
    ) -> Result<GuiPanelId, String> {
        stream_panel_calls::get_or_create_stream(&self.connection, config, stream)
    }

    fn set_stream(&self, panel: GuiPanelId, stream: StreamId) -> Result<(), String> {
        stream_panel_calls::set_stream(&self.connection, panel, stream)
    }

    fn set_size(&self, panel: GuiPanelId, size: GuiPanelSize) -> Result<(), String> {
        self.connection
            .session()
            .gui_panels()
            .set_size(panel, size)
            .map_err(|error| error.to_string())
    }

    fn set_pose(&self, panel: GuiPanelId, pose: Pose) -> Result<(), String> {
        self.connection
            .session()
            .gui_panels()
            .set_pose(panel, pose)
            .map_err(|error| error.to_string())
    }

    fn set_state(&self, panel: GuiPanelId, state: GuiPanelState) -> Result<(), String> {
        self.connection
            .session()
            .gui_panels()
            .set_state(panel, state)
            .map_err(|error| error.to_string())
    }

    fn close_panel(&self, panel: GuiPanelId) -> Result<(), String> {
        self.connection
            .session()
            .gui_panels()
            .close(panel)
            .map_err(|error| error.to_string())
    }

    fn subscribe_gestures(
        &self,
        panel: GuiPanelId,
        inbox: PanelGestures,
    ) -> Result<PanelGestureSubscription, String> {
        PanelGestureSubscription::subscribe(
            &self.connection,
            panel,
            inbox,
            self.connection.wake_viewer(),
        )
    }

    fn watch_state(&self, panel: GuiPanelId) -> Result<PanelStateWatch, String> {
        PanelStateWatch::subscribe(&self.connection, panel)
    }
}

/// A sender whose panel shows a newer one, kept until that one is proven.
struct RetiringSender<Sender> {
    sender: Sender,
    since: Instant,
}

/// An image size that differs from the sender's, and the publish it was first seen in.
#[derive(Clone, Copy)]
struct SizeCandidate {
    width: u32,
    height: u32,
    first_published_at: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SenderHealth {
    Healthy,
    /// Shows nothing until [`StreamPanels::maintain`] replaces it.
    Failed,
    /// Failed after every replacement allowed; the panel keeps its last frame.
    GivenUp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplacementCause {
    SizeChanged,
    SenderFailed,
}

/// One claimed panel and its sender.
pub(crate) struct PanelStream<Sender, Subscription, StateWatch> {
    image_key: String,
    panel: GuiPanelId,
    sender: Sender,
    /// Whether `sender` has had a ready begin, which proves the mixer connected a receiver.
    sender_proven: bool,
    retiring: Vec<RetiringSender<Sender>>,
    /// Where the panel's gestures go; only the timeline panel has one.
    gesture_inbox: Option<PanelGestures>,
    subscription: Option<Subscription>,
    subscribe_failure_logged: bool,
    shown_sequence: Option<u64>,
    last_show: Option<Instant>,
    /// When `sender` was put in place, while it has not shown its first frame.
    first_frame_due_since: Option<Instant>,
    health: SenderHealth,
    failure_replacements: u32,
    last_replacement: Option<Instant>,
    size_candidate: Option<SizeCandidate>,
    absent_listings: u32,
    /// Hidden by [`StreamPanels::follow_published_views`] because its view left the layout;
    /// shown again when the view is back.
    hidden_with_its_view: bool,
    /// What the claim's first frame sends once more; `None` once sent.
    first_frame_reassert: Option<FirstFrameReassert>,
    /// Tells a close in the Hub Display; `None` when the subscription failed.
    state_watch: Option<StateWatch>,
}

/// What the claim set on its panel and sends once more with the claim's first frame, when the
/// mixer has taken the claim's stream. The show the claim sends right after pointing the panel
/// at its stream is refused once as stale by QAROS (see [`StreamPanels::claim`]). The size and
/// the pose are fast updates, which the Hub applies without a stale check, but each is sent once
/// and not resent, and one that reaches the Hub before a new panel's create is dropped there.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FirstFrameReassert {
    size: GuiPanelSize,
    /// `Some` when the claim moved the panel.
    pose: Option<Pose>,
    show: bool,
}

/// Where a claim leaves its panel.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PanelPlace {
    /// A panel that exists stays where the Hub holds it; a new one opens at the config's pose.
    AsHeld,
    /// The panel is moved to the config's pose, for the reason given: its held place is no
    /// use to this run's user (ADR-0090).
    MovedBecause(String),
}

type HeldStream<Backend> = PanelStream<
    <Backend as StreamBackend>::Sender,
    <Backend as StreamBackend>::Subscription,
    <Backend as StreamBackend>::StateWatch,
>;

/// A panel closed by someone else while this run held it, released and waiting to be reopened.
struct ClosedWhileHeld {
    image_key: String,
    panel: GuiPanelId,
    noticed_at: Instant,
}

pub(crate) struct StreamPanels<Backend: StreamBackend> {
    backend: Backend,
    streams: Vec<HeldStream<Backend>>,
    /// Where the next round-robin show search starts.
    next_stream: usize,
    closed_while_held: Vec<ClosedWhileHeld>,
    /// Reopens so far this run, by image key.
    reopens: BTreeMap<String, u32>,
    /// When a target's head last streamed.
    last_target_head: Option<Instant>,
}

impl<Backend: StreamBackend> StreamPanels<Backend> {
    pub(crate) fn new(backend: Backend) -> Self {
        Self {
            backend,
            streams: Vec::new(),
            next_stream: 0,
            closed_while_held: Vec::new(),
            reopens: BTreeMap::new(),
            last_target_head: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn backend(&self) -> &Backend {
        &self.backend
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// Claims the stream panel named by `config` for the image under `image_key`, at
    /// `width` by `height` pixels:
    ///
    /// 1. A website panel listed under the name (only possible if something else created it
    ///    under ours) refuses the claim before any call.
    /// 2. The sender is created, then the panel with the sender's stream.
    /// 3. A listed panel, which `get_or_create` returns unchanged, still names an earlier
    ///    run's sender, so it gets `set_stream`. One listed `Closed` is built afresh from the
    ///    config instead (ADR-0075) and gets `set_stream` too.
    /// 4. The size follows this run's image; with [`PanelPlace::MovedBecause`] the panel is
    ///    moved to the config's pose, and a panel listed `Closed` that may stay where it was
    ///    held is put back there. A panel the earlier run hid is shown again once its stream is
    ///    in place, and one listed `Closed` is shown like a new one (ADR-0091).
    /// 5. With `gestures`, the panel's gestures are subscribed into it, and the panel's state
    ///    is watched for a close by someone else.
    /// 6. The claim's first frame sends the size, the pose it moved the panel to and the show
    ///    once more: the QAROS authority commits the stream with a snapshot that stamps every
    ///    field, so the show sent right after it is refused as stale ("field 'State' no longer
    ///    matches") and lives only on the library's resend (see `docs/known-issues.md`).
    pub(crate) fn claim(
        &mut self,
        image_key: &str,
        config: &GuiPanelConfig,
        listed: &[ListedPanel],
        (width, height): (u32, u32),
        gestures: Option<PanelGestures>,
        place: &PanelPlace,
    ) -> Result<GuiPanelId, String> {
        let listed_name = config
            .display_name
            .as_deref()
            .unwrap_or(&config.common_name);
        let earlier = listed
            .iter()
            .find(|panel| panel.display_name == listed_name);
        if let Some(ListedPanel {
            id,
            content: PanelContent::Uri(uri),
            ..
        }) = earlier
        {
            return Err(format!(
                "website panel {id} showing {uri:?} holds the name {listed_name}; leaving it \
                 and streaming nothing for {image_key}"
            ));
        }
        let sender = self.backend.create_sender(width, height)?;
        let stream = sender.stream_id();
        let panel = self.backend.get_or_create_stream(config, stream)?;
        let earlier =
            earlier.or_else(|| listed.iter().find(|listed_panel| listed_panel.id == panel));
        // `get_or_create` returns an existing panel unchanged, still naming an earlier run's
        // sender; the listing is not trusted to show every panel, so the stream is always set.
        self.backend.set_stream(panel, stream)?;
        self.backend.set_size(panel, config.size)?;
        let closed_earlier = earlier.filter(|earlier| earlier.state == GuiPanelState::Closed);
        let moved_to = match (place, closed_earlier) {
            (PanelPlace::AsHeld, Some(closed)) => Some(self.restore_held_place(panel, closed.pose)),
            _ => self.move_if_asked(panel, config, place),
        };
        let show_with_first_frame = self.show_after_claim(panel, stream, earlier, place, listed)?;
        log::info!(
            "qaros: {image_key} streams to panel {panel} {listed_name} at {width}x{height} px on \
             {:.3}x{:.3} m, stream id {stream}",
            config.size.width,
            config.size.height
        );
        let state_watch = match self.backend.watch_state(panel) {
            Ok(watch) => Some(watch),
            Err(error) => {
                log::warn!(
                    "qaros: a close of panel {panel} ({image_key}) in the Hub Display is not \
                     seen before the next listing: {error}"
                );
                None
            }
        };
        let mut claimed = PanelStream {
            image_key: image_key.to_owned(),
            panel,
            sender,
            sender_proven: false,
            retiring: Vec::new(),
            gesture_inbox: gestures,
            subscription: None,
            subscribe_failure_logged: false,
            shown_sequence: None,
            last_show: None,
            first_frame_due_since: Some(Instant::now()),
            health: SenderHealth::Healthy,
            failure_replacements: 0,
            last_replacement: None,
            size_candidate: None,
            absent_listings: 0,
            hidden_with_its_view: false,
            first_frame_reassert: Some(FirstFrameReassert {
                size: config.size,
                pose: moved_to,
                show: show_with_first_frame,
            }),
            state_watch,
        };
        subscribe_if_wanted(&self.backend, &mut claimed);
        self.streams.push(claimed);
        Ok(panel)
    }

    /// Shows the claimed `panel` once its `stream` is set, unless it was listed and is to stay
    /// as it is, and logs what the claim found. Returns whether the first frame shows it again.
    fn show_after_claim(
        &self,
        panel: GuiPanelId,
        stream: StreamId,
        earlier: Option<&ListedPanel>,
        place: &PanelPlace,
        listed: &[ListedPanel],
    ) -> Result<bool, String> {
        let Some(earlier) = earlier else {
            // New, or existing but missing from the listing (an earlier run's hidden panel
            // would otherwise stay hidden): shown either way, after its stream is set.
            self.backend.set_state(panel, GuiPanelState::Visible)?;
            let listed_names: Vec<&str> = listed
                .iter()
                .map(|listed_panel| listed_panel.display_name.as_str())
                .collect();
            log::info!(
                "qaros: stream panel {panel} not in this run's listing (new, or not listed: \
                 {} listed {listed_names:?}); stream {stream} set and shown",
                listed.len()
            );
            return Ok(true);
        };
        if earlier.state == GuiPanelState::Closed {
            // `get_or_create` built the closed panel afresh from the config, so it is a new
            // panel to show, at the place the claim chose (ADR-0091).
            self.backend.set_state(panel, GuiPanelState::Visible)?;
            log::info!(
                "qaros: stream panel {panel} was listed Closed (as {:?}, held at {} with its top \
                 edge); created afresh with stream {stream} and shown, {}",
                earlier.display_name,
                describe_position(earlier.pose),
                match place {
                    PanelPlace::AsHeld => "back at its held place".to_owned(),
                    PanelPlace::MovedBecause(reason) => format!("where new panels open: {reason}"),
                }
            );
            return Ok(true);
        }
        log::info!(
            "qaros: stream panel {panel} reattached (listed as {:?}, {:?}, showing {:?}, held at {} \
             with its top edge, {:.3}x{:.3} m); now showing stream {stream}",
            earlier.display_name,
            earlier.state,
            earlier.content,
            describe_position(earlier.pose),
            earlier.size.width,
            earlier.size.height
        );
        // Shown again only once its stream is in place (ADR-0054); a panel a user minimised
        // stays minimised.
        if earlier.state != GuiPanelState::Hidden {
            return Ok(false);
        }
        self.backend.set_state(panel, GuiPanelState::Visible)?;
        log::info!("qaros: stream panel {panel} shown again after the earlier run hid it");
        Ok(true)
    }

    /// Moves the panel to the config's pose when `place` asks for it, and returns that pose.
    /// A failed move is logged; the first frame tries once more.
    fn move_if_asked(
        &self,
        panel: GuiPanelId,
        config: &GuiPanelConfig,
        place: &PanelPlace,
    ) -> Option<Pose> {
        let PanelPlace::MovedBecause(reason) = place else {
            return None;
        };
        match self.backend.set_pose(panel, config.pose) {
            Ok(()) => log::info!(
                "qaros: stream panel {panel} moved to {} with its top edge: {reason}",
                describe_position(config.pose)
            ),
            Err(error) => log::warn!(
                "qaros: moving stream panel {panel} ({reason}): {error}; tried again with its \
                 first frame"
            ),
        }
        Some(config.pose)
    }

    /// Puts a panel that `get_or_create` built afresh at the config's pose back where it was
    /// held, and returns that pose. A failed move is logged; the first frame tries once more.
    fn restore_held_place(&self, panel: GuiPanelId, held: Pose) -> Pose {
        if let Err(error) = self.backend.set_pose(panel, held) {
            log::warn!(
                "qaros: putting stream panel {panel} back at its held place {}: {error}; tried \
                 again with its first frame",
                describe_position(held)
            );
        }
        held
    }

    /// `qar_gui_panels_close_panel` on a panel an earlier run left, so that the next
    /// `get_or_create` builds it afresh (`--reset-panels`).
    pub(crate) fn close_for_reset(&self, panel: GuiPanelId) -> Result<(), String> {
        self.backend.close_panel(panel)
    }

    /// Retires replaced senders past [`SENDER_RETIRE_LIMIT`], then shows at most one due
    /// image, round robin over the panels, so the work per call stays bounded on the render
    /// thread. An image is due when its sequence was not shown yet, or [`STREAM_REFRESH`]
    /// after its last show. An image whose size differs from its sender's begins no cycle: a
    /// begun cycle cannot be abandoned; [`StreamPanels::maintain`] replaces the sender.
    pub(crate) fn pump(&mut self, images: &PanelImages, now: Instant) {
        for stream in &mut self.streams {
            retire_overdue_senders(stream, now);
        }
        let count = self.streams.len();
        for offset in 0..count {
            let index = (self.next_stream + offset) % count;
            let Some(image) = self.due_image(index, images, now) else {
                continue;
            };
            self.next_stream = index + 1;
            self.show(index, &image, now);
            return;
        }
    }

    fn due_image(&self, index: usize, images: &PanelImages, now: Instant) -> Option<PanelImage> {
        let stream = &self.streams[index];
        if stream.health != SenderHealth::Healthy {
            return None;
        }
        let image = images.latest(&stream.image_key)?;
        let is_due = stream.shown_sequence != Some(image.sequence)
            || stream
                .last_show
                .is_none_or(|last| now.saturating_duration_since(last) >= STREAM_REFRESH);
        let fits_the_sender = (image.width, image.height) == stream.sender.size();
        (is_due && fits_the_sender).then_some(image)
    }

    fn show(&mut self, index: usize, image: &PanelImage, now: Instant) {
        let stream = &mut self.streams[index];
        match stream
            .sender
            .show_bgra8(&image.bgra8, image.width, image.height)
        {
            Ok(true) => {
                if stream.shown_sequence.is_none() {
                    log::info!(
                        "qaros: first frame of {} shown on stream {}",
                        stream.image_key,
                        stream.sender.stream_id()
                    );
                }
                stream.shown_sequence = Some(image.sequence);
                stream.last_show = Some(now);
                stream.first_frame_due_since = None;
                if !stream.sender_proven {
                    stream.sender_proven = true;
                    retire_all_senders(stream, "its successor had its first ready begin");
                }
                reassert_with_the_first_frame(&self.backend, stream);
            }
            Ok(false) => {}
            Err(error) => {
                stream.health = SenderHealth::Failed;
                log::warn!(
                    "qaros: the stream {} of {} failed: {error}",
                    stream.sender.stream_id(),
                    stream.image_key
                );
            }
        }
    }

    /// Whether a sender put in place within [`FIRST_FRAME_WAIT`] before `now` has not shown
    /// its first frame yet. Until it has, no claim may block this thread (ADR-0096).
    pub(crate) fn awaits_a_first_frame(&self, now: Instant) -> bool {
        self.first_frame_awaited_besides(None, now)
    }

    fn first_frame_awaited_besides(&self, except: Option<usize>, now: Instant) -> bool {
        self.streams.iter().enumerate().any(|(index, stream)| {
            Some(index) != except
                && stream.health == SenderHealth::Healthy
                && stream
                    .first_frame_due_since
                    .is_some_and(|since| now.saturating_duration_since(since) < FIRST_FRAME_WAIT)
        })
    }

    /// Once a sync: releases the panels closed by someone else, subscribes gestures that failed
    /// to subscribe, and replaces a sender that failed (at most [`FAILURE_REPLACEMENT_LIMIT`]
    /// times a run) or whose image has had a new size for two publishes in a row.
    /// Replacements of one panel are at least [`REPLACEMENT_SPACING`] apart, and wait while
    /// another panel's new sender has not shown its first frame (ADR-0096). `panel_size` gives
    /// the panel's size in metres for an image size.
    pub(crate) fn maintain(
        &mut self,
        images: &PanelImages,
        now: Instant,
        panel_size: impl Fn(u32, u32) -> GuiPanelSize,
    ) {
        self.release_panels_closed_elsewhere(now);
        for index in 0..self.streams.len() {
            subscribe_if_wanted(&self.backend, &mut self.streams[index]);
            let image = images.latest(&self.streams[index].image_key);
            let Some((cause, width, height)) = self.replacement_due(index, image.as_ref(), now)
            else {
                continue;
            };
            if self.first_frame_awaited_besides(Some(index), now) {
                continue;
            }
            self.replace_sender(index, cause, (width, height), &panel_size, now);
        }
    }

    fn replacement_due(
        &mut self,
        index: usize,
        image: Option<&PanelImage>,
        now: Instant,
    ) -> Option<(ReplacementCause, u32, u32)> {
        let stream = &mut self.streams[index];
        let spaced = stream
            .last_replacement
            .is_none_or(|last| now.saturating_duration_since(last) >= REPLACEMENT_SPACING);
        let (width, height) =
            image.map_or(stream.sender.size(), |image| (image.width, image.height));
        match stream.health {
            SenderHealth::GivenUp => return None,
            SenderHealth::Failed if stream.failure_replacements >= FAILURE_REPLACEMENT_LIMIT => {
                stream.health = SenderHealth::GivenUp;
                log::warn!(
                    "qaros: the stream of {} failed {} times after {FAILURE_REPLACEMENT_LIMIT} \
                     replacements; its panel keeps its last frame for the rest of this run",
                    stream.image_key,
                    FAILURE_REPLACEMENT_LIMIT + 1
                );
                return None;
            }
            SenderHealth::Failed => {
                return spaced.then_some((ReplacementCause::SenderFailed, width, height));
            }
            SenderHealth::Healthy => {}
        }
        let image = image?;
        if (image.width, image.height) == stream.sender.size() {
            stream.size_candidate = None;
            return None;
        }
        match stream.size_candidate {
            Some(candidate)
                if (candidate.width, candidate.height) == (image.width, image.height) =>
            {
                let published_again = image.published_at > candidate.first_published_at;
                (published_again && spaced).then_some((
                    ReplacementCause::SizeChanged,
                    width,
                    height,
                ))
            }
            _ => {
                stream.size_candidate = Some(SizeCandidate {
                    width: image.width,
                    height: image.height,
                    first_published_at: image.published_at,
                });
                None
            }
        }
    }

    /// Creates a sender at the new size, points the panel at it and resizes the panel. The
    /// old sender stays alive until the new one's first ready begin, or for
    /// [`SENDER_RETIRE_LIMIT`]; a fixed delay could let it die first and close the panel.
    fn replace_sender(
        &mut self,
        index: usize,
        cause: ReplacementCause,
        (width, height): (u32, u32),
        panel_size: &impl Fn(u32, u32) -> GuiPanelSize,
        now: Instant,
    ) {
        let stream = &mut self.streams[index];
        stream.last_replacement = Some(now);
        if cause == ReplacementCause::SenderFailed {
            stream.failure_replacements += 1;
        }
        let reason = match cause {
            ReplacementCause::SizeChanged => "the image's size changed",
            ReplacementCause::SenderFailed => "the sender failed",
        };
        let replacement = match self.backend.create_sender(width, height) {
            Ok(replacement) => replacement,
            Err(error) => {
                log::warn!(
                    "qaros: no new sender for {} ({reason}): {error}",
                    stream.image_key
                );
                return;
            }
        };
        let new_stream = replacement.stream_id();
        if let Err(error) = self.backend.set_stream(stream.panel, new_stream) {
            log::warn!(
                "qaros: panel {} of {} keeps its stream ({reason}); set_stream: {error}",
                stream.panel,
                stream.image_key
            );
            return;
        }
        let new_size = panel_size(width, height);
        if let Err(error) = self.backend.set_size(stream.panel, new_size) {
            log::warn!(
                "qaros: resizing panel {} of {}: {error}",
                stream.panel,
                stream.image_key
            );
        }
        if let Some(reassert) = &mut stream.first_frame_reassert {
            reassert.size = new_size;
        }
        let old_sender = std::mem::replace(&mut stream.sender, replacement);
        log::info!(
            "qaros: {} now streams {new_stream} at {width}x{height} px instead of {} ({reason}); \
             the old stream lives until the new one's first ready begin, at most \
             {SENDER_RETIRE_LIMIT:?}",
            stream.image_key,
            old_sender.stream_id()
        );
        stream.retiring.push(RetiringSender {
            sender: old_sender,
            since: now,
        });
        stream.sender_proven = false;
        stream.shown_sequence = None;
        stream.last_show = None;
        stream.first_frame_due_since = Some(now);
        stream.health = SenderHealth::Healthy;
        stream.size_candidate = None;
    }

    /// Keeps the held view panels in step with the viewer's layout, once a sync. A view panel
    /// whose view is not among `published` (the active recording changed, or its blueprint
    /// dropped or hid the view) is hidden, never closed, and keeps its sender. It is shown
    /// again once its view is among `settled` and the view's current image has been streamed to
    /// it, so it does not come back with the image it showed before it left. The timeline
    /// panel is no view and follows the active recording by itself.
    pub(crate) fn follow_published_views(&mut self, published: &[String], settled: &[String]) {
        for stream in &mut self.streams {
            if stream.image_key == TIMELINE_IMAGE_KEY {
                continue;
            }
            let is_published = published.contains(&stream.image_key);
            if !stream.hidden_with_its_view && !is_published {
                hide_with_its_view(&self.backend, stream);
            } else if stream.hidden_with_its_view
                && settled.contains(&stream.image_key)
                && stream.shown_sequence.is_some()
            {
                show_with_its_view(&self.backend, stream);
            }
        }
    }

    /// Checks the held panels against a listing at `now`. A panel listed `Closed`, or missing
    /// from [`ABSENT_LISTINGS_BEFORE_GONE`] listings in a row, is released like one its state
    /// watch saw closed (see [`StreamPanels::panels_to_reopen`]). Returns the image keys
    /// released.
    pub(crate) fn note_listing(&mut self, listed: &[ListedPanel], now: Instant) -> Vec<String> {
        let mut released = Vec::new();
        let mut index = 0;
        while index < self.streams.len() {
            let stream = &mut self.streams[index];
            let listed_state = listed
                .iter()
                .find(|listed_panel| listed_panel.id == stream.panel)
                .map(|listed_panel| listed_panel.state);
            let gone = match listed_state {
                Some(GuiPanelState::Closed) => Some("is listed Closed"),
                Some(_) => {
                    stream.absent_listings = 0;
                    None
                }
                None => {
                    stream.absent_listings += 1;
                    (stream.absent_listings >= ABSENT_LISTINGS_BEFORE_GONE)
                        .then_some("is missing from two panel listings in a row")
                }
            };
            let Some(how) = gone else {
                index += 1;
                continue;
            };
            released.push(self.release_closed(index, how, now));
        }
        released
    }

    /// Releases every held panel whose state watch saw it closed after it was open.
    fn release_panels_closed_elsewhere(&mut self, now: Instant) {
        let mut index = 0;
        while index < self.streams.len() {
            if is_closed_elsewhere(&self.streams[index]) {
                self.release_closed(index, "was closed", now);
            } else {
                index += 1;
            }
        }
    }

    /// Releases the held panel at `index`, closed by someone else: its subscriptions cancelled
    /// and its senders destroyed, so Stop does not hide it. It waits to be reopened. Returns its
    /// image key.
    fn release_closed(&mut self, index: usize, how: &str, now: Instant) -> String {
        let mut stream = self.streams.remove(index);
        log::warn!(
            "qaros: stream panel {} of {} {how} while this run held it. QAROS reports no proposer \
             for a panel's state, so a close in the Hub Display cannot be told from a mixer's \
             (Quaternar/Quaternar#344: one mixer's failure closes a panel for every viewer, \
             headsets included); the panel is reopened once a target's head streams after this \
             close (ADR-0091)",
            stream.panel,
            stream.image_key
        );
        drop(stream.subscription.take());
        drop(stream.state_watch.take());
        if self.next_stream >= self.streams.len() {
            self.next_stream = 0;
        }
        self.closed_while_held.push(ClosedWhileHeld {
            image_key: stream.image_key.clone(),
            panel: stream.panel,
            noticed_at: now,
        });
        stream.image_key
    }

    /// A target's head streamed at `now`.
    pub(crate) fn note_target_head(&mut self, now: Instant) {
        self.last_target_head = Some(now);
    }

    /// The panels closed while held that are due to be reopened: those closed before a target's
    /// head last streamed, so a headset that came back, or one that streamed on through the
    /// close, sees them again. Each is reopened at most [`REOPEN_LIMIT`] times a run; past that
    /// it stays closed until the next run. The caller claims each returned panel again.
    pub(crate) fn panels_to_reopen(&mut self) -> Vec<GuiPanelId> {
        let Some(last_target_head) = self.last_target_head else {
            return Vec::new();
        };
        let (due, waiting): (Vec<ClosedWhileHeld>, Vec<ClosedWhileHeld>) = self
            .closed_while_held
            .drain(..)
            .partition(|closed| closed.noticed_at < last_target_head);
        self.closed_while_held = waiting;
        let mut reopened = Vec::new();
        for closed in due {
            let reopens = self.reopens.entry(closed.image_key.clone()).or_insert(0);
            if *reopens >= REOPEN_LIMIT {
                log::warn!(
                    "qaros: stream panel {} of {} was closed again after {REOPEN_LIMIT} reopens \
                     this run; it stays closed until the next run reopens it",
                    closed.panel,
                    closed.image_key
                );
                continue;
            }
            *reopens += 1;
            log::info!(
                "qaros: reopening stream panel {} of {} (reopen {} of at most {REOPEN_LIMIT} this \
                 run): a target's head streamed after its close",
                closed.panel,
                closed.image_key,
                *reopens
            );
            reopened.push(closed.panel);
        }
        reopened
    }

    /// The first half of Stop: every held panel hidden for the next run. A panel already hidden
    /// with its view gets no second call, and one closed by someone else since the last sync
    /// gets none at all.
    pub(crate) fn hide_all(&self) {
        for stream in self
            .streams
            .iter()
            .filter(|stream| !stream.hidden_with_its_view && !is_closed_elsewhere(stream))
        {
            match self.backend.set_state(stream.panel, GuiPanelState::Hidden) {
                Ok(()) => log::info!(
                    "qaros: stream panel {} for {} hidden until the next run",
                    stream.panel,
                    stream.image_key
                ),
                Err(error) => log::warn!(
                    "qaros: hiding the stream panel for {}: {error}",
                    stream.image_key
                ),
            }
        }
    }

    /// The second half of Stop, before the session goes: every gesture and state subscription
    /// cancelled (the cancel waits for a running callback), then every sender destroyed,
    /// retiring ones included.
    pub(crate) fn close(&mut self) {
        let started = Instant::now();
        for stream in &mut self.streams {
            drop(stream.subscription.take());
            drop(stream.state_watch.take());
        }
        let cancelled = started.elapsed();
        self.streams.clear();
        log::info!(
            "qaros: panel streams closed: gestures cancelled in {} us, senders destroyed in {} us",
            cancelled.as_micros(),
            started.elapsed().saturating_sub(cancelled).as_micros()
        );
    }
}

/// Hides the panel of a view that left the layout. Its sender stays, and its shown image is
/// forgotten: the view's image starts a new sequence when it comes back, and that image has to
/// be streamed before the panel is shown again. A failed call is retried at the next sync.
fn hide_with_its_view<Backend: StreamBackend>(backend: &Backend, stream: &mut HeldStream<Backend>) {
    match backend.set_state(stream.panel, GuiPanelState::Hidden) {
        Ok(()) => {
            stream.hidden_with_its_view = true;
            stream.shown_sequence = None;
            stream.last_show = None;
            log::info!(
                "qaros: {} left the viewer's layout; its panel {} is hidden until it is back",
                stream.image_key,
                stream.panel
            );
        }
        Err(error) => log::warn!(
            "qaros: hiding the panel of {}, which left the viewer's layout: {error}",
            stream.image_key
        ),
    }
}

/// Shows the panel of a view that is back in the layout. A failed call is retried at the
/// next sync.
fn show_with_its_view<Backend: StreamBackend>(backend: &Backend, stream: &mut HeldStream<Backend>) {
    match backend.set_state(stream.panel, GuiPanelState::Visible) {
        Ok(()) => {
            stream.hidden_with_its_view = false;
            log::info!(
                "qaros: {} is back in the viewer's layout; its panel {} is shown again",
                stream.image_key,
                stream.panel
            );
        }
        Err(error) => log::warn!(
            "qaros: showing the panel of {} again: {error}",
            stream.image_key
        ),
    }
}

fn is_closed_elsewhere<Sender, Subscription, StateWatch: ClosedElsewhere>(
    stream: &PanelStream<Sender, Subscription, StateWatch>,
) -> bool {
    stream
        .state_watch
        .as_ref()
        .is_some_and(ClosedElsewhere::closed_after_open)
}

/// Sends what the claim set on the panel once more with the claim's first frame: the size,
/// the pose when the claim moved the panel, and the show when the claim showed it, unless its
/// view has left the layout since, which leaves the showing to
/// [`StreamPanels::follow_published_views`]. By the first frame the mixer has taken the claim's
/// stream, so these follow the stream's commit instead of racing it. A failure is logged and
/// not retried: the claim's own calls stand.
fn reassert_with_the_first_frame<Backend: StreamBackend>(
    backend: &Backend,
    stream: &mut HeldStream<Backend>,
) {
    let Some(reassert) = stream.first_frame_reassert.take() else {
        return;
    };
    if let Err(error) = backend.set_size(stream.panel, reassert.size) {
        log::warn!(
            "qaros: sizing the panel of {} again with its first frame: {error}",
            stream.image_key
        );
    }
    if let Some(pose) = reassert.pose
        && let Err(error) = backend.set_pose(stream.panel, pose)
    {
        log::warn!(
            "qaros: moving the panel of {} again with its first frame: {error}",
            stream.image_key
        );
    }
    let show = reassert.show && !stream.hidden_with_its_view;
    if show && let Err(error) = backend.set_state(stream.panel, GuiPanelState::Visible) {
        log::warn!(
            "qaros: showing the panel of {} again with its first frame: {error}",
            stream.image_key
        );
    }
    log::info!(
        "qaros: stream panel {} of {} sized {:.3}x{:.3} m{}{} again with its first frame (the \
         claim's show is refused once as stale by QAROS 0.9.0-dev.3184)",
        stream.panel,
        stream.image_key,
        reassert.size.width,
        reassert.size.height,
        if reassert.pose.is_some() {
            ", moved"
        } else {
            ""
        },
        if show { ", shown" } else { "" }
    );
}

/// For logs: a panel pose's position, `[x, y, z]` in metres.
fn describe_position(pose: Pose) -> String {
    format!(
        "[{:.3}, {:.3}, {:.3}]",
        pose.position.x, pose.position.y, pose.position.z
    )
}

/// Subscribes the panel's gestures when it wants them and has no subscription; a failure is
/// logged once and tried again at the next maintain.
fn subscribe_if_wanted<Backend: StreamBackend>(
    backend: &Backend,
    stream: &mut HeldStream<Backend>,
) {
    let Some(inbox) = &stream.gesture_inbox else {
        return;
    };
    if stream.subscription.is_some() {
        return;
    }
    match backend.subscribe_gestures(stream.panel, inbox.clone()) {
        Ok(subscription) => {
            log::info!(
                "qaros: gestures of panel {} ({}) subscribed",
                stream.panel,
                stream.image_key
            );
            stream.subscription = Some(subscription);
        }
        Err(error) => {
            if !stream.subscribe_failure_logged {
                stream.subscribe_failure_logged = true;
                log::warn!(
                    "qaros: no gestures from panel {} ({}) yet: {error}",
                    stream.panel,
                    stream.image_key
                );
            }
        }
    }
}

fn retire_overdue_senders<Sender: StreamSender, Subscription, StateWatch>(
    stream: &mut PanelStream<Sender, Subscription, StateWatch>,
    now: Instant,
) {
    let has_overdue = stream
        .retiring
        .iter()
        .any(|retiring| now.saturating_duration_since(retiring.since) >= SENDER_RETIRE_LIMIT);
    if has_overdue {
        retire_all_senders(
            stream,
            "its successor had no ready begin within the retire limit",
        );
    }
}

/// Every retiring sender is older than the current one, so once any is due, all are.
fn retire_all_senders<Sender: StreamSender, Subscription, StateWatch>(
    stream: &mut PanelStream<Sender, Subscription, StateWatch>,
    why: &str,
) {
    for retiring in stream.retiring.drain(..) {
        log::info!(
            "qaros: old stream {} of {} destroyed: {why}",
            retiring.sender.stream_id(),
            stream.image_key
        );
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use qaros::{Pose, Quat, Vec3};

    use super::*;

    type CallLog = Rc<RefCell<Vec<String>>>;

    struct FakeSender {
        number: u8,
        size: (u32, u32),
        log: CallLog,
        ready: Rc<RefCell<bool>>,
        failing: Rc<RefCell<bool>>,
    }

    impl StreamSender for FakeSender {
        fn stream_id(&self) -> StreamId {
            StreamId([self.number; 16])
        }

        fn size(&self) -> (u32, u32) {
            self.size
        }

        fn show_bgra8(&mut self, bgra8: &[u8], width: u32, height: u32) -> Result<bool, String> {
            if *self.failing.borrow() {
                self.log.borrow_mut().push(format!("fail {}", self.number));
                return Err("QAR_STATUS_MEDIA_STREAMING_STREAM_IS_CLOSED".to_owned());
            }
            if !*self.ready.borrow() {
                self.log
                    .borrow_mut()
                    .push(format!("not ready {}", self.number));
                return Ok(false);
            }
            assert_eq!(bgra8.len(), width as usize * height as usize * 4);
            self.log.borrow_mut().push(format!("show {}", self.number));
            Ok(true)
        }
    }

    impl Drop for FakeSender {
        fn drop(&mut self) {
            self.log
                .borrow_mut()
                .push(format!("destroy sender {}", self.number));
        }
    }

    struct FakeSubscription {
        panel: GuiPanelId,
        log: CallLog,
    }

    impl Drop for FakeSubscription {
        fn drop(&mut self) {
            self.log
                .borrow_mut()
                .push(format!("cancel gestures {}", self.panel.0[0]));
        }
    }

    /// The panels, by the first byte of their id, that someone else closed after the claim.
    type ClosedPanels = Rc<RefCell<Vec<u8>>>;

    struct FakeStateWatch {
        panel: GuiPanelId,
        closed_elsewhere: ClosedPanels,
    }

    impl ClosedElsewhere for FakeStateWatch {
        fn closed_after_open(&self) -> bool {
            self.closed_elsewhere.borrow().contains(&self.panel.0[0])
        }
    }

    #[derive(Default)]
    struct FakeBackend {
        log: CallLog,
        senders_created: RefCell<u8>,
        ready: Rc<RefCell<bool>>,
        failing: Rc<RefCell<bool>>,
        closed_elsewhere: ClosedPanels,
        state_watch_fails: bool,
    }

    impl FakeBackend {
        fn calls(&self) -> Vec<String> {
            self.log.borrow().clone()
        }

        fn ready() -> Self {
            let backend = Self::default();
            *backend.ready.borrow_mut() = true;
            backend
        }

        /// A close in the Hub Display or by a mixer, which the state watch reports alike.
        fn close_elsewhere(&self, panel: GuiPanelId) {
            self.closed_elsewhere.borrow_mut().push(panel.0[0]);
        }

        /// The claim's `get_or_create` built the panel afresh, so a new watch sees it open.
        fn forget_the_close(&self, panel: GuiPanelId) {
            self.closed_elsewhere
                .borrow_mut()
                .retain(|closed| *closed != panel.0[0]);
        }
    }

    impl StreamBackend for FakeBackend {
        type Sender = FakeSender;
        type Subscription = FakeSubscription;
        type StateWatch = FakeStateWatch;

        fn create_sender(&self, width: u32, height: u32) -> Result<FakeSender, String> {
            let mut created = self.senders_created.borrow_mut();
            *created += 1;
            self.log
                .borrow_mut()
                .push(format!("create sender {} {width}x{height}", *created));
            Ok(FakeSender {
                number: *created,
                size: (width, height),
                log: Rc::clone(&self.log),
                ready: Rc::clone(&self.ready),
                failing: Rc::clone(&self.failing),
            })
        }

        fn get_or_create_stream(
            &self,
            config: &GuiPanelConfig,
            stream: StreamId,
        ) -> Result<GuiPanelId, String> {
            self.log.borrow_mut().push(format!(
                "get_or_create {} stream {}",
                config.common_name, stream.0[0]
            ));
            Ok(panel_id_of(&config.common_name))
        }

        fn set_stream(&self, panel: GuiPanelId, stream: StreamId) -> Result<(), String> {
            self.log
                .borrow_mut()
                .push(format!("set_stream {} {}", panel.0[0], stream.0[0]));
            Ok(())
        }

        fn set_size(&self, panel: GuiPanelId, size: GuiPanelSize) -> Result<(), String> {
            self.log
                .borrow_mut()
                .push(format!("set_size {} {:.3}", panel.0[0], size.height));
            Ok(())
        }

        fn set_pose(&self, panel: GuiPanelId, pose: Pose) -> Result<(), String> {
            self.log.borrow_mut().push(format!(
                "set_pose {} [{:.2}, {:.2}, {:.2}]",
                panel.0[0], pose.position.x, pose.position.y, pose.position.z
            ));
            Ok(())
        }

        fn set_state(&self, panel: GuiPanelId, state: GuiPanelState) -> Result<(), String> {
            self.log
                .borrow_mut()
                .push(format!("set_state {} {state:?}", panel.0[0]));
            Ok(())
        }

        fn close_panel(&self, panel: GuiPanelId) -> Result<(), String> {
            self.log.borrow_mut().push(format!("close {}", panel.0[0]));
            Ok(())
        }

        fn subscribe_gestures(
            &self,
            panel: GuiPanelId,
            _inbox: PanelGestures,
        ) -> Result<FakeSubscription, String> {
            self.log
                .borrow_mut()
                .push(format!("subscribe gestures {}", panel.0[0]));
            Ok(FakeSubscription {
                panel,
                log: Rc::clone(&self.log),
            })
        }

        /// Not in the call log: every claim watches its panel.
        fn watch_state(&self, panel: GuiPanelId) -> Result<FakeStateWatch, String> {
            if self.state_watch_fails {
                return Err("QAR_STATUS_NOT_IMPLEMENTED".to_owned());
            }
            Ok(FakeStateWatch {
                panel,
                closed_elsewhere: Rc::clone(&self.closed_elsewhere),
            })
        }
    }

    /// The first byte tells the panels apart in the call log.
    fn panel_id_of(common_name: &str) -> GuiPanelId {
        GuiPanelId([common_name.len().try_into().unwrap(); 16])
    }

    fn config(common_name: &str) -> GuiPanelConfig {
        GuiPanelConfig {
            display_name: Some(common_name.to_owned()),
            size: GuiPanelSize {
                width: 0.45,
                height: 0.3,
                content_scale: 1.0,
            },
            ..GuiPanelConfig::new(common_name)
        }
    }

    fn listed(display_name: &str, state: GuiPanelState, content: PanelContent) -> ListedPanel {
        ListedPanel {
            id: panel_id_of(display_name),
            display_name: display_name.to_owned(),
            pose: Pose {
                orientation: Quat::IDENTITY,
                position: Vec3::new(0.0, 0.0, 0.0),
            },
            size: GuiPanelSize::default(),
            state,
            content,
        }
    }

    /// A panel's size in metres as `panels.rs` would give it: 0.5 m tall per height-to-width
    /// ratio, which is enough to tell sizes apart in the call log.
    fn panel_size_by_aspect(width: u32, height: u32) -> GuiPanelSize {
        GuiPanelSize {
            width: 0.45,
            height: 0.5 * f32::from(u16::try_from(height).unwrap())
                / f32::from(u16::try_from(width).unwrap()),
            content_scale: 1.0,
        }
    }

    fn calls_from(panels: &StreamPanels<FakeBackend>, start: usize) -> Vec<String> {
        panels.backend().calls()[start..].to_vec()
    }

    fn shows(panels: &StreamPanels<FakeBackend>) -> Vec<String> {
        panels
            .backend()
            .calls()
            .into_iter()
            .filter(|call| call.starts_with("show"))
            .collect()
    }

    const CAMERA: &str = "camera.stream.rerun.rerun";
    const PLOT: &str = "plot.stream.rerun.rerun";
    const TIMELINE: &str = "timeline.panel-stream.rerun.rerun";

    #[test]
    fn a_new_view_creates_its_sender_before_its_panel_and_subscribes_no_gestures() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        let panel = panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (640, 426),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        assert_eq!(panel, panel_id_of(CAMERA));
        assert_eq!(
            panels.backend().calls(),
            [
                "create sender 1 640x426",
                "get_or_create camera.stream.rerun.rerun stream 1",
                "set_stream 25 1",
                "set_size 25 0.300",
                "set_state 25 Visible",
            ]
        );
    }

    #[test]
    fn the_timeline_panel_subscribes_its_gestures_after_its_claim_and_cancels_them_at_stop() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        panels
            .claim(
                "timeline.panel",
                &config(TIMELINE),
                &[],
                (960, 256),
                Some(PanelGestures::default()),
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let calls = panels.backend().calls();
        assert_eq!(
            calls.last().map(String::as_str),
            Some("subscribe gestures 33")
        );
        assert_eq!(calls[4], "set_state 33 Visible");
        let claimed = calls.len();
        panels.hide_all();
        panels.close();
        assert_eq!(
            calls_from(&panels, claimed),
            [
                "set_state 33 Hidden",
                "cancel gestures 33",
                "destroy sender 1"
            ]
        );
    }

    #[test]
    fn a_hidden_panel_found_by_id_under_another_listed_name_gets_the_new_stream_then_is_shown() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        let mut earlier = listed(
            CAMERA,
            GuiPanelState::Hidden,
            PanelContent::Stream(StreamId([9; 16])),
        );
        earlier.display_name = String::new();
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[earlier],
                (640, 426),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        assert_eq!(
            panels.backend().calls(),
            [
                "create sender 1 640x426",
                "get_or_create camera.stream.rerun.rerun stream 1",
                "set_stream 25 1",
                "set_size 25 0.300",
                "set_state 25 Visible",
            ]
        );
    }

    #[test]
    fn a_listed_minimised_panel_gets_the_new_stream_and_stays_minimised() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        let minimised = listed(
            CAMERA,
            GuiPanelState::Minimized,
            PanelContent::Stream(StreamId([9; 16])),
        );
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[minimised],
                (640, 426),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let calls = panels.backend().calls();
        assert_eq!(calls[2], "set_stream 25 1");
        assert!(
            !calls.iter().any(|call| call.starts_with("set_state")),
            "{calls:?}"
        );
    }

    /// The camera panel as a listing shows it after a mixer closed it, held at `[x, y, z]`.
    fn closed_at(x: f32, y: f32, z: f32) -> ListedPanel {
        let mut closed = listed(
            CAMERA,
            GuiPanelState::Closed,
            PanelContent::Stream(StreamId([9; 16])),
        );
        closed.display_name = String::new();
        closed.pose.position = Vec3::new(x, y, z);
        closed
    }

    #[test]
    fn a_panel_listed_closed_is_created_afresh_back_at_its_held_place_and_shown_twice() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config_placed_at(CAMERA, 0.25, -0.3, -0.4),
                &[closed_at(0.7, -0.3, 0.3)],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let claim_calls = [
            "create sender 1 4x2",
            "get_or_create camera.stream.rerun.rerun stream 1",
            "set_stream 25 1",
            "set_size 25 0.300",
            "set_pose 25 [0.70, -0.30, 0.30]",
            "set_state 25 Visible",
        ];
        assert_eq!(panels.backend().calls(), claim_calls);

        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, Instant::now());
        assert_eq!(
            calls_from(&panels, claim_calls.len()),
            [
                "show 1",
                "set_size 25 0.300",
                "set_pose 25 [0.70, -0.30, 0.30]",
                "set_state 25 Visible"
            ],
            "`get_or_create` opened it at the config's pose; the held place and the show follow \
             the stream's commit"
        );
    }

    #[test]
    fn a_panel_listed_closed_whose_held_place_is_of_no_use_opens_where_new_panels_open() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        panels
            .claim(
                "camera",
                &config_placed_at(CAMERA, 0.25, -0.3, -0.4),
                &[closed_at(9.0, 0.0, 0.0)],
                (4, 2),
                None,
                &PanelPlace::MovedBecause("held 9.0 m from the eyes".to_owned()),
            )
            .unwrap();
        assert_eq!(
            panels.backend().calls()[3..],
            [
                "set_size 25 0.300",
                "set_pose 25 [0.25, -0.30, -0.40]",
                "set_state 25 Visible"
            ]
        );
    }

    fn hidden_by_an_earlier_run(display_name: &str) -> ListedPanel {
        let mut earlier = listed(
            display_name,
            GuiPanelState::Hidden,
            PanelContent::Stream(StreamId([9; 16])),
        );
        earlier.display_name = String::new();
        earlier
    }

    #[test]
    fn a_reattached_hidden_panel_is_shown_again_once_with_its_first_frame() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[hidden_by_an_earlier_run(CAMERA)],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let claimed = panels.backend().calls().len();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();

        let start = Instant::now();
        panels.pump(&images, start);
        assert_eq!(
            calls_from(&panels, claimed),
            ["not ready 1"],
            "no receiver yet: no frame shown, so no second show"
        );

        *panels.backend().ready.borrow_mut() = true;
        panels.pump(&images, start + STREAM_REFRESH);
        panels.pump(&images, start + STREAM_REFRESH * 2);
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 2)).unwrap();
        panels.pump(&images, start + STREAM_REFRESH * 2);
        assert_eq!(
            calls_from(&panels, claimed),
            [
                "not ready 1",
                "show 1",
                "set_size 25 0.300",
                "set_state 25 Visible",
                "show 1",
                "show 1"
            ]
        );
    }

    #[test]
    fn a_new_panel_is_shown_again_with_its_first_frame_and_the_timeline_too() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        panels
            .claim(
                TIMELINE_IMAGE_KEY,
                &config(TIMELINE),
                &[hidden_by_an_earlier_run(TIMELINE)],
                (4, 2),
                Some(PanelGestures::default()),
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let claimed = panels.backend().calls().len();
        let images = PanelImages::default();
        for key in ["camera", TIMELINE_IMAGE_KEY] {
            images.publish_rgba8(key, 4, 2, rgba(4, 2, 1)).unwrap();
        }
        panels.pump(&images, Instant::now());
        panels.pump(&images, Instant::now());
        assert_eq!(
            calls_from(&panels, claimed),
            [
                "show 1",
                "set_size 25 0.300",
                "set_state 25 Visible",
                "show 2",
                "set_size 33 0.300",
                "set_state 33 Visible"
            ]
        );
    }

    #[test]
    fn a_minimised_panel_is_not_shown_with_its_first_frame() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        let minimised = listed(
            CAMERA,
            GuiPanelState::Minimized,
            PanelContent::Stream(StreamId([9; 16])),
        );
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[minimised],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let claimed = panels.backend().calls().len();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, Instant::now());
        assert_eq!(
            calls_from(&panels, claimed),
            ["show 1", "set_size 25 0.300"],
            "the size again, and no show"
        );
    }

    #[test]
    fn a_panel_whose_view_left_before_its_first_frame_is_not_shown_with_it() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[hidden_by_an_earlier_run(CAMERA)],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        panels.follow_published_views(&[], &[]);
        let hidden = panels.backend().calls().len();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, Instant::now());
        assert_eq!(calls_from(&panels, hidden), ["show 1", "set_size 25 0.300"]);
    }

    #[test]
    fn a_replaced_sender_s_first_frame_shows_the_panel_no_second_time() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let images = PanelImages::default();
        let start = Instant::now();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, start);
        images.publish_rgba8("camera", 8, 2, rgba(8, 2, 1)).unwrap();
        panels.maintain(&images, start, panel_size_by_aspect);
        images.publish_rgba8("camera", 8, 2, rgba(8, 2, 2)).unwrap();
        let later = start + Duration::from_secs(5);
        panels.maintain(&images, later, panel_size_by_aspect);
        let replaced = panels.backend().calls().len();
        panels.pump(&images, later);
        let calls = panels.backend().calls();
        assert!(
            calls.iter().any(|call| call == "set_stream 25 2"),
            "the sender was replaced: {calls:?}"
        );
        assert_eq!(
            calls_from(&panels, replaced),
            ["show 2", "destroy sender 1"]
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.as_str() == "set_state 25 Visible")
                .count(),
            2,
            "the claim's show and the one with the claim's first frame: {calls:?}"
        );
    }

    #[test]
    fn a_website_panel_under_our_name_gets_no_call() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        let website = listed(
            CAMERA,
            GuiPanelState::Visible,
            PanelContent::Uri("http://192.168.50.40:9877/views/camera".to_owned()),
        );
        let refused = panels.claim(
            "camera",
            &config(CAMERA),
            &[website],
            (640, 426),
            None,
            &PanelPlace::AsHeld,
        );
        assert!(refused.unwrap_err().contains("website panel"));
        assert!(panels.backend().calls().is_empty());
    }

    #[test]
    fn two_views_and_a_restart_reclaim_both_panels_with_set_stream_and_no_duplicate() {
        let first_run_backend = FakeBackend::default();
        let log = Rc::clone(&first_run_backend.log);
        let mut first_run = StreamPanels::new(first_run_backend);
        let camera = first_run
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (64, 48),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let plot = first_run
            .claim(
                "plot",
                &config(PLOT),
                &[],
                (64, 48),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        first_run.hide_all();
        first_run.close();
        drop(first_run);
        log.borrow_mut().clear();

        let listed_after_stop = [
            listed(
                CAMERA,
                GuiPanelState::Hidden,
                PanelContent::Stream(StreamId([1; 16])),
            ),
            listed(
                PLOT,
                GuiPanelState::Hidden,
                PanelContent::Stream(StreamId([2; 16])),
            ),
        ];
        let mut second_run = StreamPanels::new(FakeBackend::default());
        let camera_again = second_run
            .claim(
                "camera",
                &config(CAMERA),
                &listed_after_stop,
                (64, 48),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let plot_again = second_run
            .claim(
                "plot",
                &config(PLOT),
                &listed_after_stop,
                (64, 48),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        assert_eq!((camera_again, plot_again), (camera, plot));
        assert_eq!(
            second_run.backend().calls(),
            [
                "create sender 1 64x48",
                "get_or_create camera.stream.rerun.rerun stream 1",
                "set_stream 25 1",
                "set_size 25 0.300",
                "set_state 25 Visible",
                "create sender 2 64x48",
                "get_or_create plot.stream.rerun.rerun stream 2",
                "set_stream 23 2",
                "set_size 23 0.300",
                "set_state 23 Visible",
            ]
        );
    }

    #[test]
    fn stop_hides_then_cancels_the_gestures_then_destroys_every_sender_retiring_ones_included() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        panels
            .claim(
                "timeline.panel",
                &config(TIMELINE),
                &[],
                (4, 2),
                Some(PanelGestures::default()),
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        let start = Instant::now();
        panels.maintain(&images, start, panel_size_by_aspect);
        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        // No mixer connects the timeline's sender, so it stops holding the replacement back.
        panels.maintain(&images, start + FIRST_FRAME_WAIT, panel_size_by_aspect);
        let claimed_and_replaced = panels.backend().calls().len();

        panels.hide_all();
        panels.close();
        assert_eq!(
            calls_from(&panels, claimed_and_replaced),
            [
                "set_state 25 Hidden",
                "set_state 33 Hidden",
                "cancel gestures 33",
                "destroy sender 3",
                "destroy sender 1",
                "destroy sender 2",
            ]
        );
    }

    fn rgba(width: u32, height: u32, red: u8) -> Vec<u8> {
        [red, 0, 0, 255].repeat(width as usize * height as usize)
    }

    #[test]
    fn shows_follow_new_images_one_per_call_round_robin_and_refresh_once_a_second() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        panels
            .claim(
                "plot",
                &config(PLOT),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        images.publish_rgba8("plot", 4, 2, rgba(4, 2, 1)).unwrap();
        let start = Instant::now();

        panels.pump(&images, start);
        panels.pump(&images, start);
        panels.pump(&images, start);
        assert_eq!(shows(&panels), ["show 1", "show 2"]);

        images.publish_rgba8("plot", 4, 2, rgba(4, 2, 2)).unwrap();
        panels.pump(&images, start + Duration::from_millis(100));
        assert_eq!(shows(&panels), ["show 1", "show 2", "show 2"]);

        for millis in [200, 500, 999] {
            panels.pump(&images, start + Duration::from_millis(millis));
        }
        assert_eq!(shows(&panels).len(), 3, "nothing due within the second");
        panels.pump(&images, start + STREAM_REFRESH);
        assert_eq!(shows(&panels).last().map(String::as_str), Some("show 1"));
        panels.pump(&images, start + STREAM_REFRESH);
        panels.pump(&images, start + STREAM_REFRESH + Duration::from_millis(99));
        assert_eq!(
            shows(&panels),
            ["show 1", "show 2", "show 2", "show 1"],
            "the plot was shown at 100 ms, so it is not due before 1.1 s"
        );
        panels.pump(&images, start + STREAM_REFRESH + Duration::from_millis(100));
        assert_eq!(shows(&panels).last().map(String::as_str), Some("show 2"));
    }

    #[test]
    fn a_size_mismatch_begins_no_cycle_and_a_not_ready_receiver_is_tried_again() {
        let backend = FakeBackend::default();
        let ready = Rc::clone(&backend.ready);
        let mut panels = StreamPanels::new(backend);
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        let now = Instant::now();
        panels.pump(&images, now);
        assert!(
            !panels
                .backend()
                .calls()
                .iter()
                .any(|call| call.starts_with("show") || call.starts_with("not ready")),
            "{:?}",
            panels.backend().calls()
        );

        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, now);
        *ready.borrow_mut() = true;
        panels.pump(&images, now);
        let calls = panels.backend().calls();
        assert_eq!(
            &calls[calls.len() - 4..],
            [
                "not ready 1",
                "show 1",
                "set_size 25 0.300",
                "set_state 25 Visible"
            ]
        );
    }

    #[test]
    fn a_size_stable_for_two_publishes_replaces_the_sender_and_the_old_one_goes_at_the_first_ready_begin()
     {
        let backend = FakeBackend::default();
        let ready = Rc::clone(&backend.ready);
        let mut panels = StreamPanels::new(backend);
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let claimed = panels.backend().calls().len();
        let images = PanelImages::default();
        let start = Instant::now();

        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        panels.maintain(&images, start, panel_size_by_aspect);
        assert_eq!(
            calls_from(&panels, claimed),
            Vec::<String>::new(),
            "one publish"
        );

        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        panels.maintain(
            &images,
            start + Duration::from_secs(1),
            panel_size_by_aspect,
        );
        assert_eq!(
            calls_from(&panels, claimed),
            [
                "create sender 2 2x2",
                "set_stream 25 2",
                "set_size 25 0.500"
            ]
        );

        let replaced = panels.backend().calls().len();
        panels.pump(&images, start + Duration::from_secs(2));
        assert_eq!(
            calls_from(&panels, replaced),
            ["not ready 2"],
            "the old sender lives while the new one has no receiver"
        );
        *ready.borrow_mut() = true;
        panels.pump(&images, start + Duration::from_secs(3));
        assert_eq!(
            calls_from(&panels, replaced),
            [
                "not ready 2",
                "show 2",
                "destroy sender 1",
                "set_size 25 0.500",
                "set_state 25 Visible"
            ],
            "the panel's first frame, on the replacing sender, sizes it at the replacement's \
             size and shows it again"
        );
    }

    #[test]
    fn a_view_scaled_into_its_fixed_frame_keeps_its_one_sender_through_a_resize() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (576, 304),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let claimed = panels.backend().calls().len();
        let images = PanelImages::default();
        let mut now = Instant::now();
        // A desktop resize changes what the frame shows at every capture, never its size.
        for shade in 1..=12 {
            images
                .publish_rgba8("camera", 576, 304, rgba(576, 304, shade))
                .unwrap();
            panels.pump(&images, now);
            panels.maintain(&images, now, panel_size_by_aspect);
            now += Duration::from_millis(250);
        }
        let calls = calls_from(&panels, claimed);
        let mut expected = vec!["show 1", "set_size 25 0.300", "set_state 25 Visible"];
        expected.extend(["show 1"].repeat(11));
        assert_eq!(
            calls, expected,
            "the first frame sizes and shows the panel once more; no new sender, set_stream or \
             resize"
        );
    }

    #[test]
    fn an_old_sender_whose_successor_never_gets_a_receiver_goes_after_ten_seconds() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let images = PanelImages::default();
        let start = Instant::now();
        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        panels.maintain(&images, start, panel_size_by_aspect);
        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        let replaced_at = start + Duration::from_secs(1);
        panels.maintain(&images, replaced_at, panel_size_by_aspect);

        panels.pump(&images, replaced_at + Duration::from_millis(9_999));
        assert!(
            !panels
                .backend()
                .calls()
                .contains(&"destroy sender 1".to_owned())
        );
        panels.pump(&images, replaced_at + SENDER_RETIRE_LIMIT);
        assert!(
            panels
                .backend()
                .calls()
                .contains(&"destroy sender 1".to_owned())
        );
    }

    #[test]
    fn a_size_seen_in_one_publish_only_or_within_two_seconds_of_a_replacement_waits() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let claimed = panels.backend().calls().len();
        let images = PanelImages::default();
        let start = Instant::now();
        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        panels.maintain(&images, start, panel_size_by_aspect);
        panels.maintain(
            &images,
            start + Duration::from_secs(1),
            panel_size_by_aspect,
        );
        assert_eq!(
            calls_from(&panels, claimed),
            Vec::<String>::new(),
            "no second publish"
        );

        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        let replaced_at = start + Duration::from_secs(2);
        panels.maintain(&images, replaced_at, panel_size_by_aspect);
        images.publish_rgba8("camera", 4, 4, rgba(4, 4, 1)).unwrap();
        panels.maintain(&images, replaced_at, panel_size_by_aspect);
        images.publish_rgba8("camera", 4, 4, rgba(4, 4, 1)).unwrap();
        panels.maintain(
            &images,
            replaced_at + Duration::from_millis(1999),
            panel_size_by_aspect,
        );
        let creates = |panels: &StreamPanels<FakeBackend>| {
            panels
                .backend()
                .calls()
                .into_iter()
                .filter(|call| call.starts_with("create sender"))
                .count()
        };
        assert_eq!(creates(&panels), 2, "spaced two seconds apart");
        panels.maintain(
            &images,
            replaced_at + REPLACEMENT_SPACING,
            panel_size_by_aspect,
        );
        assert_eq!(creates(&panels), 3);
    }

    #[test]
    fn a_failing_sender_is_replaced_at_most_three_times() {
        let backend = FakeBackend::ready();
        let failing = Rc::clone(&backend.failing);
        let mut panels = StreamPanels::new(backend);
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        *failing.borrow_mut() = true;
        let mut now = Instant::now();
        for _ in 0..6 {
            panels.pump(&images, now);
            panels.pump(&images, now);
            panels.maintain(&images, now, panel_size_by_aspect);
            now += REPLACEMENT_SPACING;
        }
        let calls = panels.backend().calls();
        let count_of = |prefix: &str| calls.iter().filter(|call| call.starts_with(prefix)).count();
        assert_eq!(
            count_of("create sender"),
            4,
            "the claim's and three more: {calls:?}"
        );
        assert_eq!(
            count_of("fail"),
            4,
            "a failed sender is not shown again: {calls:?}"
        );
        assert_eq!(count_of("set_stream"), 4);
    }

    #[test]
    fn a_panel_closed_or_gone_while_held_is_released_once_and_gets_no_call_until_reopened() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        panels
            .claim(
                "timeline.panel",
                &config(TIMELINE),
                &[],
                (4, 2),
                Some(PanelGestures::default()),
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let claimed = panels.backend().calls().len();
        let camera_closed = listed(
            CAMERA,
            GuiPanelState::Closed,
            PanelContent::Stream(StreamId([1; 16])),
        );
        let timeline_visible = listed(
            TIMELINE,
            GuiPanelState::Visible,
            PanelContent::Stream(StreamId([2; 16])),
        );
        assert_eq!(
            panels.note_listing(&[camera_closed, timeline_visible.clone()], Instant::now()),
            ["camera"]
        );
        assert_eq!(calls_from(&panels, claimed), ["destroy sender 1"]);

        assert!(
            panels.note_listing(&[], Instant::now()).is_empty(),
            "one missing listing"
        );
        assert!(
            panels
                .note_listing(&[timeline_visible], Instant::now())
                .is_empty()
        );
        assert!(
            panels.note_listing(&[], Instant::now()).is_empty(),
            "the count starts again"
        );
        assert_eq!(panels.note_listing(&[], Instant::now()), ["timeline.panel"]);
        assert_eq!(
            calls_from(&panels, claimed),
            ["destroy sender 1", "cancel gestures 33", "destroy sender 2"]
        );
        assert!(panels.is_empty());

        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, Instant::now());
        panels.maintain(&images, Instant::now(), panel_size_by_aspect);
        panels.hide_all();
        assert_eq!(
            calls_from(&panels, claimed).len(),
            3,
            "no call on a released panel"
        );
    }

    /// The camera, the plot and the timeline claimed, each with its image streamed once.
    fn camera_plot_and_timeline_streaming() -> (StreamPanels<FakeBackend>, PanelImages) {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        panels
            .claim(
                "plot",
                &config(PLOT),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        panels
            .claim(
                TIMELINE_IMAGE_KEY,
                &config(TIMELINE),
                &[],
                (4, 2),
                Some(PanelGestures::default()),
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let images = PanelImages::default();
        for key in ["camera", "plot", TIMELINE_IMAGE_KEY] {
            images.publish_rgba8(key, 4, 2, rgba(4, 2, 1)).unwrap();
        }
        for _ in 0..3 {
            panels.pump(&images, Instant::now());
        }
        assert_eq!(shows(&panels), ["show 1", "show 2", "show 3"]);
        (panels, images)
    }

    #[test]
    fn after_a_recording_switch_only_the_panels_of_views_that_left_are_hidden() {
        let (mut panels, images) = camera_plot_and_timeline_streaming();
        let before_switch = panels.backend().calls().len();

        // The new recording's layout has a plot too, but no camera.
        images.retain_views(&["plot".to_owned()]);
        let published = images.view_names();
        panels.follow_published_views(&published, &[]);
        panels.follow_published_views(&published, &published);

        assert_eq!(calls_from(&panels, before_switch), ["set_state 25 Hidden"]);
        let after_hide = panels.backend().calls().len();
        images.publish_rgba8("plot", 4, 2, rgba(4, 2, 2)).unwrap();
        for _ in 0..3 {
            panels.pump(&images, Instant::now());
        }
        assert_eq!(
            calls_from(&panels, after_hide),
            ["show 2"],
            "the hidden camera panel gets nothing, the plot keeps streaming"
        );
    }

    #[test]
    fn a_view_back_in_the_layout_is_shown_again_once_settled_and_its_image_streamed() {
        let (mut panels, images) = camera_plot_and_timeline_streaming();
        images.retain_views(&["plot".to_owned()]);
        panels.follow_published_views(&images.view_names(), &[]);
        let after_hide = panels.backend().calls().len();

        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 7)).unwrap();
        let published = images.view_names();
        panels.follow_published_views(&published, &["plot".to_owned()]);
        panels.follow_published_views(&published, &published);
        assert!(
            calls_from(&panels, after_hide).is_empty(),
            "settled but not streamed yet: still hidden"
        );

        panels.pump(&images, Instant::now());
        panels.follow_published_views(&published, &published);
        panels.follow_published_views(&published, &published);
        assert_eq!(
            calls_from(&panels, after_hide),
            ["show 1", "set_state 25 Visible"]
        );
    }

    #[test]
    fn stop_hides_only_the_panels_not_already_hidden_with_their_views() {
        let (mut panels, images) = camera_plot_and_timeline_streaming();
        images.retain_views(&[]);
        panels.follow_published_views(&images.view_names(), &[]);
        let after_hide = panels.backend().calls().len();

        panels.hide_all();

        assert_eq!(
            calls_from(&panels, after_hide),
            ["set_state 33 Hidden"],
            "the camera and plot panels are hidden already; the timeline stays until Stop"
        );
    }

    fn config_placed_at(common_name: &str, x: f32, y: f32, z: f32) -> GuiPanelConfig {
        GuiPanelConfig {
            pose: Pose {
                orientation: Quat::IDENTITY,
                position: Vec3::new(x, y, z),
            },
            ..config(common_name)
        }
    }

    #[test]
    fn a_panel_moved_by_its_claim_is_sized_moved_and_shown_then_all_three_again_with_its_first_frame()
     {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        let moved = PanelPlace::MovedBecause("held 9.1 m from the eyes".to_owned());
        panels
            .claim(
                "camera",
                &config_placed_at(CAMERA, 0.25, -0.3, -0.4),
                &[hidden_by_an_earlier_run(CAMERA)],
                (4, 2),
                None,
                &moved,
            )
            .unwrap();
        let claim_calls = [
            "create sender 1 4x2",
            "get_or_create camera.stream.rerun.rerun stream 1",
            "set_stream 25 1",
            "set_size 25 0.300",
            "set_pose 25 [0.25, -0.30, -0.40]",
            "set_state 25 Visible",
        ];
        assert_eq!(panels.backend().calls(), claim_calls);

        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, Instant::now());
        panels.pump(&images, Instant::now() + STREAM_REFRESH);
        assert_eq!(
            calls_from(&panels, claim_calls.len()),
            [
                "show 1",
                "set_size 25 0.300",
                "set_pose 25 [0.25, -0.30, -0.40]",
                "set_state 25 Visible",
                "show 1"
            ],
            "the stale refusal of the claim's calls is answered once, with the first frame"
        );
    }

    #[test]
    fn a_panel_kept_where_it_is_held_gets_no_pose_call() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config_placed_at(CAMERA, 0.25, -0.3, -0.4),
                &[hidden_by_an_earlier_run(CAMERA)],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, Instant::now());
        let calls = panels.backend().calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("set_pose")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_panel_closed_while_held_is_released_at_the_next_sync_and_not_hidden_at_stop() {
        let (mut panels, images) = camera_plot_and_timeline_streaming();
        panels.backend().close_elsewhere(panel_id_of(CAMERA));
        let closed = panels.backend().calls().len();

        panels.maintain(&images, Instant::now(), panel_size_by_aspect);
        assert_eq!(calls_from(&panels, closed), ["destroy sender 1"]);
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 2)).unwrap();
        panels.pump(&images, Instant::now());
        let released = panels.backend().calls().len();

        panels.hide_all();
        panels.close();
        assert_eq!(
            calls_from(&panels, released),
            [
                "set_state 23 Hidden",
                "set_state 33 Hidden",
                "cancel gestures 33",
                "destroy sender 2",
                "destroy sender 3"
            ],
            "the closed camera panel stays closed: no show, no hide"
        );
    }

    #[test]
    fn a_panel_closed_after_the_last_sync_is_not_hidden_at_stop() {
        let (panels, _images) = camera_plot_and_timeline_streaming();
        panels.backend().close_elsewhere(panel_id_of(PLOT));
        let closed = panels.backend().calls().len();

        panels.hide_all();
        assert_eq!(
            calls_from(&panels, closed),
            ["set_state 25 Hidden", "set_state 33 Hidden"]
        );
    }

    #[test]
    fn without_a_state_watch_a_claimed_panel_is_hidden_at_stop_as_before() {
        let backend = FakeBackend {
            state_watch_fails: true,
            ..FakeBackend::default()
        };
        let mut panels = StreamPanels::new(backend);
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        panels.backend().close_elsewhere(panel_id_of(CAMERA));
        let claimed = panels.backend().calls().len();
        panels.hide_all();
        assert_eq!(calls_from(&panels, claimed), ["set_state 25 Hidden"]);
    }

    #[test]
    fn a_reset_closes_the_panel_through_the_close_call() {
        let panels = StreamPanels::new(FakeBackend::default());
        panels.close_for_reset(panel_id_of(CAMERA)).unwrap();
        assert_eq!(panels.backend().calls(), ["close 25"]);
    }

    #[test]
    fn a_panel_closed_as_its_headset_left_is_reopened_once_a_head_streams_after_the_close() {
        let (mut panels, images) = camera_plot_and_timeline_streaming();
        let start = Instant::now();
        panels.note_target_head(start);
        panels.backend().close_elsewhere(panel_id_of(CAMERA));
        panels.maintain(
            &images,
            start + Duration::from_secs(1),
            panel_size_by_aspect,
        );
        assert!(
            panels.panels_to_reopen().is_empty(),
            "no head streamed since the close: the headset left with it"
        );
        panels.maintain(
            &images,
            start + Duration::from_secs(2),
            panel_size_by_aspect,
        );
        assert!(panels.panels_to_reopen().is_empty());

        panels.note_target_head(start + Duration::from_secs(10));
        assert_eq!(panels.panels_to_reopen(), [panel_id_of(CAMERA)]);
        assert!(panels.panels_to_reopen().is_empty(), "reopened once");

        panels.backend().forget_the_close(panel_id_of(CAMERA));
        let released = panels.backend().calls().len();
        panels
            .claim(
                "camera",
                &config_placed_at(CAMERA, 0.25, -0.3, -0.4),
                &[closed_at(0.7, -0.3, 0.3)],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        assert_eq!(
            calls_from(&panels, released),
            [
                "create sender 4 4x2",
                "get_or_create camera.stream.rerun.rerun stream 4",
                "set_stream 25 4",
                "set_size 25 0.300",
                "set_pose 25 [0.70, -0.30, 0.30]",
                "set_state 25 Visible"
            ]
        );
        panels.maintain(
            &images,
            start + Duration::from_secs(11),
            panel_size_by_aspect,
        );
        panels.hide_all();
        assert!(
            panels
                .backend()
                .calls()
                .contains(&"set_state 25 Hidden".to_owned()),
            "the reopened panel is held again and hidden at Stop"
        );
    }

    #[test]
    fn a_panel_closed_while_a_head_streams_on_is_reopened_at_the_next_head() {
        let (mut panels, images) = camera_plot_and_timeline_streaming();
        let start = Instant::now();
        panels.backend().close_elsewhere(panel_id_of(PLOT));
        panels.maintain(&images, start, panel_size_by_aspect);
        panels.note_target_head(start + Duration::from_millis(16));
        assert_eq!(
            panels.panels_to_reopen(),
            [panel_id_of(PLOT)],
            "a close in the Hub Display cannot be told from a mixer's, so it is undone too"
        );
    }

    #[test]
    fn a_panel_closed_again_after_three_reopens_stays_closed_for_the_run() {
        let (mut panels, images) = camera_plot_and_timeline_streaming();
        let mut now = Instant::now();
        for reopen in 1..=4 {
            panels.backend().close_elsewhere(panel_id_of(PLOT));
            panels.maintain(&images, now, panel_size_by_aspect);
            now += Duration::from_secs(1);
            panels.note_target_head(now);
            let reopened = panels.panels_to_reopen();
            if reopen > 3 {
                assert!(reopened.is_empty(), "the fourth close stands");
                break;
            }
            assert_eq!(reopened, [panel_id_of(PLOT)], "reopen {reopen}");
            panels.backend().forget_the_close(panel_id_of(PLOT));
            panels
                .claim(
                    "plot",
                    &config(PLOT),
                    &[],
                    (4, 2),
                    None,
                    &PanelPlace::AsHeld,
                )
                .unwrap();
        }
    }

    fn claim_unready_camera() -> StreamPanels<FakeBackend> {
        let mut panels = StreamPanels::new(FakeBackend::default());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        panels
    }

    #[test]
    fn a_claimed_sender_holds_back_claims_until_its_first_frame_shows() {
        let mut panels = claim_unready_camera();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        let now = Instant::now();
        panels.pump(&images, now);
        assert_eq!(shows(&panels), Vec::<String>::new());
        assert!(panels.awaits_a_first_frame(now));

        *panels.backend().ready.borrow_mut() = true;
        panels.pump(&images, now);
        assert_eq!(shows(&panels), ["show 1"]);
        assert!(!panels.awaits_a_first_frame(now));
    }

    #[test]
    fn a_sender_no_mixer_connects_stops_holding_back_claims_after_the_wait() {
        let panels = claim_unready_camera();
        let claimed = Instant::now();
        assert!(panels.awaits_a_first_frame(claimed));
        assert!(!panels.awaits_a_first_frame(claimed + FIRST_FRAME_WAIT));
    }

    #[test]
    fn a_replacement_waits_for_another_panel_s_first_frame_then_holds_back_claims_itself() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim(
                "camera",
                &config(CAMERA),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        let images = PanelImages::default();
        let start = Instant::now();
        images.publish_rgba8("camera", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, start);
        *panels.backend().ready.borrow_mut() = false;
        panels
            .claim(
                "plot",
                &config(PLOT),
                &[],
                (4, 2),
                None,
                &PanelPlace::AsHeld,
            )
            .unwrap();
        images.publish_rgba8("camera", 8, 2, rgba(8, 2, 1)).unwrap();
        panels.maintain(&images, start, panel_size_by_aspect);
        images.publish_rgba8("camera", 8, 2, rgba(8, 2, 2)).unwrap();
        panels.maintain(&images, start, panel_size_by_aspect);
        let calls = panels.backend().calls();
        assert!(
            !calls.iter().any(|call| call.starts_with("create sender 3")),
            "no replacement while the plot's first frame is due: {calls:?}"
        );

        *panels.backend().ready.borrow_mut() = true;
        images.publish_rgba8("plot", 4, 2, rgba(4, 2, 1)).unwrap();
        panels.pump(&images, start);
        assert_eq!(shows(&panels), ["show 1", "show 2"]);
        let before_replacement = panels.backend().calls().len();
        panels.maintain(&images, start, panel_size_by_aspect);
        assert_eq!(
            calls_from(&panels, before_replacement)[..2],
            ["create sender 3 8x2", "set_stream 25 3"]
        );
        assert!(panels.awaits_a_first_frame(start));
    }
}
