//! The stream panels: one video sender per claimed panel, fed from [`PanelImages`] on
//! the QAROS thread, a replacement sender when the image's size settles on a new value or the
//! sender fails, and the timeline panel's gesture subscription.
//!
//! Every QAROS call goes through [`StreamBackend`], so the claim, the shows, the replacements
//! and the Stop order are tested with a fake on hosts without a mixer (`qaros-stub` implements
//! none of the video sender, `set_stream` or gesture calls).

use std::rc::Rc;
use std::time::{Duration, Instant};

use qaros::{GuiPanelConfig, GuiPanelId, GuiPanelSize, GuiPanelState, StreamId};

use crate::connection::QarosConnection;
use crate::panel_gestures::{PanelGestureSubscription, PanelGestures};
use crate::panel_images::{PanelImage, PanelImages};
use crate::panel_listing::{ListedPanel, PanelContent};
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

/// How often per run a failed sender is replaced before the panel is left as it is.
const FAILURE_REPLACEMENT_LIMIT: u32 = 3;

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

/// Every QAROS call of the stream panels.
pub(crate) trait StreamBackend {
    type Sender: StreamSender;
    type Subscription;

    fn create_sender(&self, width: u32, height: u32) -> Result<Self::Sender, String>;
    fn get_or_create_stream(
        &self,
        config: &GuiPanelConfig,
        stream: StreamId,
    ) -> Result<GuiPanelId, String>;
    fn set_stream(&self, panel: GuiPanelId, stream: StreamId) -> Result<(), String>;
    fn set_size(&self, panel: GuiPanelId, size: GuiPanelSize) -> Result<(), String>;
    fn set_state(&self, panel: GuiPanelId, state: GuiPanelState) -> Result<(), String>;
    fn subscribe_gestures(
        &self,
        panel: GuiPanelId,
        inbox: PanelGestures,
    ) -> Result<Self::Subscription, String>;
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

impl StreamBackend for QarosStreamBackend {
    type Sender = VideoSender;
    type Subscription = PanelGestureSubscription;

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

