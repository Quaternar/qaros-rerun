use std::collections::BTreeMap;
use std::rc::Rc;

use frame_sink::{
    DepthEncoding, DepthRange, EYE_COUNT, EyeImage, FrameId, FrameKey, FrameRequest, FrameSink,
    MaxUsers, RenderPolicy, ShowInfo, SinkError, UserId, UserInfo, ViewRequest,
};
use qaros::{
    Eye, FrameLayout, FrameViewSpec, OwnedFrame, PeerId, PixelFormat, RenderSender,
    RenderSenderConfig, ShowParams, StatusCode, VideoFrameLayout, ViewKind,
};
use xr_math::{Fov, Pose};

use crate::begin_drain::{BEGIN_WAIT_DRAIN_LIMIT, drain_begin_waits};
use crate::connection::{QarosConnection, from_qaros_pose};
use crate::pending::{FinishedCreation, PendingSenders};
use crate::pixels::{ViewRegion, find_view, write_eye};
use crate::users::{Admission, PeerUsers, ReleaseCause, TargetPeers, UserEventSource};

/// Which space `qar_render_frame_info_get_view_pose` delivers eye poses in, while the stream
/// is bound to an app volume. The header at `b1338df` (L145-158, ADR-0253) says app content
/// space: view poses, hands and the rendered pose are in the volume's app space, in app units
/// (app metres: room metres divided by the app scale), and so are the near and far planes
/// handed back with each frame (`QarNearFar`, `QarRenderFrameShow::rendered_near_far`).
/// `Room` stays as a switch for a runtime that delivers room poses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PoseSpace {
    /// Room space: the poses are used as they come and the renderer applies the volume pose,
    /// app pose and app scale itself; near and far go back in room metres.
    Room,
    /// App content space: the sink maps each pose into the room with the volume's placement,
    /// so the renderer works in room metres, and hands near and far back in app units.
    #[default]
    App,
}

/// Maps stream view poses into room space according to [`PoseSpace`], with the volume's
/// placement (volume pose times app pose, and app scale).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PoseMapping {
    pose_space: PoseSpace,
    room_from_app: Pose,
    app_scale: f32,
}

impl PoseMapping {
    pub(crate) fn new(volume: &qaros::AppVolumeConfig, pose_space: PoseSpace) -> Self {
        Self {
            pose_space,
            room_from_app: from_qaros_pose(volume.pose) * from_qaros_pose(volume.app_pose),
            app_scale: volume.app_scale,
        }
    }

    pub(crate) fn room_from_eye(&self, pose: qaros::Pose) -> Pose {
        let eye = from_qaros_pose(pose);
        match self.pose_space {
            PoseSpace::Room => eye,
            PoseSpace::App => {
                self.room_from_app * Pose::new(eye.orientation, eye.position * self.app_scale)
            }
        }
    }

