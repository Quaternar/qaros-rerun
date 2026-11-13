//! Where QAROS users come from: [`TargetPeers`] finds the targets to render for in the peer
//! list, as the header's "Basic flow" prescribes, and reports them as they join and leave.
//! Another source (a dedicated new-user callback, say) plugs in by implementing
//! [`UserEventSource`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use frame_sink::{RenderPolicy, RosterChange, UserEvent, UserId, UserInfo, UserRoster};
use qaros::{AppState, PeerId, PeerInfo};

use crate::connection::QarosConnection;
use crate::unanswered::UnansweredPeers;

/// Who is rendered for, by peer: the [`UserEventSource`]'s joins and leaves, with a user id per
/// peer while it is present. Shared by the CPU and the Vulkan sink, which each create and drop
/// their own kind of render sender.
///
/// A peer that joins becomes a candidate, and the sink creates senders for the candidates in
/// parallel (hedged): a listed peer that does not answer costs its creation's
/// timeout in the background, not a live peer's wait. A candidate takes a slot of the
/// [`UserRoster`] only once its sender exists; the first created get the free slots, and a
/// sender created when every slot is taken goes again, its candidate waiting for a slot.
/// Candidates that asked for a stream come first, then the others in the order they joined,
/// then the held-back ones ([`PeerEvent::Joined::held_back`]): those are tried outside
/// [`HEDGED_CREATION_LIMIT`] and take a slot only when one remains for every other candidate.
///
/// Frames are keyed by user id and a frame id that restarts at 0 with every new sender, so a
/// user id is never reused: a peer that leaves and joins again, even within one poll, comes
/// back as a new user, and no frame of its old sender can be taken for one of the new.
pub(crate) struct PeerUsers {
    events: Box<dyn UserEventSource>,
    roster: UserRoster,
    /// The present peers' users, candidates and rendered; a peer leaves this map when its user
    /// is released.
    users_by_peer: BTreeMap<PeerId, UserId>,
    candidates: Vec<Candidate>,
    next_user_number: u32,
    /// Begins and shows that failed in a row per user since its last shown frame.
    frame_failures: BTreeMap<UserId, FailureRun>,
    /// Users whose begins were refused for an unresolved app volume, warned about once.
    unresolved_volume_warned: BTreeSet<UserId>,
    /// Creations still running for users that left: they count against
    /// [`HEDGED_CREATION_LIMIT`] (when listed) and keep a rejoined peer from a second
    /// creation until they resolve.
    departed_creations: BTreeMap<UserId, DepartedCreation>,
}

#[derive(Clone, Copy, Debug)]
struct DepartedCreation {
    peer: PeerId,
    counts_against_the_limit: bool,
}

/// A present user without a slot.
#[derive(Clone, Debug)]
struct Candidate {
    user: UserInfo,
    peer: PeerId,
    requested_a_stream: bool,
    held_back: bool,
    sender_in_creation: bool,
}

impl Candidate {
    fn counts_against_the_limit(&self) -> bool {
        !self.requested_a_stream && !self.held_back
    }
}

/// Creations for candidates found in the peer list that run at once. Candidates that asked for
/// a stream and held-back ones are not counted against it.
pub const HEDGED_CREATION_LIMIT: usize = 8;

/// What becomes of a user whose sender was just created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    /// The user took a free slot: render for it with the sender.
    Admitted(UserInfo),
    /// Every slot is taken: the sender goes, and the user waits as a candidate.
    NoFreeSlot,
    /// The user left while its sender was created: the sender goes.
    Left,
}

/// How long begins and shows of one user may keep failing, with no frame shown between them,
/// before the user is released. A begin that fails for good (an image format the sink cannot
/// render into, say) would otherwise hold the user's slot for good.
pub const FRAME_FAILURE_PERIOD: Duration = Duration::from_secs(1);

/// Failures in a row that the release also needs, so that a few failures spread over a pause
/// of the viewer are not taken for a lasting error.
pub const FRAME_FAILURE_MINIMUM: u32 = 10;

/// Begins and shows of one user that failed in a row, since its last shown frame.
#[derive(Clone, Copy, Debug)]
struct FailureRun {
    first_failure: Instant,
    failures: u32,
}

impl PeerUsers {
    pub(crate) fn new(events: Box<dyn UserEventSource>, policy: Box<dyn RenderPolicy>) -> Self {
        Self {
            events,
            roster: UserRoster::new(policy),
            users_by_peer: BTreeMap::new(),
            candidates: Vec::new(),
            next_user_number: 1,
            frame_failures: BTreeMap::new(),
            unresolved_volume_warned: BTreeSet::new(),
            departed_creations: BTreeMap::new(),
        }
    }

    pub(crate) fn rendered(&self) -> &[UserInfo] {
        self.roster.rendered()
    }

    /// The candidates, in the order their senders are tried.
    pub(crate) fn waiting(&self) -> Vec<UserInfo> {
        self.candidates
            .iter()
            .map(|candidate| candidate.user.clone())
            .collect()
    }

    /// The user's place among the candidates, for picking up created senders in that order;
    /// past every candidate for a user that is none.
    pub(crate) fn candidate_rank(&self, user: UserId) -> usize {
        self.candidates
            .iter()
            .position(|candidate| candidate.user.id == user)
            .unwrap_or(usize::MAX)
    }

    pub(crate) fn peer_of(&self, user: UserId) -> Option<PeerId> {
        self.users_by_peer
            .iter()
            .find(|(_, candidate)| **candidate == user)
            .map(|(peer, _)| *peer)
    }

    /// The peer's user while it is present, else a new user id.
    fn user_joining(&mut self, peer: PeerId) -> UserId {
        let next_user_number = &mut self.next_user_number;
        *self.users_by_peer.entry(peer).or_insert_with(|| {
            let user = UserId(*next_user_number);
            *next_user_number = next_user_number.saturating_add(1);
            user
        })
    }

    /// Takes the source's events and returns the rendered users whose peers left, for the
    /// sink to drop their senders.
    pub(crate) fn poll(&mut self) -> Vec<UserId> {
        let events = self.events.poll_events();
        self.apply_peer_events(events)
    }

    /// Before the sink begins frames: releases the users whose peers asked for no frame in the
    /// source's stall timeout, as far as the source lets them go while a candidate waits for a
    /// slot, and returns them.
    pub(crate) fn release_stalled(&mut self) -> Vec<UserId> {
        let slot_wanted = self.is_slot_wanted();
        let events = self.events.leave_stalled(slot_wanted);
        self.apply_peer_events(events)
    }

    /// The first candidate in line without a creation would not get a slot now. A candidate
    /// whose creation still runs wants none yet: it may be a target that never answers;
    /// nor does a held-back one, which did not answer before.
    fn is_slot_wanted(&mut self) -> bool {
        self.candidates
            .iter()
            .find(|candidate| !candidate.sender_in_creation && !candidate.held_back)
            .is_some_and(|first| !self.roster.would_render(&first.user))
    }