    fn set_state(&self, panel: GuiPanelId, state: GuiPanelState) -> Result<(), String> {
        self.connection
            .session()
            .gui_panels()
            .set_state(panel, state)
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
pub(crate) struct PanelStream<Sender, Subscription> {
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
    health: SenderHealth,
    failure_replacements: u32,
    last_replacement: Option<Instant>,
    size_candidate: Option<SizeCandidate>,
    absent_listings: u32,
}

pub(crate) struct StreamPanels<Backend: StreamBackend> {
    backend: Backend,
    streams: Vec<PanelStream<Backend::Sender, Backend::Subscription>>,
    /// Where the next round-robin show search starts.
    next_stream: usize,
}

impl<Backend: StreamBackend> StreamPanels<Backend> {
    pub(crate) fn new(backend: Backend) -> Self {
        Self {
            backend,
            streams: Vec::new(),
            next_stream: 0,
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
    ///    run's sender, so it gets `set_stream`; so does one a user closed (ADR-0075).
    /// 4. The size follows this run's image; a panel the earlier run hid is shown again once
    ///    its stream is in place.
    /// 5. With `gestures`, the panel's gestures are subscribed into it.
    pub(crate) fn claim(
        &mut self,
        image_key: &str,
        config: &GuiPanelConfig,
        listed: &[ListedPanel],
        (width, height): (u32, u32),
        gestures: Option<PanelGestures>,
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
        if let Some(earlier) = earlier {
            log::info!(
                "qaros: stream panel {panel} reattached (listed as {:?}, {:?}, showing {:?}); \
                 now showing stream {stream}",
                earlier.display_name,
                earlier.state,
                earlier.content
            );
            // Shown again only once its stream is in place (ADR-0054); a panel a user
            // minimised stays minimised.
            if earlier.state == GuiPanelState::Hidden {
                self.backend.set_state(panel, GuiPanelState::Visible)?;
                log::info!("qaros: stream panel {panel} shown again after the earlier run hid it");
            }
        } else {
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
        }
        log::info!(
            "qaros: {image_key} streams to panel {panel} {listed_name} at {width}x{height} px on \
             {:.3}x{:.3} m, stream id {stream}",
            config.size.width,
            config.size.height
        );
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
            health: SenderHealth::Healthy,
            failure_replacements: 0,
            last_replacement: None,
            size_candidate: None,
            absent_listings: 0,
        };
        subscribe_if_wanted(&self.backend, &mut claimed);
        self.streams.push(claimed);
        Ok(panel)
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
                if !stream.sender_proven {
                    stream.sender_proven = true;
                    retire_all_senders(stream, "its successor had its first ready begin");
                }
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

    /// Once a sync: subscribes gestures that failed to subscribe, and replaces a sender that
    /// failed (at most [`FAILURE_REPLACEMENT_LIMIT`] times a run) or whose image has had a
    /// new size for two publishes in a row. Replacements of one panel are at least
    /// [`REPLACEMENT_SPACING`] apart. `panel_size` gives the panel's size in metres for an
    /// image size.
    pub(crate) fn maintain(
        &mut self,
        images: &PanelImages,
        now: Instant,
        panel_size: impl Fn(u32, u32) -> GuiPanelSize,
    ) {
        for index in 0..self.streams.len() {
            subscribe_if_wanted(&self.backend, &mut self.streams[index]);
            let image = images.latest(&self.streams[index].image_key);
            let Some((cause, width, height)) = self.replacement_due(index, image.as_ref(), now)
            else {
                continue;
            };
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
        if let Err(error) = self
            .backend
            .set_size(stream.panel, panel_size(width, height))
        {
            log::warn!(
                "qaros: resizing panel {} of {}: {error}",
                stream.panel,
                stream.image_key
            );
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
        stream.health = SenderHealth::Healthy;
        stream.size_candidate = None;
    }

    /// Checks the held panels against a listing. A panel listed `Closed`, or missing from
    /// [`ABSENT_LISTINGS_BEFORE_GONE`] listings in a row, is released: its subscription
    /// cancelled and its senders destroyed, and it gets no further call this run. A close by a
    /// mixer cannot be told from a close by a user, so it is not revived before the next run
    /// (ADR-0075). Returns the image keys released.
    pub(crate) fn note_listing(&mut self, listed: &[ListedPanel]) -> Vec<String> {
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
            let mut stream = self.streams.remove(index);
            log::warn!(
                "qaros: stream panel {} of {} {how} while this run holds it. A QAROS 0.9.0-dev.3177 \
                 mixer whose receiver fails closes the panel for every viewer and closed panels \
                 leave the listing (Quaternar/Quaternar#344, after a VulkanDeviceLost on a \
                 reattached stream, #343); a close in the Hub Display looks the same, so this run \
                 streams nothing more to it and the next run claims it again by name (ADR-0075)",
                stream.panel,
                stream.image_key
            );
            drop(stream.subscription.take());
            released.push(stream.image_key.clone());
        }
        if self.next_stream >= self.streams.len() {
            self.next_stream = 0;
        }
        released
    }

    /// The first half of Stop: every held panel hidden for the next run.
    pub(crate) fn hide_all(&self) {
        for stream in &self.streams {
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

    /// The second half of Stop, before the session goes: every gesture subscription cancelled
    /// (the cancel waits for a running callback), then every sender destroyed, retiring ones
    /// included.
    pub(crate) fn close(&mut self) {
        let started = Instant::now();
        for stream in &mut self.streams {
            drop(stream.subscription.take());
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

/// Subscribes the panel's gestures when it wants them and has no subscription; a failure is
/// logged once and tried again at the next maintain.
fn subscribe_if_wanted<Backend: StreamBackend>(
    backend: &Backend,
    stream: &mut PanelStream<Backend::Sender, Backend::Subscription>,
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

fn retire_overdue_senders<Sender: StreamSender, Subscription>(
    stream: &mut PanelStream<Sender, Subscription>,
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
fn retire_all_senders<Sender: StreamSender, Subscription>(
    stream: &mut PanelStream<Sender, Subscription>,
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

    #[derive(Default)]
    struct FakeBackend {
        log: CallLog,
        senders_created: RefCell<u8>,
        ready: Rc<RefCell<bool>>,
        failing: Rc<RefCell<bool>>,
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
    }

    impl StreamBackend for FakeBackend {
        type Sender = FakeSender;
        type Subscription = FakeSubscription;

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

        fn set_state(&self, panel: GuiPanelId, state: GuiPanelState) -> Result<(), String> {
            self.log
                .borrow_mut()
                .push(format!("set_state {} {state:?}", panel.0[0]));
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
            .claim("camera", &config(CAMERA), &[], (640, 426), None)
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
            .claim("camera", &config(CAMERA), &[earlier], (640, 426), None)
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
            .claim("camera", &config(CAMERA), &[minimised], (640, 426), None)
            .unwrap();
        let calls = panels.backend().calls();
        assert_eq!(calls[2], "set_stream 25 1");
        assert!(
            !calls.iter().any(|call| call.starts_with("set_state")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_listed_closed_panel_is_claimed_by_name_and_gets_the_new_stream() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        let closed = listed(
            CAMERA,
            GuiPanelState::Closed,
            PanelContent::Stream(StreamId([9; 16])),
        );
        panels
            .claim("camera", &config(CAMERA), &[closed], (640, 426), None)
            .unwrap();
        let calls = panels.backend().calls();
        assert_eq!(calls[1], "get_or_create camera.stream.rerun.rerun stream 1");
        assert_eq!(calls[2], "set_stream 25 1");
        assert!(
            !calls.iter().any(|call| call.starts_with("set_state")),
            "{calls:?}"
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
        let refused = panels.claim("camera", &config(CAMERA), &[website], (640, 426), None);
        assert!(refused.unwrap_err().contains("website panel"));
        assert!(panels.backend().calls().is_empty());
    }

    #[test]
    fn two_views_and_a_restart_reclaim_both_panels_with_set_stream_and_no_duplicate() {
        let first_run_backend = FakeBackend::default();
        let log = Rc::clone(&first_run_backend.log);
        let mut first_run = StreamPanels::new(first_run_backend);
        let camera = first_run
            .claim("camera", &config(CAMERA), &[], (64, 48), None)
            .unwrap();
        let plot = first_run
            .claim("plot", &config(PLOT), &[], (64, 48), None)
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
            )
            .unwrap();
        let plot_again = second_run
            .claim("plot", &config(PLOT), &listed_after_stop, (64, 48), None)
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
            .claim("camera", &config(CAMERA), &[], (4, 2), None)
            .unwrap();
        panels
            .claim(
                "timeline.panel",
                &config(TIMELINE),
                &[],
                (4, 2),
                Some(PanelGestures::default()),
            )
            .unwrap();
        let images = PanelImages::default();
        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        let start = Instant::now();
        panels.maintain(&images, start, panel_size_by_aspect);
        images.publish_rgba8("camera", 2, 2, rgba(2, 2, 1)).unwrap();
        panels.maintain(
            &images,
            start + Duration::from_secs(1),
            panel_size_by_aspect,
        );
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
            .claim("camera", &config(CAMERA), &[], (4, 2), None)
            .unwrap();
        panels
            .claim("plot", &config(PLOT), &[], (4, 2), None)
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
            .claim("camera", &config(CAMERA), &[], (4, 2), None)
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
        assert_eq!(&calls[calls.len() - 2..], ["not ready 1", "show 1"]);
    }

    #[test]
    fn a_size_stable_for_two_publishes_replaces_the_sender_and_the_old_one_goes_at_the_first_ready_begin()
     {
        let backend = FakeBackend::default();
        let ready = Rc::clone(&backend.ready);
        let mut panels = StreamPanels::new(backend);
        panels
            .claim("camera", &config(CAMERA), &[], (4, 2), None)
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
            ["not ready 2", "show 2", "destroy sender 1"]
        );
    }

    #[test]
    fn an_old_sender_whose_successor_never_gets_a_receiver_goes_after_ten_seconds() {
        let mut panels = StreamPanels::new(FakeBackend::default());
        panels
            .claim("camera", &config(CAMERA), &[], (4, 2), None)
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
            .claim("camera", &config(CAMERA), &[], (4, 2), None)
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
            .claim("camera", &config(CAMERA), &[], (4, 2), None)
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
    fn a_panel_closed_or_gone_while_held_is_released_once_and_not_revived() {
        let mut panels = StreamPanels::new(FakeBackend::ready());
        panels
            .claim("camera", &config(CAMERA), &[], (4, 2), None)
            .unwrap();
        panels
            .claim(
                "timeline.panel",
                &config(TIMELINE),
                &[],
                (4, 2),
                Some(PanelGestures::default()),
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
            panels.note_listing(&[camera_closed, timeline_visible.clone()]),
            ["camera"]
        );
        assert_eq!(calls_from(&panels, claimed), ["destroy sender 1"]);

        assert!(panels.note_listing(&[]).is_empty(), "one missing listing");
        assert!(panels.note_listing(&[timeline_visible]).is_empty());
        assert!(
            panels.note_listing(&[]).is_empty(),
            "the count starts again"
        );
        assert_eq!(panels.note_listing(&[]), ["timeline.panel"]);
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
}