    /// `QarRenderFrameShow` for a frame rendered with `show` (room metres): near and far in
    /// the units of the poses (app units in app space). Linear depth is written in room
    /// metres, so its `depth_scale` converts to those units too; normalized depth needs none.
    pub(crate) fn show_params(&self, show: ShowInfo, depth_is_linear: bool) -> ShowParams {
        let room_to_pose_units = match self.pose_space {
            PoseSpace::Room => 1.0,
            PoseSpace::App => 1.0 / self.app_scale,
        };
        ShowParams {
            near: show.near_far.near_plane * room_to_pose_units,
            far: show.near_far.far_plane * room_to_pose_units,
            depth_scale: if depth_is_linear {
                show.depth_scale * room_to_pose_units
            } else {
                show.depth_scale
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct QarosSinkOptions {
    pub depth_range: DepthRange,
    pub pose_space: PoseSpace,
    /// Color format requested at sender creation; frames are written in whatever format the
    /// negotiated layout reports.
    pub color_format: PixelFormat,
    /// Users rendered at the same time with the default policy.
    pub max_users: usize,
    /// Stop streaming after this many shown frames across all users; `None` streams until
    /// the viewer closes.
    pub stop_after_frames: Option<u64>,
}

/// Views requested from each render sender: both eyes' color side by side in texture 0, both
/// eyes' depth side by side in texture 1. View indices 0 and 1 (the two color views) are the
/// eye indices used for poses and fields of view.
const STREAM_VIEWS: [FrameViewSpec; 4] = [
    FrameViewSpec {
        kind: ViewKind::Color,
        eye: Eye::Left,
        texture_index: 0,
    },
    FrameViewSpec {
        kind: ViewKind::Color,
        eye: Eye::Right,
        texture_index: 0,
    },
    FrameViewSpec {
        kind: ViewKind::Depth,
        eye: Eye::Left,
        texture_index: 1,
    },
    FrameViewSpec {
        kind: ViewKind::Depth,
        eye: Eye::Right,
        texture_index: 1,
    },
];

const EYES: [Eye; EYE_COUNT] = [Eye::Left, Eye::Right];

struct OpenFrame {
    frame_id: FrameId,
    frame: OwnedFrame,
    submitted: [bool; EYE_COUNT],
}

/// Where a user's current QAROS frame stands. The frame ends as shown or dropped (released
/// unshown) before the next one is begun, so every frame is shown with the poses its own
/// begin issued. Frames are begun by polling `qar_render_sender_try_begin_frame` once per
/// viewer frame (ADR-0039): the UI thread never waits, and a sender that goes needs nothing
/// from the library for a begin that was not ready.
enum FrameState {
    Idle,
    Open(OpenFrame),
}

/// One user's render stream: its own sender, frame counter and frame lifecycle.
struct UserStream {
    peer: PeerId,
    sender: RenderSender,
    resolution_in_pixel: [u32; 2],
    /// Frames the sender delivered so far; the next delivered frame gets this id.
    frames_delivered: FrameId,
    state: FrameState,
    /// The library's begin-frame wait runs: the last poll found no frame ready.
    begin_waiting: bool,
}

/// A [`FrameSink`] over QAROS CPU render senders, one per rendered user.
///
/// Each viewer frame, `begin_frames` (or `update_users` when no frames are begun) takes user
/// joins and leaves from the [`UserEventSource`], starts senders for the users without one
/// (all at once, as `PeerUsers` describes), lets the [`RenderPolicy`] decide which users whose
/// senders arrived are rendered, and drops the sender of each released user; `begin_frames`
/// then releases stalled users and begins (or polls) the next frame of every user without an
/// open frame.
///
/// Fields drop in declaration order: the streams go before the connection, so when this sink
/// holds the last reference the app volume is closed after its senders.
pub struct QarosSink {
    options: QarosSinkOptions,
    pose_mapping: PoseMapping,
    users: PeerUsers,
    pending: PendingSenders,
    streams: BTreeMap<UserId, UserStream>,
    /// Senders of released users whose begin-frame wait still ran (a paused receiver, say),
    /// polled every viewer frame and destroyed once the wait has ended (see `begin_drain`).
    draining: Vec<(UserId, RenderSender)>,
    shown_frame_count: u64,
    connection: Rc<QarosConnection>,
}

impl QarosSink {
    /// Users are the running target apps in the peer list (`TargetPeers`), rendered up to
    /// `options.max_users`.
    pub fn new(connection: Rc<QarosConnection>, options: QarosSinkOptions) -> Self {
        let events = Box::new(TargetPeers::new(Rc::clone(&connection)));
        let policy = Box::new(MaxUsers {
            maximum: options.max_users,
        });
        Self::with_user_events(connection, options, events, policy)
    }

    pub fn with_user_events(
        connection: Rc<QarosConnection>,
        options: QarosSinkOptions,
        events: Box<dyn UserEventSource>,
        policy: Box<dyn RenderPolicy>,
    ) -> Self {
        let pose_mapping = PoseMapping::new(connection.volume_config(), options.pose_space);
        Self {
            options,
            pose_mapping,
            users: PeerUsers::new(events, policy),
            pending: PendingSenders::default(),
            streams: BTreeMap::new(),
            draining: Vec::new(),
            shown_frame_count: 0,
            connection,
        }
    }

    pub fn shown_frame_count(&self) -> u64 {
        self.shown_frame_count
    }

    /// The users rendered for right now, and the peer each one streams to.
    pub fn rendered_users(&self) -> Vec<(UserInfo, PeerId)> {
        self.users
            .rendered()
            .iter()
            .filter_map(|user| {
                let stream = self.streams.get(&user.id)?;
                Some((user.clone(), stream.peer))
            })
            .collect()
    }

    /// Users without a slot, in the order their senders are tried.
    pub fn waiting_users(&self) -> Vec<UserInfo> {
        self.users.waiting()
    }

    /// True while a render sender is still being created.
    pub fn is_creating_senders(&self) -> bool {
        !self.pending.is_empty()
    }

    /// How long a creation that has not answered runs before it is given up and its user
    /// released, by default 30 s.
    pub fn set_sender_creation_deadline(&mut self, deadline: std::time::Duration) {
        self.pending.set_deadline(deadline);
    }

    fn apply_user_events_and_creations(&mut self) {
        self.connection.session().release_abandoned_creations();
        self.destroy_senders_whose_waits_ended();
        let left = self.users.poll();
        self.drop_streams(left);
        for (user, peer) in self.users.creations_to_start() {
            self.start_stream(user, peer);
        }
        let mut finished = self.pending.take_finished();
        finished.sort_by_key(|creation| self.users.candidate_rank(creation.user.id));
        for creation in finished {
            self.finish_stream(creation);
        }
    }

    fn drop_streams(&mut self, users: Vec<UserId>) {
        for user in users {
            self.drop_stream(user);
        }
    }

    /// A sender whose begin-frame wait still runs is parked in `draining` instead of destroyed.
    fn drop_stream(&mut self, user: UserId) {
        let Some(stream) = self.streams.remove(&user) else {
            return;
        };
        if stream.begin_waiting {
            log::info!(
                "qaros: released {user} (peer {}); its sender goes once its begin-frame wait ends",
                stream.peer
            );
            self.draining.push((user, stream.sender));
        } else {
            log::info!("qaros: released {user} (peer {})", stream.peer);
        }
    }

    /// The frame an ended wait yields is dropped unshown, before its sender.
    fn destroy_senders_whose_waits_ended(&mut self) {
        self.draining
            .retain_mut(|(user, sender)| match sender.try_begin_owned_frame() {
                Ok(None) => true,
                Ok(Some(_unshown)) => {
                    log::info!(
                        "qaros: the begin-frame wait of released {user} ended; its sender goes"
                    );
                    false
                }
                Err(error) => {
                    log::info!(
                        "qaros: the begin-frame wait of released {user} ended in {error}; its \
                         sender goes"
                    );
                    false
                }
            });
    }

    fn start_stream(&mut self, user: UserInfo, peer: PeerId) {
        let config = RenderSenderConfig {
            views: STREAM_VIEWS.to_vec(),
            app_volume: Some(self.connection.volume()),
            color_format: self.options.color_format,
            depth_format: PixelFormat::D32Float,
            layout: FrameLayout::SideBySide,
            ..RenderSenderConfig::new(peer)
        };
        match self.connection.start_render_sender(&config) {
            Ok(creation) => self.pending.insert(user, peer, creation),
            Err(error) => {
                log::warn!(
                    "qaros: cannot create a render stream for {}: {error}",
                    user.id
                );
                self.users.release(user.id, ReleaseCause::Failed);
            }
        }
    }

    fn finish_stream(&mut self, finished: FinishedCreation) {
        let FinishedCreation { user, peer, sender } = finished;
        let stream = sender
            .map_err(|failure| (failure.message, failure.cause))
            .and_then(|sender| {
                Self::user_stream(peer, sender)
                    .map_err(|error| (error.to_string(), ReleaseCause::Failed))
            });
        let stream = match stream {
            Ok(stream) => stream,
            Err((message, cause)) => {
                log::warn!(
                    "qaros: cannot create a render stream for {}: {message}",
                    user.id
                );
                self.users.release(user.id, cause);
                return;
            }
        };
        match self.users.creation_succeeded(user.id) {
            Admission::Admitted(user) => {
                log::info!(
                    "qaros: rendering for {} ({}, peer {peer}) at {}x{} per eye",
                    user.id,
                    user.label,
                    stream.resolution_in_pixel[0],
                    stream.resolution_in_pixel[1],
                );
                self.streams.insert(user.id, stream);
                self.users.stream_started(user.id);
            }
            Admission::NoFreeSlot => log::info!(
                "qaros: {} ({}, peer {peer}) answered, but every slot is taken; its sender goes \
                 and it waits for a slot",
                user.id,
                user.label
            ),
            Admission::Left => log::info!(
                "qaros: {} (peer {peer}) left while its sender was created; the sender goes",
                user.id
            ),
        }
    }

    fn user_stream(peer: PeerId, sender: RenderSender) -> Result<UserStream, SinkError> {
        let layout = sender.layout().map_err(|error| backend(&error))?;
        let resolution_in_pixel = eye_resolution(&layout)?;
        log::info!("qaros: stream to peer {peer} has layout {:?}", layout.views);
        Ok(UserStream {
            peer,
            sender,
            resolution_in_pixel,
            frames_delivered: 0,
            state: FrameState::Idle,
            begin_waiting: false,
        })
    }

    /// Stops rendering for the user from the sink's side; a waiting user may take the slot.
    fn release(&mut self, user: UserId, cause: ReleaseCause) {
        self.users.release(user, cause);
        self.drop_stream(user);
    }

    /// True when the stream closed and the user was released.
    fn release_if_closed(&mut self, user: UserId, error: &qaros::Error) -> bool {
        let closed = error.has_code(StatusCode::RENDERING_STREAM_IS_CLOSED);
        if closed {
            log::info!("qaros: the render stream of {user} closed");
            self.release(user, ReleaseCause::Failed);
        }
        closed
    }

    /// A begin or a show failed: a closed stream releases the user at once, an unresolved app
    /// volume is not ready yet, and other errors release it once they keep failing.
    fn frame_failed(&mut self, user: UserId, error: &qaros::Error) {
        if error.has_code(StatusCode::RENDERING_APP_VOLUME_UNRESOLVED) {
            self.users.volume_unresolved(user);
        } else if !self.release_if_closed(user, error) && self.users.frame_failed(user, error) {
            self.release(user, ReleaseCause::Failed);
        }
    }

    fn release_stalled_users(&mut self) {
        let stalled = self.users.release_stalled();
        self.drop_streams(stalled);
    }

    /// Moves one user's frame lifecycle forward: polls for a frame when idle, and returns the
    /// request once a frame is open.
    fn advance(&mut self, user: UserId) -> Result<Option<FrameRequest>, qaros::Error> {
        let Some(stream) = self.streams.get_mut(&user) else {
            return Ok(None);
        };
        if matches!(stream.state, FrameState::Open(_)) {
            return Ok(None);
        }
        let begun = stream.sender.try_begin_owned_frame();
        stream.begin_waiting = matches!(begun, Ok(None));
        let Some(frame) = begun? else {
            return Ok(None);
        };
        let frame_id = stream.frames_delivered;
        stream.frames_delivered += 1;
        let resolution_in_pixel = stream.resolution_in_pixel;
        let mut views = Vec::with_capacity(EYE_COUNT);
        for eye_index in 0..EYE_COUNT {
            let view = frame.view(eye_index)?;
            views.push(ViewRequest {
                pose: self.pose_mapping.room_from_eye(view.pose),
                fov: to_fov(view.fov),
            });
        }
        let Ok(views) = <[ViewRequest; EYE_COUNT]>::try_from(views) else {
            return Ok(None);
        };
        if let Some(stream) = self.streams.get_mut(&user) {
            stream.state = FrameState::Open(OpenFrame {
                frame_id,
                frame,
                submitted: [false; EYE_COUNT],
            });
        }
        Ok(Some(FrameRequest {
            key: FrameKey { user, frame_id },
            views,
            resolution_in_pixel,
        }))
    }

    fn open_frame(&mut self, key: FrameKey) -> Result<&mut OpenFrame, SinkError> {
        match self
            .streams
            .get_mut(&key.user)
            .map(|stream| &mut stream.state)
        {
            Some(FrameState::Open(open)) if open.frame_id == key.frame_id => Ok(open),
            _ => Err(SinkError::UnknownFrame(key)),
        }
    }

    fn take_open_frame(&mut self, key: FrameKey) -> Option<OpenFrame> {
        let stream = self.streams.get_mut(&key.user)?;
        match std::mem::replace(&mut stream.state, FrameState::Idle) {
            FrameState::Open(open) if open.frame_id == key.frame_id => Some(open),
            other => {
                stream.state = other;
                None
            }
        }
    }
}

/// At Stop, the begin-frame waits end, where they can, before the senders go with the streams,
/// so no sender outlives the app volume; the frames they yield are released unshown. Senders
/// already released and still draining are part of the same bounded drain.
impl Drop for QarosSink {
    fn drop(&mut self) {
        let waiting = self
            .streams
            .iter_mut()
            .filter(|(_, stream)| stream.begin_waiting)
            .map(|(user, stream)| (*user, &mut stream.sender))
            .chain(
                self.draining
                    .iter_mut()
                    .map(|(user, sender)| (*user, sender)),
            )
            .collect();
        drop(drain_begin_waits(waiting, BEGIN_WAIT_DRAIN_LIMIT));
    }
}

/// Per-eye resolution from the negotiated layout: both eyes' color and depth views must have
/// the same size.
fn eye_resolution(layout: &VideoFrameLayout) -> Result<[u32; 2], SinkError> {
    let mut resolution = None;
    for kind in [ViewKind::Color, ViewKind::Depth] {
        for eye in EYES {
            let view_resolution =
                ViewRegion::of(&find_view(&layout.views, kind, eye)?).resolution_in_pixel();
            match resolution {
                None => resolution = Some(view_resolution),
                Some(expected) if expected != view_resolution => {
                    return Err(SinkError::Backend(format!(
                        "views differ in size ({expected:?} and {view_resolution:?}); layout {:?}",
                        layout.views
                    )));
                }
                Some(_) => {}
            }
        }
    }
    match resolution {
        Some(resolution) if !resolution.contains(&0) => Ok(resolution),
        _ => Err(SinkError::Backend(format!(
            "empty views in layout {:?}",
            layout.views
        ))),
    }
}

fn backend(error: &qaros::Error) -> SinkError {
    SinkError::Backend(error.to_string())
}

fn to_fov(fov: qaros::Fov) -> Fov {
    Fov {
        angle_left: fov.angle_left,
        angle_right: fov.angle_right,
        angle_up: fov.angle_up,
        angle_down: fov.angle_down,
    }
}

impl FrameSink for QarosSink {
    fn update_users(&mut self) {
        self.apply_user_events_and_creations();
    }

    fn begin_frames(&mut self) -> Result<Vec<FrameRequest>, SinkError> {
        self.apply_user_events_and_creations();
        if self.is_finished() {
            return Ok(Vec::new());
        }
        self.release_stalled_users();
        let users: Vec<UserId> = self.streams.keys().copied().collect();
        let mut requests = Vec::with_capacity(users.len());
        for user in users {
            match self.advance(user) {
                Ok(Some(request)) => {
                    self.users.frame_begun(user);
                    requests.push(request);
                }
                Ok(None) => {}
                Err(error) => self.frame_failed(user, &error),
            }
        }
        Ok(requests)
    }

    fn submit_view(
        &mut self,
        key: FrameKey,
        view_index: usize,
        image: EyeImage,
    ) -> Result<(), SinkError> {
        image.validate()?;
        let eye = *EYES
            .get(view_index)
            .ok_or(SinkError::UnknownView(view_index))?;
        let open = self.open_frame(key)?;
        let mut cpu_frame = open.frame.cpu_frame().map_err(|error| backend(&error))?;
        write_eye(&mut cpu_frame, eye, &image)?;
        open.submitted[view_index] = true;
        Ok(())
    }

    fn show_frame(&mut self, key: FrameKey, show: ShowInfo) -> Result<(), SinkError> {
        let open = self
            .take_open_frame(key)
            .ok_or(SinkError::UnknownFrame(key))?;
        if !open.submitted.iter().all(|&submitted| submitted) {
            return Err(SinkError::InvalidImage(format!(
                "{key} shown before both eyes were submitted"
            )));
        }
        let depth_is_linear = self.options.depth_range.encoding == DepthEncoding::LinearMeters;
        let shown = open
            .frame
            .show(&self.pose_mapping.show_params(show, depth_is_linear));
        if let Err(error) = shown {
            self.frame_failed(key.user, &error);
            return Err(backend(&error));
        }
        self.users.frame_shown(key.user);
        self.shown_frame_count += 1;
        Ok(())
    }

    fn abandon_frame(&mut self, key: FrameKey) {
        drop(self.take_open_frame(key));
    }

    fn is_streaming(&self) -> bool {
        !self.streams.is_empty() && !self.is_finished()
    }

    fn is_finished(&self) -> bool {
        self.options
            .stop_after_frames
            .is_some_and(|limit| self.shown_frame_count >= limit)
    }

    fn depth_range(&self) -> DepthRange {
        self.options.depth_range
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qaros::{TextureSize, VideoFrameView};

    fn pose_mapping(pose_space: PoseSpace) -> PoseMapping {
        PoseMapping {
            pose_space,
            room_from_app: Pose::IDENTITY,
            app_scale: 4.0,
        }
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "divisions by a power of two are exact")]
    fn near_far_and_linear_depth_go_back_in_the_poses_units() {
        let show = ShowInfo {
            near_far: xr_math::NearFar {
                near_plane: 0.2,
                far_plane: 20.0,
            },
            depth_scale: 1.0,
        };
        let app = pose_mapping(PoseSpace::App).show_params(show, false);
        assert_eq!((app.near, app.far, app.depth_scale), (0.05, 5.0, 1.0));
        let app_linear = pose_mapping(PoseSpace::App).show_params(show, true);
        assert_eq!(app_linear.depth_scale, 0.25);
        let room = pose_mapping(PoseSpace::Room).show_params(show, true);
        assert_eq!((room.near, room.far, room.depth_scale), (0.2, 20.0, 1.0));
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "divisions by a power of two are exact")]
    fn reversed_depth_reaches_qaros_with_near_greater_than_far() {
        let range = DepthRange::new(0.2, 20.0, xr_math::DepthEncoding::ReverseNormalized).unwrap();
        let app =
            pose_mapping(PoseSpace::App).show_params(ShowInfo::from_depth_range(range), false);
        assert_eq!((app.near, app.far), (5.0, 0.05));
    }

    fn layout_view(kind: ViewKind, eye: Eye, corners: [u32; 4]) -> VideoFrameView {
        let [start_x, start_y, end_x, end_y] = corners;
        VideoFrameView {
            start_x,
            start_y,
            end_x,
            end_y,
            texture_index: u32::from(kind == ViewKind::Depth),
            array_layer_index: 0,
            format: PixelFormat::B8G8R8A8,
            kind,
            eye,
        }
    }

    fn layout(right_depth_corners: [u32; 4]) -> VideoFrameLayout {
        VideoFrameLayout {
            views: vec![
                layout_view(ViewKind::Color, Eye::Left, [0, 0, 640, 480]),
                layout_view(ViewKind::Color, Eye::Right, [640, 0, 1280, 480]),
                layout_view(ViewKind::Depth, Eye::Left, [0, 0, 640, 480]),
                layout_view(ViewKind::Depth, Eye::Right, right_depth_corners),
            ],
            textures: vec![TextureSize {
                format: PixelFormat::B8G8R8A8,
                width: 1280,
                height: 480,
                array_layers: 1,
            }],
        }
    }

    #[test]
    fn eye_resolution_comes_from_the_layout_and_must_agree() {
        assert_eq!(
            eye_resolution(&layout([640, 0, 1280, 480])).unwrap(),
            [640, 480]
        );
        assert!(eye_resolution(&layout([640, 0, 1200, 480])).is_err());
        let mut missing_eye = layout([640, 0, 1280, 480]);
        missing_eye.views.pop();
        assert!(eye_resolution(&missing_eye).is_err());
    }
}