    fn apply_peer_events(&mut self, events: Vec<PeerEvent>) -> Vec<UserId> {
        let mut released = Vec::new();
        for event in events {
            match event {
                PeerEvent::Joined {
                    peer,
                    label,
                    requested_a_stream,
                    held_back,
                } => {
                    let user = self.user_joining(peer);
                    self.add_candidate(
                        UserInfo { id: user, label },
                        peer,
                        requested_a_stream,
                        held_back,
                    );
                }
                PeerEvent::Left { peer } => {
                    if let Some(user) = self.users_by_peer.remove(&peer) {
                        self.keep_counting_a_running_creation(user);
                        released.extend(self.drop_user(user));
                    }
                }
                PeerEvent::AskedForAStream { peer } => {
                    if let Some(user) = self.users_by_peer.get(&peer).copied() {
                        self.raise_to_the_requesters(user);
                    }
                }
            }
        }
        released
    }

    /// A held-back candidate that asks for a stream moves to the requesters' group, keeping a
    /// creation that already runs, so its sender is not dropped for want of a slot behind the
    /// other candidates. Other candidates keep their place: a listed target asks for content
    /// once a sender attached it, and that is no reason to count it outside the creation limit.
    fn raise_to_the_requesters(&mut self, user: UserId) {
        let Some(index) = self
            .candidates
            .iter()
            .position(|candidate| candidate.user.id == user && candidate.held_back)
        else {
            return;
        };
        let mut candidate = self.candidates.remove(index);
        log::info!(
            "qaros: {user} (peer {}) asked for a stream during its background retry; it goes \
             ahead of the listed targets",
            candidate.peer
        );
        candidate.requested_a_stream = true;
        candidate.held_back = false;
        let place = self
            .candidates
            .iter()
            .position(|queued| !queued.requested_a_stream)
            .unwrap_or(self.candidates.len());
        self.candidates.insert(place, candidate);
    }

    fn keep_counting_a_running_creation(&mut self, user: UserId) {
        if let Some(candidate) = self
            .candidates
            .iter()
            .find(|candidate| candidate.user.id == user && candidate.sender_in_creation)
        {
            self.departed_creations.insert(
                user,
                DepartedCreation {
                    peer: candidate.peer,
                    counts_against_the_limit: candidate.counts_against_the_limit(),
                },
            );
        }
    }

    fn is_creating_for_departed(&self, peer: PeerId) -> bool {
        self.departed_creations
            .values()
            .any(|departed| departed.peer == peer)
    }

    fn add_candidate(
        &mut self,
        user: UserInfo,
        peer: PeerId,
        requested_a_stream: bool,
        held_back: bool,
    ) {
        let known = self.roster.is_rendered(user.id)
            || self
                .candidates
                .iter()
                .any(|candidate| candidate.user.id == user.id);
        if known {
            return;
        }
        let candidate = Candidate {
            user,
            peer,
            requested_a_stream,
            held_back: held_back && !requested_a_stream,
            sender_in_creation: false,
        };
        let first_after = |queued: &Candidate| {
            if candidate.requested_a_stream {
                !queued.requested_a_stream
            } else if candidate.held_back {
                false
            } else {
                queued.held_back
            }
        };
        let place = self
            .candidates
            .iter()
            .position(first_after)
            .unwrap_or(self.candidates.len());
        self.candidates.insert(place, candidate);
    }

    /// Removes the user, candidate or rendered; returns it when it was rendered.
    fn drop_user(&mut self, user: UserId) -> Option<UserId> {
        self.candidates
            .retain(|candidate| candidate.user.id != user);
        self.frame_failures.remove(&user);
        self.unresolved_volume_warned.remove(&user);
        self.roster
            .apply([UserEvent::Left(user)])
            .contains(&RosterChange::Released(user))
            .then_some(user)
    }

    /// The candidates to create senders for now, in order, each marked as in creation: while
    /// a slot is free, every candidate without a creation, up to [`HEDGED_CREATION_LIMIT`]
    /// creations for candidates from the peer list (creations of departed users included), and
    /// a held-back candidate only while a slot remains for every other candidate. A peer whose
    /// departed user's creation still runs waits for it to resolve.
    pub(crate) fn creations_to_start(&mut self) -> Vec<(UserInfo, PeerId)> {
        let mut listed_creations = self
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.sender_in_creation && candidate.counts_against_the_limit()
            })
            .count()
            + self
                .departed_creations
                .values()
                .filter(|departed| departed.counts_against_the_limit)
                .count();
        let mut to_start = Vec::new();
        for index in 0..self.candidates.len() {
            let candidate = &self.candidates[index];
            if candidate.sender_in_creation || self.is_creating_for_departed(candidate.peer) {
                continue;
            }
            if candidate.held_back {
                if !self.would_render_behind_the_others(index) {
                    break;
                }
            } else if !self.roster.would_render(&candidate.user) {
                break;
            }
            if self.candidates[index].counts_against_the_limit() {
                if listed_creations >= HEDGED_CREATION_LIMIT {
                    continue;
                }
                listed_creations += 1;
            }
            let candidate = &mut self.candidates[index];
            candidate.sender_in_creation = true;
            to_start.push((candidate.user.clone(), candidate.peer));
        }
        for (_, peer) in &to_start {
            self.events.creation_started(*peer);
        }
        to_start
    }

    /// The held-back candidate at `index` would get a slot with every candidate that is not
    /// held back rendered first.
    fn would_render_behind_the_others(&mut self, index: usize) -> bool {
        let ahead: Vec<UserInfo> = self
            .candidates
            .iter()
            .filter(|candidate| !candidate.held_back)
            .map(|candidate| candidate.user.clone())
            .collect();
        let held = self.candidates[index].user.clone();
        self.roster.would_render_behind(&held, &ahead)
    }

    /// The user's sender was created: it takes a free slot, if there is one (a held-back
    /// user only one left over for every other candidate). Its peer answered, so it is no
    /// longer held back.
    pub(crate) fn creation_succeeded(&mut self, user: UserId) -> Admission {
        if self.departed_creations.remove(&user).is_some() {
            return Admission::Left;
        }
        let Some(index) = self
            .candidates
            .iter()
            .position(|candidate| candidate.user.id == user)
        else {
            return Admission::Left;
        };
        let has_a_slot = if self.candidates[index].held_back {
            self.would_render_behind_the_others(index)
        } else {
            self.roster.would_render(&self.candidates[index].user)
        };
        let peer = self.candidates[index].peer;
        self.candidates[index].held_back = false;
        self.events.answered(peer);
        if !has_a_slot {
            self.candidates[index].sender_in_creation = false;
            return Admission::NoFreeSlot;
        }
        let candidate = self.candidates.remove(index);
        self.roster
            .apply([UserEvent::Joined(candidate.user.clone())]);
        Admission::Admitted(candidate.user)
    }

    /// Stops rendering for the user, or trying to, from the sink's side (its stream closed,
    /// its sender could not be created, its begins kept failing). The peer joins anew, as a
    /// new user, when the source reports it again and its back-off for `cause` has passed.
    pub(crate) fn release(&mut self, user: UserId, cause: ReleaseCause) {
        self.departed_creations.remove(&user);
        if let Some(peer) = self.peer_of(user) {
            self.users_by_peer.remove(&peer);
            self.events.forget(peer, cause);
        }
        self.drop_user(user);
    }

    /// A begin or a show for the user failed with something other than a closed stream. Warns
    /// on the first failure in a row only; true once failures in a row have lasted
    /// [`FRAME_FAILURE_PERIOD`] and numbered [`FRAME_FAILURE_MINIMUM`], and the caller then
    /// releases the user.
    pub(crate) fn frame_failed(&mut self, user: UserId, error: &dyn Display) -> bool {
        self.frame_failed_at(user, error, Instant::now())
    }

    fn frame_failed_at(&mut self, user: UserId, error: &dyn Display, now: Instant) -> bool {
        let run = self.frame_failures.entry(user).or_insert(FailureRun {
            first_failure: now,
            failures: 0,
        });
        run.failures += 1;
        let failing_for = now.saturating_duration_since(run.first_failure);
        if run.failures == 1 {
            log::warn!("qaros: a frame for {user} failed: {error}");
        } else if run.failures >= FRAME_FAILURE_MINIMUM && failing_for >= FRAME_FAILURE_PERIOD {
            log::warn!(
                "qaros: frames for {user} kept failing for {failing_for:?}, {} in a row \
                 ({error}); releasing it",
                run.failures
            );
            return true;
        } else {
            log::debug!("qaros: a frame for {user} failed: {error}");
        }
        false
    }

    /// A begin for the user was refused because its app volume is not resolved where the
    /// receiver runs: not ready rather than failed, as the header says to keep beginning.
    /// Warns once per user.
    pub(crate) fn volume_unresolved(&mut self, user: UserId) {
        if self.unresolved_volume_warned.insert(user) {
            log::warn!(
                "qaros: the app volume is not resolved for {user} yet; its frames begin once it \
                 is"
            );
        }
    }

    /// A frame of the user was shown: its frames work again.
    pub(crate) fn frame_shown(&mut self, user: UserId) {
        self.frame_failures.remove(&user);
        self.unresolved_volume_warned.remove(&user);
    }

    /// The sink is streaming to the user's peer.
    pub(crate) fn stream_started(&mut self, user: UserId) {
        if let Some(peer) = self.peer_of(user) {
            self.events.stream_started(peer);
        }
    }

    /// A frame for the user began: its peer still asks for frames.
    pub(crate) fn frame_begun(&mut self, user: UserId) {
        if let Some(peer) = self.peer_of(user) {
            self.events.frame_begun(peer);
        }
    }
}

/// Why the sink stopped rendering for a user on its own, which sets the peer's back-off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseCause {
    /// The target did not answer: its sender creation timed out or was given up. Such a peer
    /// (listed as running, but not answering) waits the longest back-off, until news
    /// shows it alive.
    Unanswered,
    /// Anything else: a failed creation, a closed stream, a failed show, begins that keep
    /// failing.
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerEvent {
    /// The peer wants a render stream of its own.
    Joined {
        peer: PeerId,
        label: String,
        /// Found by its own render stream request rather than in the peer list: its sender is
        /// tried first.
        requested_a_stream: bool,
        /// A background retry of a listed target that did not answer before: tried last,
        /// outside [`HEDGED_CREATION_LIMIT`], and never ahead of another candidate for a slot.
        held_back: bool,
    },
    Left {
        peer: PeerId,
    },
    /// A present peer asked for a stream: a held-back candidate moves to the requesters' group.
    /// Any other candidate or rendered user is left as it is.
    AskedForAStream {
        peer: PeerId,
    },
}

/// Reports peers that start or stop wanting frames. Polled once per viewer frame on the UI
/// thread; implementations forward their library callbacks through channels.
pub trait UserEventSource {
    fn poll_events(&mut self) -> Vec<PeerEvent>;

    /// The sink stopped rendering for the peer on its own (see [`ReleaseCause`]); the peer
    /// joins again once the source hears from it anew and any back-off has passed.
    fn forget(&mut self, peer: PeerId, cause: ReleaseCause);

    /// Called just before the sink begins frames, so only time spent asking for frames counts:
    /// the peers that left because no frame began for them in the stall timeout. `slot_wanted`
    /// says a user waits for a slot that a stalled peer holds.
    fn leave_stalled(&mut self, _slot_wanted: bool) -> Vec<PeerEvent> {
        Vec::new()
    }

    /// The sink is streaming to the peer: a back-off from earlier failures starts over.
    fn stream_started(&mut self, _peer: PeerId) {}

    /// A sender for the peer was created, whether or not it got a slot: the peer is alive.
    fn answered(&mut self, _peer: PeerId) {}

    /// A frame for the peer began.
    fn frame_begun(&mut self, _peer: PeerId) {}

    /// A sender creation for the peer starts now.
    fn creation_started(&mut self, _peer: PeerId) {}
}

/// When a target the sink gave up on may join again. Each attempt costs a sender creation, so a
/// target whose sender keeps failing is retried after
/// [`FIRST_RETRY_DELAY`], doubling up to [`LONGEST_RETRY_DELAY`] the way the Rust `vulkan_cube`
/// sample retries its senders, whatever brings news about it (the peer list, a peer update or a
/// render request). During the first [`STARTUP_RETRY_WINDOW`] the delay does not double: a
/// first create often fails while QAROS is still wiring the session up, and a fresh start
/// should not wait out a growing back-off. A target that did not answer (its creation timed
/// out, or it asked for no frames) waits [`LONGEST_RETRY_DELAY`] at once (the unanswered
/// retry period): an unchanged running listing is no news about it, but a render request from
/// it is. Only a creation that times out during the startup window is tried again after
/// [`FIRST_RETRY_DELAY`], as a background retry (see [`TargetPeers`]). A stream that starts,
/// or a target whose listed state changes to running or that is listed under another version
/// id, resets the delay.
#[derive(Debug)]
struct RetryPlan {
    started: Instant,
    next_attempts: BTreeMap<PeerId, RetryWait>,
    unanswered_delay: Duration,
}

#[derive(Clone, Copy, Debug)]
struct RetryWait {
    next_attempt: Instant,
    delay: Duration,
    unanswered: bool,
}

impl RetryPlan {
    fn new(started: Instant) -> Self {
        Self {
            started,
            next_attempts: BTreeMap::new(),
            unanswered_delay: LONGEST_RETRY_DELAY,
        }
    }

    fn is_starting_up(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) < STARTUP_RETRY_WINDOW
    }

    fn is_due(&self, peer: PeerId, now: Instant) -> bool {
        self.next_attempts
            .get(&peer)
            .is_none_or(|wait| now >= wait.next_attempt)
    }

    fn released(&mut self, peer: PeerId, cause: ReleaseCause, now: Instant) -> Duration {
        match cause {
            ReleaseCause::Unanswered => self.unanswered(peer, now),
            ReleaseCause::Failed => self.failed(peer, now),
        }
    }

    fn failed(&mut self, peer: PeerId, now: Instant) -> Duration {
        let starting_up = self.is_starting_up(now);
        let delay = match self.next_attempts.get(&peer) {
            Some(previous) if !starting_up => (previous.delay * 2).min(LONGEST_RETRY_DELAY),
            _ => FIRST_RETRY_DELAY,
        };
        self.wait(peer, now, delay, false)
    }

    fn unanswered(&mut self, peer: PeerId, now: Instant) -> Duration {
        self.wait(peer, now, self.unanswered_delay, true)
    }

    /// A creation that timed out while the sink starts up: QAROS may still be wiring the
    /// session up, so the target is tried
    /// again after [`FIRST_RETRY_DELAY`], or the unanswered retry period if that is shorter.
    fn unanswered_while_starting_up(&mut self, peer: PeerId, now: Instant) -> Duration {
        self.wait(
            peer,
            now,
            FIRST_RETRY_DELAY.min(self.unanswered_delay),
            true,
        )
    }

    fn wait(&mut self, peer: PeerId, now: Instant, delay: Duration, unanswered: bool) -> Duration {
        let wait = RetryWait {
            next_attempt: now + delay,
            delay,
            unanswered,
        };
        self.next_attempts.insert(peer, wait);
        delay
    }

    /// The peer showed it is alive (it asked for a stream): a wait for an answer it did not
    /// give is over; a back-off after a failure stays.
    fn heard_from(&mut self, peer: PeerId) {
        if self
            .next_attempts
            .get(&peer)
            .is_some_and(|wait| wait.unanswered)
        {
            self.next_attempts.remove(&peer);
        }
    }

    fn succeeded(&mut self, peer: PeerId) {
        self.next_attempts.remove(&peer);
    }
}

/// The first wait before a target the sink gave up on may join again.
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(2);

/// After the sink starts, failing targets are retried every [`FIRST_RETRY_DELAY`] this long.
const STARTUP_RETRY_WINDOW: Duration = Duration::from_secs(30);

/// The longest wait between attempts for a target whose sender keeps failing.
const LONGEST_RETRY_DELAY: Duration = Duration::from_mins(1);

/// A peer the application can render for: a target app (headset, Android streamer,
/// visualizer receiver) that is running (header `b1338df`, "Basic flow", step 5).
pub fn is_render_target(peer: &PeerInfo) -> bool {
    peer.is_target_app && peer.app_state == AppState::Running
}

/// A peer shutting down or retired (gone for good under its id) has left, even when it still
/// holds a stream it requested.
fn has_left(app_state: AppState) -> bool {
    matches!(app_state, AppState::ShuttingDown | AppState::Retired)
}

fn label_of(peer: &PeerInfo) -> String {
    if peer.display_name.is_empty() {
        peer.id.to_string()
    } else {
        peer.display_name.clone()
    }
}

/// Targets from the peer list (`rendering-streams.mdx`, "Finding rendering targets"): the
/// peer updates (subscribed first, in `connect`) and the peer list, read at the first poll,
/// after updates and every few seconds. A running target app joins; a peer that stops running, or is no
/// longer listed, leaves (a departed peer stays listed as shutting down for a while). A
/// target only asks for content once a sender has attached it to the app volume, so render
/// stream requests are not needed to find one; a request from an unlisted peer still joins it.
/// A listed target whose creation timed out, in this run or one of the last
/// [`UNANSWERED_MEMORY`](crate::unanswered::UNANSWERED_MEMORY), is held back
/// ([`UnansweredPeers`]): it joins from the peer list only as a background retry
/// ([`PeerEvent::Joined::held_back`]), at most once per unanswered retry period (60 s by
/// default), until it answers, asks for a stream, its listed state changes or it is listed under
/// another version id. A timeout in the first 30 s of the run gets its first background retry
/// after 2 s instead. A creation begun before its target was listed under a new version id
/// says nothing about the restarted app: its timeout neither holds the target back nor delays
/// its next creation.
pub struct TargetPeers {
    connection: Rc<QarosConnection>,
    last_listing: Option<Instant>,
    present: Vec<PeerId>,
    /// Targets the sink released, retried only once their back-off has passed.
    retries: RetryPlan,
    unanswered: UnansweredPeers,
    /// When each held-back target's last background retry began, in this run.
    held_back_retries: BTreeMap<PeerId, Instant>,
    unanswered_retry_period: Duration,
    /// Each peer's app state and target flag as last seen, so a change is logged once.
    last_seen: BTreeMap<PeerId, (AppState, bool)>,
    /// Each listed peer's version id as last seen.
    version_ids: BTreeMap<PeerId, String>,
    /// The version id each running creation was begun for, to tell a timeout of the app
    /// listed now from one of an app that has since restarted.
    creation_versions: BTreeMap<PeerId, String>,
    /// Held-back targets whose creation timed out during the startup window and whose first
    /// background retry is due after [`FIRST_RETRY_DELAY`].
    startup_retries: BTreeSet<PeerId>,
    /// Present peers that joined by their own render stream request. Such a requester can be
    /// missing from the peer list or listed as no target app, so the peer list does not make it
    /// leave. It leaves when it is listed as shutting down or retired, when its stream closes
    /// or its sender fails, or when it stalls.
    requesters: BTreeSet<PeerId>,
    /// Present peers with a started stream, with when a frame for them last began. A requester
    /// that asks for no frame in [`STALL_TIMEOUT`] is taken for gone and waits the longest
    /// back-off, since a vanished requester is never listed as leaving. A listed target that
    /// stalls is taken for gone only while a user waits for its slot: a receiver pauses its
    /// stream while its display is not visible (a headset taken off, a minimised visualizer)
    /// and keeps its sender for when it resumes, while a target listed as running that
    /// does not answer must not keep a live one out.
    last_frames: BTreeMap<PeerId, Instant>,
    stall_timeout: Duration,
    /// When the sink last checked for stalls, just before beginning frames; a longer gap (a
    /// heavy load, a blocked thread, viewer frames without a drawn 3D view) is not counted
    /// against the peers.
    last_stall_check: Option<Instant>,
}

/// How long a started stream may go without a begun frame before its peer is taken for gone.
/// Receivers ask for frames continuously.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(10);

/// A gap between stall checks longer than this (or half the stall timeout, if shorter) is the
/// viewer not asking for frames, and moves the stall clocks on by what exceeds it.
const VIEWER_PAUSE: Duration = Duration::from_secs(1);

impl TargetPeers {
    /// Remembers unanswered targets in the connection's
    /// [`unanswered_peers_file`](crate::ConnectOptions::unanswered_peers_file), if it names one.
    pub fn new(connection: Rc<QarosConnection>) -> Self {
        let unanswered = connection
            .unanswered_peers_file()
            .map_or_else(UnansweredPeers::in_memory, |file| {
                UnansweredPeers::load(file.to_path_buf(), SystemTime::now())
            });
        Self {
            connection,
            last_listing: None,
            present: Vec::new(),
            retries: RetryPlan::new(Instant::now()),
            unanswered,
            held_back_retries: BTreeMap::new(),
            unanswered_retry_period: LONGEST_RETRY_DELAY,
            last_seen: BTreeMap::new(),
            version_ids: BTreeMap::new(),
            creation_versions: BTreeMap::new(),
            startup_retries: BTreeSet::new(),
            requesters: BTreeSet::new(),
            last_frames: BTreeMap::new(),
            stall_timeout: STALL_TIMEOUT,
            last_stall_check: None,
        }
    }

    fn discount_viewer_pause(&mut self, now: Instant) {
        let pause = VIEWER_PAUSE.min(self.stall_timeout / 2);
        for last_frame in self.last_frames.values_mut() {
            let counted_from = self
                .last_stall_check
                .map_or(*last_frame, |last_check| last_check.max(*last_frame));
            let gap = now.saturating_duration_since(counted_from);
            // Only the excess: a viewer that keeps running slowly still counts `pause` of
            // every frame, so a vanished peer still goes.
            *last_frame += gap.saturating_sub(pause);
        }
        self.last_stall_check = Some(now);
    }

    #[must_use]
    pub fn with_stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = timeout;
        self
    }

    /// How long a target that did not answer (or stalled) waits before it is tried again, and
    /// how often a held-back target is retried in the background; 60 s by default.
    #[must_use]
    pub fn with_unanswered_retry_period(mut self, period: Duration) -> Self {
        self.unanswered_retry_period = period;
        self.retries.unanswered_delay = period;
        self
    }

    /// True when the peer joined now.
    fn join(
        &mut self,
        peer: PeerId,
        label: String,
        how: Joining,
        events: &mut Vec<PeerEvent>,
    ) -> bool {
        if self.present.contains(&peer) || !self.retries.is_due(peer, Instant::now()) {
            return false;
        }
        let found_by = match how {
            Joining::Requested => "its render stream request",
            Joining::Listed => "the peer list",
            Joining::HeldBack => {
                "the peer list, retried in the background as it did not answer before"
            }
        };
        log::info!("qaros: target {peer} ({label}) found by {found_by}");
        self.present.push(peer);
        events.push(PeerEvent::Joined {
            peer,
            label,
            requested_a_stream: how == Joining::Requested,
            held_back: how == Joining::HeldBack,
        });
        true
    }

    /// A held-back target's background retry is due: its last timeout (also from an earlier
    /// run) and its last retry in this run are both a retry period ago, or it timed out while
    /// the sink started up (the retry plan then holds it for [`FIRST_RETRY_DELAY`]).
    fn is_held_back_retry_due(&self, peer: PeerId, now: SystemTime) -> bool {
        if self.startup_retries.contains(&peer) {
            return true;
        }
        self.unanswered
            .is_retry_due(peer, now, self.unanswered_retry_period)
            && self
                .held_back_retries
                .get(&peer)
                .is_none_or(|last| last.elapsed() >= self.unanswered_retry_period)
    }

    fn leave(&mut self, peer: PeerId, events: &mut Vec<PeerEvent>) {
        self.requesters.remove(&peer);
        self.last_frames.remove(&peer);
        if let Some(index) = self.present.iter().position(|present| *present == peer) {
            self.present.remove(index);
            events.push(PeerEvent::Left { peer });
        }
    }

    fn keeps_its_requested_stream(&self, peer: &PeerInfo) -> bool {
        self.requesters.contains(&peer.id) && !has_left(peer.app_state)
    }

    fn note_progress(&mut self, peer: PeerId) {
        if let Some(last_frame) = self.last_frames.get_mut(&peer) {
            *last_frame = Instant::now();
        }
    }

    fn observe(&mut self, peer: &PeerInfo, events: &mut Vec<PeerEvent>) {
        let seen_as = (peer.app_state, peer.is_target_app);
        let previous_version_id = self.version_ids.insert(peer.id, peer.version_id.clone());
        if is_render_target(peer)
            && previous_version_id.is_some_and(|previous| previous != peer.version_id)
        {
            log::info!(
                "qaros: peer {} ({}) is listed under a new version id; it restarted",
                peer.id,
                label_of(peer)
            );
            self.retries.succeeded(peer.id);
        }
        let previously_seen_as = self.last_seen.insert(peer.id, seen_as);
        if previously_seen_as != Some(seen_as) {
            if is_render_target(peer) {
                // A receiver that (re)starts is worth a sender at once, back-off or not.
                self.retries.succeeded(peer.id);
                if previously_seen_as.is_some() {
                    self.unanswered
                        .forget(peer.id, "its listed state changed to running");
                }
            }
            log::info!(
                "qaros: peer {} ({}): {:?}, {}",
                peer.id,
                label_of(peer),
                peer.app_state,
                if peer.is_target_app {
                    "a target app"
                } else {
                    "not a target app"
                }
            );
        }
        if is_render_target(peer) {
            let now = SystemTime::now();
            if !self.unanswered.holds_back(peer.id, &peer.version_id, now) {
                self.join(peer.id, label_of(peer), Joining::Listed, events);
            } else if self.is_held_back_retry_due(peer.id, now)
                && self.join(peer.id, label_of(peer), Joining::HeldBack, events)
            {
                self.startup_retries.remove(&peer.id);
                self.held_back_retries.insert(peer.id, Instant::now());
            }
        } else if !self.keeps_its_requested_stream(peer) {
            self.leave(peer.id, events);
        }
    }

    fn listed_peers(&self) -> Option<Vec<PeerInfo>> {
        match self.connection.session().peers() {
            Ok(peers) => Some(peers),
            Err(error) => {
                log::warn!("qaros: cannot list peers: {error}");
                None
            }
        }
    }

    /// Reads the peer list and follows it: listed running targets join, other listed peers and
    /// present peers no longer listed leave.
    fn follow_peer_list(&mut self, events: &mut Vec<PeerEvent>) {
        self.last_listing = Some(Instant::now());
        let Some(listed) = self.listed_peers() else {
            return;
        };
        for peer in &listed {
            self.observe(peer, events);
        }
        let unlisted: Vec<PeerId> = self
            .present
            .iter()
            .filter(|present| !listed.iter().any(|info| info.id == **present))
            .filter(|present| !self.requesters.contains(present))
            .copied()
            .collect();
        for peer in unlisted {
            self.leave(peer, events);
        }
    }

    /// At the first poll, after any update (updates cannot tell join, leave and change apart,
    /// `peers.mdx` L98-106), and every [`PEER_LIST_INTERVAL`] in case an update never comes.
    fn is_listing_due(&self, updates_arrived: bool) -> bool {
        updates_arrived
            || self
                .last_listing
                .is_none_or(|last| last.elapsed() >= PEER_LIST_INTERVAL)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Joining {
    Requested,
    Listed,
    HeldBack,
}

/// How often the peer list is read without an update.
const PEER_LIST_INTERVAL: Duration = Duration::from_secs(2);

impl UserEventSource for TargetPeers {
    fn poll_events(&mut self) -> Vec<PeerEvent> {
        let mut events = Vec::new();
        // The peers already there come first, then whoever the updates bring.
        if self.last_listing.is_none() {
            self.follow_peer_list(&mut events);
        }
        let updates = self.connection.take_peer_updates();
        for update in &updates {
            self.observe(update, &mut events);
        }
        if self.is_listing_due(!updates.is_empty()) {
            self.follow_peer_list(&mut events);
        }
        for request in self.connection.take_render_requests() {
            let peer = request.target_peer;
            self.retries.heard_from(peer);
            self.unanswered.forget(peer, "it asked for a stream");
            if self.present.contains(&peer) {
                // Not added to the requesters: a listed target asks for content once a sender
                // attached it, and the requesters' stall rule would release a paused headset.
                events.push(PeerEvent::AskedForAStream { peer });
            } else if self.join(peer, peer.to_string(), Joining::Requested, &mut events) {
                self.requesters.insert(peer);
            }
        }
        events
    }

    fn forget(&mut self, peer: PeerId, cause: ReleaseCause) {
        self.present.retain(|present| *present != peer);
        self.requesters.remove(&peer);
        self.last_frames.remove(&peer);
        let creation_version = self.creation_versions.remove(&peer);
        let listed_version = self.version_ids.get(&peer).cloned();
        let now = Instant::now();
        if cause != ReleaseCause::Unanswered {
            let delay = self.retries.released(peer, cause, now);
            log::info!("qaros: target {peer} may join again in {delay:?}");
            return;
        }
        if creation_version.is_some() && creation_version != listed_version {
            log::info!(
                "qaros: target {peer} did not answer a creation begun before it restarted; it may \
                 join again at once"
            );
            self.retries.succeeded(peer);
            return;
        }
        let delay = if self.retries.is_starting_up(now) {
            self.startup_retries.insert(peer);
            self.retries.unanswered_while_starting_up(peer, now)
        } else {
            self.startup_retries.remove(&peer);
            self.retries.released(peer, cause, now)
        };
        log::info!("qaros: target {peer} may join again in {delay:?}");
        let version_id = creation_version.or(listed_version).unwrap_or_default();
        self.unanswered
            .remember(peer, &version_id, SystemTime::now());
    }

    fn leave_stalled(&mut self, slot_wanted: bool) -> Vec<PeerEvent> {
        let now = Instant::now();
        self.discount_viewer_pause(now);
        let timeout = self.stall_timeout;
        let stalled: Vec<PeerId> = self
            .last_frames
            .iter()
            .filter(|(_, last_frame)| now.saturating_duration_since(**last_frame) >= timeout)
            .map(|(peer, _)| *peer)
            .filter(|peer| slot_wanted || self.requesters.contains(peer))
            .collect();
        let mut events = Vec::new();
        for peer in stalled {
            let delay = self.retries.unanswered(peer, now);
            log::info!(
                "qaros: target {peer} asked for no frame in {timeout:?}; taken for gone, may join \
                 again in {delay:?}"
            );
            self.leave(peer, &mut events);
        }
        events
    }

    fn answered(&mut self, peer: PeerId) {
        self.creation_versions.remove(&peer);
        self.startup_retries.remove(&peer);
        self.held_back_retries.remove(&peer);
        self.unanswered.forget(peer, "its sender was created");
    }

    fn stream_started(&mut self, peer: PeerId) {
        self.retries.succeeded(peer);
        if self.present.contains(&peer) {
            self.last_frames.insert(peer, Instant::now());
        }
    }

    fn frame_begun(&mut self, peer: PeerId) {
        self.note_progress(peer);
    }

    fn creation_started(&mut self, peer: PeerId) {
        if let Some(version_id) = self.version_ids.get(&peer) {
            self.creation_versions.insert(peer, version_id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use frame_sink::MaxUsers;

    use super::*;

    const HEADSET: PeerId = PeerId([0xa1; 16]);
    const OTHER_HEADSET: PeerId = PeerId([0xb2; 16]);

    /// Hands out scripted batches of events, one batch per poll.
    struct ScriptedPeers {
        batches: Rc<RefCell<Vec<Vec<PeerEvent>>>>,
        forgotten: Rc<RefCell<Vec<(PeerId, ReleaseCause)>>>,
    }

    impl UserEventSource for ScriptedPeers {
        fn poll_events(&mut self) -> Vec<PeerEvent> {
            let mut batches = self.batches.borrow_mut();
            if batches.is_empty() {
                Vec::new()
            } else {
                batches.remove(0)
            }
        }

        fn forget(&mut self, peer: PeerId, cause: ReleaseCause) {
            self.forgotten.borrow_mut().push((peer, cause));
        }
    }

    fn joined(peer: PeerId) -> PeerEvent {
        PeerEvent::Joined {
            peer,
            label: peer.to_string(),
            requested_a_stream: false,
            held_back: false,
        }
    }

    fn requested(peer: PeerId) -> PeerEvent {
        PeerEvent::Joined {
            peer,
            label: peer.to_string(),
            requested_a_stream: true,
            held_back: false,
        }
    }

    fn held_back(peer: PeerId) -> PeerEvent {
        PeerEvent::Joined {
            peer,
            label: peer.to_string(),
            requested_a_stream: false,
            held_back: true,
        }
    }

    fn listed_receiver(index: u8) -> PeerId {
        PeerId([index; 16])
    }

    type Forgotten = Rc<RefCell<Vec<(PeerId, ReleaseCause)>>>;

    fn peer_users(batches: Vec<Vec<PeerEvent>>) -> (PeerUsers, Forgotten) {
        peer_users_with_slots(batches, 4)
    }

    fn peer_users_with_slots(batches: Vec<Vec<PeerEvent>>, slots: usize) -> (PeerUsers, Forgotten) {
        let forgotten = Rc::new(RefCell::new(Vec::new()));
        let source = ScriptedPeers {
            batches: Rc::new(RefCell::new(batches)),
            forgotten: Rc::clone(&forgotten),
        };
        let users = PeerUsers::new(Box::new(source), Box::new(MaxUsers { maximum: slots }));
        (users, forgotten)
    }

    fn started_creations(users: &mut PeerUsers) -> Vec<UserId> {
        users
            .creations_to_start()
            .into_iter()
            .map(|(user, _)| user.id)
            .collect()
    }

    /// Polls, starts the creations and lets every one succeed: the users admitted.
    fn poll_and_admit(users: &mut PeerUsers) -> Vec<UserId> {
        users.poll();
        started_creations(users)
            .into_iter()
            .filter(|user| matches!(users.creation_succeeded(*user), Admission::Admitted(_)))
            .collect()
    }

    #[test]
    fn a_peer_that_leaves_and_joins_in_one_poll_comes_back_as_a_new_user() {
        let (mut users, _) = peer_users(vec![
            vec![joined(HEADSET), joined(OTHER_HEADSET)],
            vec![PeerEvent::Left { peer: HEADSET }, joined(HEADSET)],
        ]);
        assert_eq!(poll_and_admit(&mut users), [UserId(1), UserId(2)]);

        assert_eq!(users.poll(), [UserId(1)], "its sender goes");
        assert_eq!(
            users.waiting(),
            [UserInfo {
                id: UserId(3),
                label: HEADSET.to_string(),
            }],
            "a fresh user id, so no frame key of the old sender matches the new one's"
        );
        assert_eq!(users.peer_of(UserId(3)), Some(HEADSET));
        assert_eq!(users.peer_of(UserId(1)), None);
        assert_eq!(users.peer_of(UserId(2)), Some(OTHER_HEADSET));
    }

    #[test]
    fn a_user_the_sink_releases_comes_back_as_a_new_user() {
        let (mut users, forgotten) = peer_users(vec![vec![joined(HEADSET)], vec![joined(HEADSET)]]);
        assert_eq!(poll_and_admit(&mut users), [UserId(1)]);
        users.release(UserId(1), ReleaseCause::Failed);
        assert!(users.rendered().is_empty());
        assert_eq!(*forgotten.borrow(), [(HEADSET, ReleaseCause::Failed)]);
        assert_eq!(poll_and_admit(&mut users), [UserId(2)]);
    }

    #[test]
    fn every_listed_target_is_tried_at_once_and_the_first_created_take_the_slots() {
        let listed: Vec<PeerEvent> = (1..=4)
            .map(|index| joined(listed_receiver(index)))
            .collect();
        let (mut users, forgotten) = peer_users_with_slots(vec![listed], 2);
        users.poll();
        assert_eq!(
            started_creations(&mut users),
            [UserId(1), UserId(2), UserId(3), UserId(4)],
            "dead targets listed first do not hold the live ones back"
        );
        assert!(started_creations(&mut users).is_empty(), "each once");

        assert!(matches!(
            users.creation_succeeded(UserId(4)),
            Admission::Admitted(_)
        ));
        assert!(matches!(
            users.creation_succeeded(UserId(2)),
            Admission::Admitted(_)
        ));
        assert_eq!(users.creation_succeeded(UserId(3)), Admission::NoFreeSlot);
        let rendered: Vec<UserId> = users.rendered().iter().map(|user| user.id).collect();
        assert_eq!(rendered, [UserId(4), UserId(2)]);
        assert!(
            started_creations(&mut users).is_empty(),
            "no creation while every slot is taken"
        );

        users.release(UserId(1), ReleaseCause::Unanswered);
        assert_eq!(
            *forgotten.borrow(),
            [(listed_receiver(1), ReleaseCause::Unanswered)]
        );
        users.release(UserId(4), ReleaseCause::Failed);
        assert_eq!(
            started_creations(&mut users),
            [UserId(3)],
            "a freed slot starts the waiting candidate's sender again"
        );
    }

    #[test]
    fn a_candidate_whose_creation_still_runs_wants_no_slot_from_a_stalled_user_yet() {
        let (mut users, _) =
            peer_users_with_slots(vec![vec![joined(HEADSET), joined(OTHER_HEADSET)]], 1);
        users.poll();
        assert_eq!(started_creations(&mut users).len(), 2);
        assert!(matches!(
            users.creation_succeeded(UserId(1)),
            Admission::Admitted(_)
        ));
        assert!(!users.is_slot_wanted(), "the other may never answer");
        assert_eq!(users.creation_succeeded(UserId(2)), Admission::NoFreeSlot);
        assert!(users.is_slot_wanted(), "it answered and waits");
    }

    #[test]
    fn listed_targets_are_tried_up_to_the_limit_and_requesters_beyond_it() {
        let mut first_batch: Vec<PeerEvent> = (1..=10)
            .map(|index| joined(listed_receiver(index)))
            .collect();
        first_batch.push(requested(HEADSET));
        let (mut users, _) = peer_users_with_slots(vec![first_batch], 2);
        users.poll();
        assert_eq!(
            users.waiting()[0].label,
            HEADSET.to_string(),
            "a requester goes first"
        );
        let started = started_creations(&mut users);
        assert_eq!(started.len(), HEDGED_CREATION_LIMIT + 1);
        assert_eq!(
            started[0],
            UserId(11),
            "the requester's sender is tried first"
        );

        users.release(UserId(1), ReleaseCause::Unanswered);
        assert_eq!(
            started_creations(&mut users),
            [UserId(9)],
            "the next listed target once a creation ends"
        );
    }

    #[test]
    fn a_held_back_target_is_tried_last_and_outside_the_limit() {
        let mut first_batch = vec![held_back(HEADSET)];
        first_batch.extend((1..=9).map(|index| joined(listed_receiver(index))));
        let (mut users, _) = peer_users_with_slots(vec![first_batch], 10);
        users.poll();
        assert_eq!(
            users.waiting().last().map(|user| user.id),
            Some(UserId(1)),
            "the held-back target goes behind the listed ones that joined after it"
        );
        let started = started_creations(&mut users);
        let mut expected: Vec<UserId> = (2..=9).map(UserId).collect();
        expected.push(UserId(1));
        assert_eq!(
            started, expected,
            "the limit stops the ninth listed target, not the background retry"
        );
    }

    #[test]
    fn a_held_back_target_never_takes_a_slot_ahead_of_another_candidate() {
        let (mut users, _) = peer_users_with_slots(
            vec![
                vec![joined(OTHER_HEADSET), held_back(HEADSET)],
                vec![joined(listed_receiver(3))],
            ],
            1,
        );
        users.poll();
        assert_eq!(
            started_creations(&mut users),
            [UserId(1)],
            "no slot is left over for the held-back target"
        );
        assert!(!users.is_slot_wanted(), "a held-back target wants no slot");

        users.release(UserId(1), ReleaseCause::Unanswered);
        assert_eq!(started_creations(&mut users), [UserId(2)]);
        users.poll();
        assert_eq!(started_creations(&mut users), [UserId(3)]);
        assert_eq!(
            users.creation_succeeded(UserId(2)),
            Admission::NoFreeSlot,
            "answered first, but the listed target is ahead of it"
        );
        assert!(matches!(
            users.creation_succeeded(UserId(3)),
            Admission::Admitted(_)
        ));
    }

    #[test]
    fn a_held_back_target_that_answers_with_a_slot_free_is_admitted() {
        let (mut users, _) = peer_users_with_slots(vec![vec![held_back(HEADSET)]], 1);
        users.poll();
        assert_eq!(started_creations(&mut users), [UserId(1)]);
        assert!(matches!(
            users.creation_succeeded(UserId(1)),
            Admission::Admitted(_)
        ));
    }

    #[test]
    fn a_rejoined_peer_waits_for_its_departed_users_creation_to_resolve() {
        let (mut users, _) = peer_users(vec![
            vec![joined(HEADSET)],
            vec![PeerEvent::Left { peer: HEADSET }, joined(HEADSET)],
        ]);
        users.poll();
        assert_eq!(started_creations(&mut users), [UserId(1)]);
        users.poll();
        assert!(
            started_creations(&mut users).is_empty(),
            "no second creation for the target while the first runs"
        );
        assert_eq!(users.creation_succeeded(UserId(1)), Admission::Left);
        assert_eq!(started_creations(&mut users), [UserId(2)]);
    }

    #[test]
    fn creations_of_departed_users_count_against_the_limit_until_they_resolve() {
        let first_batch: Vec<PeerEvent> = (1..=8)
            .map(|index| joined(listed_receiver(index)))
            .collect();
        let mut second_batch: Vec<PeerEvent> = (1..=8)
            .map(|index| PeerEvent::Left {
                peer: listed_receiver(index),
            })
            .collect();
        second_batch.push(joined(listed_receiver(9)));
        let (mut users, _) = peer_users(vec![first_batch, second_batch]);
        users.poll();
        assert_eq!(started_creations(&mut users).len(), HEDGED_CREATION_LIMIT);
        users.poll();
        assert!(
            started_creations(&mut users).is_empty(),
            "the departed users' creations still run"
        );
        users.release(UserId(1), ReleaseCause::Unanswered);
        assert_eq!(started_creations(&mut users), [UserId(9)]);
    }

    #[test]
    fn senders_created_together_are_ranked_requesters_first() {
        let (mut users, _) =
            peer_users(vec![vec![joined(OTHER_HEADSET)], vec![requested(HEADSET)]]);
        users.poll();
        users.poll();
        assert!(users.candidate_rank(UserId(2)) < users.candidate_rank(UserId(1)));
        assert_eq!(users.candidate_rank(UserId(9)), usize::MAX);
    }

    #[test]
    fn a_sender_created_after_its_peer_left_goes_and_a_failed_one_frees_nothing() {
        let (mut users, forgotten) = peer_users(vec![
            vec![joined(HEADSET), joined(OTHER_HEADSET)],
            vec![PeerEvent::Left { peer: HEADSET }],
        ]);
        users.poll();
        assert_eq!(started_creations(&mut users).len(), 2);
        assert!(users.poll().is_empty(), "nobody rendered left");
        assert_eq!(users.creation_succeeded(UserId(1)), Admission::Left);
        users.release(UserId(2), ReleaseCause::Unanswered);
        assert_eq!(
            *forgotten.borrow(),
            [(OTHER_HEADSET, ReleaseCause::Unanswered)]
        );
        assert!(users.rendered().is_empty() && users.waiting().is_empty());
    }

    /// Fails the user's frames at the given offsets from `start`; the offsets at which the
    /// user was given up.
    fn give_up_offsets(
        users: &mut PeerUsers,
        start: Instant,
        offsets_in_milliseconds: impl IntoIterator<Item = u64>,
    ) -> Vec<u64> {
        offsets_in_milliseconds
            .into_iter()
            .filter(|offset| {
                let now = start + Duration::from_millis(*offset);
                users.frame_failed_at(UserId(1), &"injected", now)
            })
            .collect()
    }

    #[test]
    fn a_user_whose_frames_keep_failing_for_a_second_is_given_up_unless_a_frame_shows() {
        let (mut users, _) = peer_users(vec![vec![joined(HEADSET)]]);
        assert_eq!(poll_and_admit(&mut users), [UserId(1)]);
        assert_eq!(FRAME_FAILURE_PERIOD, Duration::from_secs(1));
        let start = Instant::now();
        let every_two_milliseconds = (0..1_000).step_by(2);
        assert!(
            give_up_offsets(&mut users, start, every_two_milliseconds).is_empty(),
            "hundreds of failures within the period at a high frame rate"
        );
        assert_eq!(give_up_offsets(&mut users, start, [1_000]), [1_000]);

        users.frame_shown(UserId(1));
        let restart = start + Duration::from_secs(5);
        assert_eq!(
            give_up_offsets(&mut users, restart, [0, 2_000, 4_000]),
            Vec::<u64>::new(),
            "three failures spread over a pause are too few"
        );
        let more = (4_100..=4_700).step_by(100);
        assert_eq!(
            give_up_offsets(&mut users, restart, more),
            [4_700],
            "the tenth failure in a row, more than a second after the first"
        );
    }

    /// The background retry of a held-back target runs when a listed target joins and the
    /// held-back one asks for a stream: it answers, and takes the only slot as a requester
    /// instead of going behind the listed target and losing its fresh sender.
    #[test]
    fn a_held_back_candidate_that_asks_for_a_stream_goes_to_the_requesters_with_its_creation() {
        let (mut users, _) = peer_users_with_slots(
            vec![
                vec![held_back(HEADSET)],
                vec![
                    joined(OTHER_HEADSET),
                    PeerEvent::AskedForAStream { peer: HEADSET },
                ],
            ],
            1,
        );
        users.poll();
        assert_eq!(
            started_creations(&mut users),
            [UserId(1)],
            "the background retry"
        );
        users.poll();
        assert_eq!(
            users
                .waiting()
                .iter()
                .map(|user| user.id)
                .collect::<Vec<_>>(),
            [UserId(1), UserId(2)],
            "asking for a stream puts it first"
        );
        assert_eq!(
            started_creations(&mut users),
            [UserId(2)],
            "its running creation is kept, the listed target's starts"
        );
        assert!(matches!(
            users.creation_succeeded(UserId(1)),
            Admission::Admitted(_)
        ));
        assert_eq!(users.creation_succeeded(UserId(2)), Admission::NoFreeSlot);
    }

    #[test]
    fn a_rendered_user_or_an_absent_peer_asking_for_a_stream_changes_nothing() {
        let (mut users, _) = peer_users(vec![
            vec![joined(HEADSET)],
            vec![
                PeerEvent::AskedForAStream { peer: HEADSET },
                PeerEvent::AskedForAStream {
                    peer: OTHER_HEADSET,
                },
            ],
        ]);
        assert_eq!(poll_and_admit(&mut users), [UserId(1)]);
        assert!(users.poll().is_empty());
        assert!(users.waiting().is_empty());
        assert_eq!(
            users.rendered(),
            [UserInfo {
                id: UserId(1),
                label: HEADSET.to_string(),
            }]
        );
    }

    #[test]
    fn a_target_that_did_not_answer_waits_the_longest_delay_until_it_asks_for_a_stream() {
        let start = Instant::now();
        let mut retries = RetryPlan::new(start);
        assert_eq!(
            retries.released(HEADSET, ReleaseCause::Unanswered, start),
            LONGEST_RETRY_DELAY,
            "no flat startup delay for a target that did not answer"
        );
        assert!(!retries.is_due(HEADSET, start + Duration::from_secs(59)));
        assert!(retries.is_due(HEADSET, start + LONGEST_RETRY_DELAY));

        retries.heard_from(HEADSET);
        assert!(
            retries.is_due(HEADSET, start),
            "its own render request lifts the wait"
        );

        assert_eq!(
            retries.released(HEADSET, ReleaseCause::Failed, start),
            FIRST_RETRY_DELAY
        );
        retries.heard_from(HEADSET);
        assert!(
            !retries.is_due(HEADSET, start),
            "a back-off after a failure holds against requests"
        );
    }

    #[test]
    fn a_failing_target_is_retried_every_two_seconds_while_starting_up() {
        let start = Instant::now();
        let mut retries = RetryPlan::new(start);
        assert!(retries.is_due(HEADSET, start), "never failed: due at once");

        let delays: Vec<Duration> = [0, 12, 24, 29]
            .map(|seconds| retries.failed(HEADSET, start + Duration::from_secs(seconds)))
            .to_vec();
        assert_eq!(delays, [2, 2, 2, 2].map(Duration::from_secs));
        assert!(!retries.is_due(HEADSET, start + Duration::from_secs(30)));
        assert!(retries.is_due(HEADSET, start + Duration::from_secs(31)));
    }

    #[test]
    fn after_startup_a_failing_target_is_retried_after_a_doubling_delay_until_it_streams() {
        let start = Instant::now();
        let mut retries = RetryPlan::new(start);
        let later = start + STARTUP_RETRY_WINDOW;

        let delays: Vec<Duration> = (0..7).map(|_| retries.failed(HEADSET, later)).collect();
        assert_eq!(
            delays,
            [2, 4, 8, 16, 32, 60, 60].map(Duration::from_secs),
            "doubles from 2 s up to 60 s"
        );
        assert!(!retries.is_due(HEADSET, later + Duration::from_secs(59)));
        assert!(retries.is_due(HEADSET, later + Duration::from_mins(1)));
        assert!(
            retries.is_due(OTHER_HEADSET, later),
            "other targets are not held back"
        );

        retries.succeeded(HEADSET);
        assert!(retries.is_due(HEADSET, later));
        assert_eq!(
            retries.failed(HEADSET, later),
            FIRST_RETRY_DELAY,
            "a stream that started resets the delay"
        );
    }

    #[test]
    fn a_peer_that_joins_twice_while_present_keeps_its_user() {
        let (mut users, _) = peer_users(vec![vec![joined(HEADSET)], vec![joined(HEADSET)]]);
        assert_eq!(poll_and_admit(&mut users), [UserId(1)]);
        assert!(users.poll().is_empty());
        assert!(users.waiting().is_empty(), "not a candidate again");
        assert_eq!(users.peer_of(UserId(1)), Some(HEADSET));
    }
}
