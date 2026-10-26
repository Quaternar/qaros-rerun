//! QAROS's Vulkan render senders as a [`VulkanFrameStream`], over `qaros::vulkan` (QAROS
//! `qar-streaming-rust/docs/vulkan-api.md`): one sender per rendered user, separated color and depth textures per
//! eye as in QuaternAR's Vulkan cube, frames begun without blocking.

use std::collections::BTreeMap;
use std::rc::Rc;

use frame_sink::{EYE_COUNT, MaxUsers, ShowInfo, SinkError, UserId, UserInfo};
use qaros::vulkan::{VulkanImage, VulkanStreamParams, assumed_vk_format};
use qaros::{
    Eye, FrameLayout, FrameViewSpec, OwnedFrame, PeerId, PixelFormat, RenderSender,
    RenderSenderConfig, StatusCode, VideoFrameView, ViewKind,
};
use vulkan_device::{VulkanDevice, VulkanDeviceHandles, vk};
use xr_math::Fov;

use super::sink::{signal_discarded_frame, wait_for_device};
use super::stream::{ImageState, SenderEye, SenderFrame, SenderImage, VulkanFrameStream};
use crate::begin_drain::{BEGIN_WAIT_DRAIN_LIMIT, drain_begin_waits};
use crate::connection::QarosConnection;
use crate::pending::{FinishedCreation, PendingSenders};
use crate::sink::{PoseMapping, PoseSpace};
use crate::users::{Admission, PeerUsers, ReleaseCause, TargetPeers};

/// One texture per eye and kind, the cube's `QAR_FRAME_LAYOUT_SEPARATED_TEXTURES` request.
const STREAM_VIEWS: [FrameViewSpec; 4] = [
    FrameViewSpec {
        kind: ViewKind::Color,
        eye: Eye::Left,
        texture_index: 0,
    },
    FrameViewSpec {
        kind: ViewKind::Depth,
        eye: Eye::Left,
        texture_index: 1,
    },
    FrameViewSpec {
        kind: ViewKind::Color,
        eye: Eye::Right,
        texture_index: 2,
    },
    FrameViewSpec {
        kind: ViewKind::Depth,
        eye: Eye::Right,
        texture_index: 3,
    },
];

const EYES: [Eye; EYE_COUNT] = [Eye::Left, Eye::Right];

#[derive(Clone, Copy, Debug)]
pub struct QarosVulkanStreamOptions {
    pub pose_space: PoseSpace,
    /// Requested at sender creation; frames are rendered in whatever the images report.
    pub color_format: PixelFormat,
    /// Users rendered at the same time; later ones wait for a free slot.
    pub max_users: usize,
}

/// A begun frame and its semaphore, signalled when the frame goes unshown with its sender.
struct OpenSenderFrame {
    frame: OwnedFrame,
    semaphore: vk::Semaphore,
}

/// One user's render sender.
struct UserSender {
    peer: PeerId,
    /// Before `sender`: an open frame goes before its sender.
    open_frame: Option<OpenSenderFrame>,
    /// A frame begun right after the last show, handed out by the next `try_begin_frame`.
    early_frame: Option<OwnedFrame>,
    /// The library's begin-frame wait runs: the last `try_begin_owned_frame` found no frame
    /// ready.
    begin_waiting: bool,
    sender: RenderSender,
}

/// After a show the runtime readies the user's next frame only once `try_begin_frame` has been
/// called again; called first in the next viewer frame, the frame is ready one viewer frame
/// later, so every user would begin only every second viewer frame. Calling it right after the
/// show readies the frame for the next viewer frame. `QAROS_RERUN_BEGIN_KICK=0` turns it off.
fn begin_after_show_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var("QAROS_RERUN_BEGIN_KICK").map_or(true, |value| value != "0"))
}

/// What a begun frame's Vulkan record holds, copied out so the record's borrow of the frame
/// ends before the poses are read.
struct FrameRecord {
    views: Vec<VideoFrameView>,
    images: Vec<VulkanImage>,
    semaphore: vk::Semaphore,
}

/// Fields drop in declaration order: the senders go before the connection, so when this
/// stream holds the last reference the app volume is closed after its senders.
pub struct QarosVulkanStream {
    device: wgpu::Device,
    parameters: VulkanStreamParams,
    options: QarosVulkanStreamOptions,
    pose_mapping: PoseMapping,
    users: PeerUsers,
    pending: PendingSenders,
    senders: BTreeMap<UserId, UserSender>,
    /// Senders of released users whose begin-frame wait still ran, polled every viewer frame
    /// and destroyed once the wait has ended (see `begin_drain`).
    draining: Vec<(UserId, UserSender)>,
    connection: Rc<QarosConnection>,
}

impl QarosVulkanStream {
    /// # Safety
    ///
    /// `device` is the viewer's wgpu device on Vulkan, created on the connection's adapter when
    /// the session names one (`QarosConnection::adapter`), with
    /// `qaros::vulkan::required_device_extensions()` enabled; it outlives the stream, and
    /// nothing but the thread using the stream submits to its queue.
    #[allow(unsafe_code)]
    pub unsafe fn new(
        connection: Rc<QarosConnection>,
        device: &wgpu::Device,
        options: QarosVulkanStreamOptions,
    ) -> Result<Self, SinkError> {
        let handles = VulkanDeviceHandles::of(device)
            .ok_or_else(|| SinkError::Backend("the viewer's device is not on Vulkan".to_owned()))?;
        let missing: Vec<String> = qaros::vulkan::required_device_extensions()
            .iter()
            .filter(|&&extension| !handles.has_device_extension(extension))
            .map(|extension| extension.to_string_lossy().into_owned())
            .collect();
        if !missing.is_empty() {
            return Err(SinkError::Backend(format!(
                "the viewer's Vulkan device lacks the extensions QAROS needs: {}",
                missing.join(", ")
            )));
        }
        let pose_mapping = PoseMapping::new(connection.volume_config(), options.pose_space);
        let users = PeerUsers::new(
            Box::new(TargetPeers::new(Rc::clone(&connection))),
            Box::new(MaxUsers {
                maximum: options.max_users,
            }),
        );
        Ok(Self {
            device: device.clone(),
            parameters: VulkanStreamParams {
                physical_device: handles.physical_device,
                device: handles.device,
            },
            options,
            pose_mapping,
            users,
            pending: PendingSenders::default(),
            senders: BTreeMap::new(),
            draining: Vec::new(),
            connection,
        })
    }

    /// The stream on a device made by `vulkan_device::VulkanDevice::create`, with what
    /// [`Self::new`] requires checked: when the session names an adapter (reported, or chained
    /// with `ConnectOptions::adapter`), the device is on it; a session that names none takes
    /// the device as it is, since the launch invite carries a GPU only when the target
    /// advertised one. QAROS's extensions are enabled (checked by `new`); the stream keeps a
    /// clone of the device, so the device outlives the sender. Like every QAROS handle the
    /// stream stays on the thread that made it, the thread that submits the viewer's work.
    pub fn on_device(
        connection: Rc<QarosConnection>,
        vulkan: &VulkanDevice,
        options: QarosVulkanStreamOptions,
    ) -> Result<Self, SinkError> {
        let identity = &vulkan.physical_device;
        match connection.adapter() {
            Some(adapter) if !adapter.matches(identity.device_luid, identity.device_uuid) => {
                return Err(SinkError::Backend(format!(
                    "the session is on adapter {adapter}, the viewer's device is {identity}"
                )));
            }
            Some(_) => {}
            None => log::warn!(
                "qaros: the session names no adapter; the Vulkan render senders use the viewer's \
                 device as it is ({identity})"
            ),
        }
        #[allow(unsafe_code)]
        // SAFETY: checked above and in `new`, or held by construction: the device is on the
        // session's adapter when the session names one, the stream owns a clone of it, and the
        // stream is not `Send`.
        unsafe {
            Self::new(connection, &vulkan.device, options)
        }
    }

    /// The users rendered for right now, and the peer each one streams to.
    pub fn rendered_users(&self) -> Vec<(UserInfo, PeerId)> {
        self.users
            .rendered()
            .iter()
            .filter_map(|user| {
                let sender = self.senders.get(&user.id)?;
                Some((user.clone(), sender.peer))
            })
            .collect()
    }

    /// Users without a slot, in the order their senders are tried.
    pub fn waiting_users(&self) -> Vec<UserInfo> {
        self.users.waiting()
    }

    fn drop_senders(&mut self, users: Vec<UserId>) {
        for user in users {
            self.drop_sender(user);
        }
    }

    fn start_creations(&mut self) {
        for (user, peer) in self.users.creations_to_start() {
            self.connection.note_user_peer(user.id, peer);
            self.start_sender(user, peer);
        }
    }

    fn start_sender(&mut self, user: UserInfo, peer: PeerId) {
        let config = RenderSenderConfig {
            views: STREAM_VIEWS.to_vec(),
            app_volume: Some(self.connection.volume()),
            color_format: self.options.color_format,
            depth_format: PixelFormat::D32Float,
            layout: FrameLayout::SeparatedTextures,
            ..RenderSenderConfig::new(peer)
        };
        #[allow(unsafe_code)]
        // SAFETY: the parameters are the raw handles of the device `new` was given, under its
        // contract: on the session's adapter, with the required extensions, outliving this
        // stream and so the sender.
        let started = unsafe {
            self.connection
                .start_vulkan_render_sender(&config, self.parameters)
        };
        match started {
            Ok(creation) => self.pending.insert(user, peer, creation),
            Err(error) => {
                log::warn!(
                    "qaros: cannot create a Vulkan render sender for {}: {error}",
                    user.id
                );
                self.users.release(user.id, ReleaseCause::Failed);
            }
        }
    }

    /// Picks up the finished creations, those of the candidates first in line first.
    fn finish_creations(&mut self) {
        let mut finished = self.pending.take_finished();
        finished.sort_by_key(|creation| self.users.candidate_rank(creation.user.id));
        for creation in finished {
            self.finish_sender(creation);
        }
    }

    fn finish_sender(&mut self, finished: FinishedCreation) {
        let FinishedCreation { user, peer, sender } = finished;
        let sender = sender
            .map_err(|failure| (failure.message, failure.cause))
            .and_then(|sender| {
                Self::log_streaming(peer, sender)
                    .map_err(|error| (error.to_string(), ReleaseCause::Failed))
            });
        let sender = match sender {
            Ok(sender) => sender,
            Err((message, cause)) => {
                log::warn!(
                    "qaros: cannot create a Vulkan render sender for {}: {message}",
                    user.id
                );
                self.users.release(user.id, cause);
                return;
            }
        };
        match self.users.creation_succeeded(user.id) {
            Admission::Admitted(user) => {
                log::info!(
                    "qaros: rendering for {} ({}, peer {peer})",
                    user.id,
                    user.label
                );
                self.senders.insert(
                    user.id,
                    UserSender {
                        peer,
                        open_frame: None,
                        early_frame: None,
                        begin_waiting: false,
                        sender,
                    },
                );
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

    /// A sender whose begin-frame wait still runs is parked in `draining` instead of destroyed.
    fn drop_sender(&mut self, user: UserId) {
        let Some(user_sender) = self.senders.remove(&user) else {
            return;
        };
        if user_sender.begin_waiting {
            log::info!(
                "qaros: released {user} (peer {}); its sender goes once its begin-frame wait ends",
                user_sender.peer
            );
            self.draining.push((user, user_sender));
        } else {
            self.destroy_sender(user, user_sender);
        }
    }

    /// The frame an ended wait yields becomes the early frame, which `destroy_sender` signals
    /// and drops unshown.
    fn destroy_senders_whose_waits_ended(&mut self) {
        for (user, mut user_sender) in std::mem::take(&mut self.draining) {
            match user_sender.sender.try_begin_owned_frame() {
                Ok(None) => {
                    self.draining.push((user, user_sender));
                    continue;
                }
                Ok(Some(frame)) => user_sender.early_frame = Some(frame),
                Err(error) => {
                    log::info!("qaros: the begin-frame wait of released {user} ended in {error}");
                }
            }
            self.destroy_sender(user, user_sender);
        }
    }

    /// Drops the sender once the device is done with its images; a frame still open on it has
    /// its semaphore signalled first, so the sender can consume it.
    fn destroy_sender(&self, user: UserId, mut user_sender: UserSender) {
        let open_frame = user_sender.open_frame.take();
        let mut early_frame = user_sender.early_frame.take();
        let early_semaphore = early_frame
            .as_mut()
            .and_then(|frame| Self::read_record(frame).ok())
            .map(|record| record.semaphore);
        #[allow(unsafe_code)]
        // SAFETY: an open or early frame's semaphore has not been signalled yet, and this is
        // the thread that submits to the device's queue.
        unsafe {
            if let Some(open) = &open_frame {
                signal_discarded_frame(&self.device, open.semaphore);
            }
            if let Some(semaphore) = early_semaphore {
                signal_discarded_frame(&self.device, semaphore);
            }
            wait_for_device(&self.device);
        }
        drop(open_frame);
        drop(early_frame);
        log::info!("qaros: released {user} (peer {})", user_sender.peer);
    }

    /// Stops rendering for the user from the stream's side; a waiting user may take the slot.
    fn release(&mut self, user: UserId, cause: ReleaseCause) {
        self.users.release(user, cause);
        self.drop_sender(user);
    }

    /// At Stop: lets the begin-frame waits end, within [`BEGIN_WAIT_DRAIN_LIMIT`], the parked
    /// senders' too, and keeps the frames they yield as early frames, which
    /// [`Self::destroy_sender`] signals and drops.
    fn let_begin_waits_end(&mut self) {
        let waiting = self
            .senders
            .iter_mut()
            .filter(|(_, user_sender)| user_sender.begin_waiting)
            .chain(
                self.draining
                    .iter_mut()
                    .map(|(user, user_sender)| (&*user, user_sender)),
            )
            .map(|(user, user_sender)| (*user, &mut user_sender.sender))
            .collect();
        for (user, frame) in drain_begin_waits(waiting, BEGIN_WAIT_DRAIN_LIMIT) {
            let user_sender = self.senders.get_mut(&user).or_else(|| {
                self.draining
                    .iter_mut()
                    .find(|(parked, _)| *parked == user)
                    .map(|(_, user_sender)| user_sender)
            });
            if let Some(user_sender) = user_sender {
                user_sender.early_frame = Some(frame);
            }
        }
    }

    fn log_streaming(peer: PeerId, mut sender: RenderSender) -> Result<RenderSender, SinkError> {
        {
            let state = sender
                .vulkan_frame_state()
                .map_err(|error| backend(&error))?;
            log::info!(
                "qaros: streaming to peer {peer} through the Vulkan render sender; views {:?}, images {:?}",
                state.views(),
                state
                    .images()
                    .iter()
                    .map(|image| image.size)
                    .collect::<Vec<_>>()
            );
        }
        Ok(sender)
    }

    fn release_if_closed(&mut self, user: UserId, error: &qaros::Error) {
        if error.has_code(StatusCode::RENDERING_STREAM_IS_CLOSED) {
            log::info!("qaros: the render stream of {user} closed");
            self.release(user, ReleaseCause::Failed);
        }
    }

    fn read_record(frame: &mut OwnedFrame) -> Result<FrameRecord, SinkError> {
        let record = frame.vulkan_frame().map_err(|error| backend(&error))?;
        Ok(FrameRecord {
            views: record.views(),
            images: record.images(),
            semaphore: record.semaphore(),
        })
    }

    fn sender_frame(
        &self,
        frame: &OwnedFrame,
        record: &FrameRecord,
    ) -> Result<SenderFrame, SinkError> {
        let images = record
            .images
            .iter()
            .map(sender_image)
            .collect::<Result<Vec<_>, _>>()?;
        let mut eyes = Vec::with_capacity(EYE_COUNT);
        for eye in EYES {
            let color_view = find_view(&record.views, eye, ViewKind::Color)?;
            let depth_view = find_view(&record.views, eye, ViewKind::Depth).ok();
            let color_image = image_of_view(&record.views[color_view], &images)?;
            let depth_image = depth_view
                .map(|view| image_of_view(&record.views[view], &images))
                .transpose()?;
            // The view index selects the pose and field of view (`frame.view(index)`).
            let view = frame.view(color_view).map_err(|error| backend(&error))?;
            eyes.push(SenderEye {
                pose: self.pose_mapping.room_from_eye(view.pose),
                fov: Fov {
                    angle_left: view.fov.angle_left,
                    angle_right: view.fov.angle_right,
                    angle_up: view.fov.angle_up,
                    angle_down: view.fov.angle_down,
                },
                color_image,
                depth_image,
            });
        }
        let eyes = <[SenderEye; EYE_COUNT]>::try_from(eyes)
            .map_err(|_| SinkError::Backend("two eyes expected".to_owned()))?;
        Ok(SenderFrame {
            eyes,
            images,
            semaphore: record.semaphore,
        })
    }
}

/// At Stop, every sender goes with no begin-frame wait left running where one can end, so no
/// sender outlives the app volume, and with its open or early frame signalled.
impl Drop for QarosVulkanStream {
    fn drop(&mut self) {
        self.let_begin_waits_end();
        let senders = std::mem::take(&mut self.senders);
        let parked = std::mem::take(&mut self.draining);
        for (user, user_sender) in senders.into_iter().chain(parked) {
            self.destroy_sender(user, user_sender);
        }
    }
}

fn backend(error: &qaros::Error) -> SinkError {
    SinkError::Backend(error.to_string())
}

/// Writes the images' layouts and queue family back into the frame, sets its fence to null,
/// and shows it.
fn write_back_and_show(
    mut frame: OwnedFrame,
    states: &[ImageState],
    show_params: &qaros::ShowParams,
) -> Result<(), qaros::Error> {
    {
        let mut record = frame.vulkan_frame()?;
        for (index, state) in states.iter().enumerate() {
            record.set_image_state(index, state.layout, state.queue_family_index)?;
        }
        record.set_fence(vk::Fence::null());
    }
    frame.show(show_params)
}

/// The wgpu format of a QAROS pixel format, through the `VkFormat` qaros assumes for it.
fn texture_format(format: PixelFormat) -> Option<wgpu::TextureFormat> {
    match assumed_vk_format(format)? {
        vk::Format::R8G8B8A8_UNORM => Some(wgpu::TextureFormat::Rgba8Unorm),
        vk::Format::B8G8R8A8_UNORM => Some(wgpu::TextureFormat::Bgra8Unorm),
        vk::Format::R16G16B16A16_SFLOAT => Some(wgpu::TextureFormat::Rgba16Float),
        vk::Format::D32_SFLOAT => Some(wgpu::TextureFormat::Depth32Float),
        _ => None,
    }
}

fn sender_image(image: &VulkanImage) -> Result<SenderImage, SinkError> {
    if image.size.array_layers > 1 {
        return Err(SinkError::Backend(format!(
            "layered images are not supported ({:?})",
            image.size
        )));
    }
    let format = texture_format(image.size.format).ok_or_else(|| {
        SinkError::Backend(format!(
            "no wgpu render format for the image format {:?}",
            image.size.format
        ))
    })?;
    Ok(SenderImage {
        image: image.image,
        format,
        resolution_in_pixel: [image.size.width, image.size.height],
        format_features: image.format_features,
        layout: image.layout,
        queue_family_index: image.queue_family_index,
    })
}

fn find_view(views: &[VideoFrameView], eye: Eye, kind: ViewKind) -> Result<usize, SinkError> {
    views
        .iter()
        .position(|view| view.eye == eye && view.kind == kind)
        .ok_or_else(|| SinkError::Backend(format!("no {kind:?} view for the {eye:?} eye")))
}

/// The image a view renders into; the view must cover all of it, as separated textures do.
fn image_of_view(view: &VideoFrameView, images: &[SenderImage]) -> Result<usize, SinkError> {
    let index = usize::try_from(view.texture_index).unwrap_or(usize::MAX);
    let image = images.get(index).ok_or_else(|| {
        SinkError::Backend(format!(
            "view {view:?} names image {index}; the frame has {}",
            images.len()
        ))
    })?;
    let [width, height] = image.resolution_in_pixel;
    let covers_image = view.start_x == 0
        && view.start_y == 0
        && view.end_x == width
        && view.end_y == height
        && view.array_layer_index == 0;
    if !covers_image {
        return Err(SinkError::Backend(format!(
            "view {view:?} covers part of a {width}x{height} image; only separated textures are supported"
        )));
    }
    Ok(index)
}

impl VulkanFrameStream for QarosVulkanStream {
    fn update_users(&mut self) {
        self.connection.session().release_abandoned_creations();
        self.destroy_senders_whose_waits_ended();
        let left = self.users.poll();
        self.drop_senders(left);
        self.start_creations();
        self.finish_creations();
    }

    fn release_stalled_users(&mut self) {
        let stalled = self.users.release_stalled();
        self.drop_senders(stalled);
    }

    fn begin_failed(&mut self, user: UserId, error: &SinkError) {
        let has_sender = self.senders.contains_key(&user);
        if has_sender && self.users.frame_failed(user, error) {
            self.release(user, ReleaseCause::Failed);
        }
    }

    fn users(&self) -> Vec<UserId> {
        self.senders.keys().copied().collect()
    }

    fn try_begin_frame(&mut self, user: UserId) -> Result<Option<SenderFrame>, SinkError> {
        let Some(user_sender) = self.senders.get_mut(&user) else {
            return Ok(None);
        };
        if user_sender.open_frame.is_some() {
            return Err(SinkError::Backend(format!(
                "a frame of {user} is already open on its Vulkan render sender"
            )));
        }
        let begun = match user_sender.early_frame.take() {
            Some(frame) => Ok(Some(frame)),
            None => user_sender.sender.try_begin_owned_frame(),
        };
        user_sender.begin_waiting = matches!(begun, Ok(None));
        let mut frame = match begun {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(None),
            Err(error) if error.has_code(StatusCode::RENDERING_APP_VOLUME_UNRESOLVED) => {
                self.users.volume_unresolved(user);
                return Ok(None);
            }
            Err(error) => {
                self.release_if_closed(user, &error);
                return Err(backend(&error));
            }
        };
        let record = match Self::read_record(&mut frame) {
            Ok(record) => record,
            Err(error) => {
                // Without its record the frame's semaphore is unknown and cannot be signalled,
                // so the sender would wait for it for good.
                log::warn!(
                    "qaros: cannot read the Vulkan record of a frame of {user} ({error}); \
                     releasing its sender"
                );
                drop(frame);
                self.release(user, ReleaseCause::Failed);
                return Err(error);
            }
        };
        match self.sender_frame(&frame, &record) {
            Ok(sender_frame) => {
                if let Some(user_sender) = self.senders.get_mut(&user) {
                    user_sender.open_frame = Some(OpenSenderFrame {
                        frame,
                        semaphore: record.semaphore,
                    });
                }
                self.users.frame_begun(user);
                Ok(Some(sender_frame))
            }
            Err(error) => {
                #[allow(unsafe_code)]
                // SAFETY: the frame just began, so its semaphore is unsignalled, and this is
                // the thread that submits to the device's queue.
                unsafe {
                    signal_discarded_frame(&self.device, record.semaphore);
                }
                Err(error)
            }
        }
    }

    fn show_frame(
        &mut self,
        user: UserId,
        states: &[ImageState],
        show: ShowInfo,
    ) -> Result<(), SinkError> {
        let frame = self
            .senders
            .get_mut(&user)
            .and_then(|user_sender| user_sender.open_frame.take())
            .map(|open| open.frame)
            .ok_or_else(|| SinkError::Backend(format!("no frame of {user} is open")))?;
        // The Vulkan sink refuses linear depth, so depth here is always normalized.
        let show_params = self.pose_mapping.show_params(show, false);
        if let Err(error) = write_back_and_show(frame, states, &show_params) {
            // The release batch has signalled the frame's semaphore, and the header does not
            // say whether the sender consumes it for a frame that was not shown, nor whether
            // the next frame hands out the same binary semaphore, which would then be signalled
            // twice. QuaternAR's cube gives up such a target; so does this stream: the sender,
            // with its semaphore, goes once the device is idle, and the target joins again
            // after its back-off.
            log::warn!(
                "qaros: showing a frame of {user} failed after its hand-over ({error}); releasing its sender"
            );
            self.release(user, ReleaseCause::Failed);
            return Err(backend(&error));
        }
        self.users.frame_shown(user);
        if begin_after_show_enabled()
            && let Some(user_sender) = self.senders.get_mut(&user)
        {
            let early = user_sender.sender.try_begin_owned_frame();
            user_sender.begin_waiting = matches!(early, Ok(None));
            match early {
                Ok(early) => user_sender.early_frame = early,
                Err(error) => log::debug!("qaros: early begin for {user}: {error}"),
            }
        }
        Ok(())
    }

    fn discard_frame(&mut self, user: UserId) {
        if let Some(user_sender) = self.senders.get_mut(&user) {
            user_sender.open_frame = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(eye: Eye, kind: ViewKind, texture_index: u32, end: [u32; 2]) -> VideoFrameView {
        VideoFrameView {
            start_x: 0,
            start_y: 0,
            end_x: end[0],
            end_y: end[1],
            texture_index,
            array_layer_index: 0,
            format: PixelFormat::R8G8B8A8,
            kind,
            eye,
        }
    }

    fn image(width: u32) -> SenderImage {
        SenderImage {
            image: vk::Image::null(),
            format: wgpu::TextureFormat::Rgba8Unorm,
            resolution_in_pixel: [width, 32],
            format_features: vk::FormatFeatureFlags::COLOR_ATTACHMENT,
            layout: vk::ImageLayout::UNDEFINED,
            queue_family_index: vk::QUEUE_FAMILY_IGNORED,
        }
    }

    #[test]
    fn views_must_cover_whole_separated_images() {
        let images = [image(64), image(128)];
        assert_eq!(
            image_of_view(&view(Eye::Left, ViewKind::Color, 1, [128, 32]), &images).unwrap(),
            1
        );
        assert!(image_of_view(&view(Eye::Left, ViewKind::Color, 1, [64, 32]), &images).is_err());
        assert!(image_of_view(&view(Eye::Left, ViewKind::Color, 2, [64, 32]), &images).is_err());
    }

    #[test]
    fn views_are_found_by_eye_and_kind() {
        let views = [
            view(Eye::Left, ViewKind::Color, 0, [64, 32]),
            view(Eye::Left, ViewKind::Depth, 1, [64, 32]),
            view(Eye::Right, ViewKind::Color, 2, [64, 32]),
        ];
        assert_eq!(find_view(&views, Eye::Right, ViewKind::Color).unwrap(), 2);
        assert_eq!(find_view(&views, Eye::Left, ViewKind::Depth).unwrap(), 1);
        assert!(find_view(&views, Eye::Right, ViewKind::Depth).is_err());
    }

    #[test]
    fn pixel_formats_map_to_render_formats() {
        assert_eq!(
            texture_format(PixelFormat::R8G8B8A8),
            Some(wgpu::TextureFormat::Rgba8Unorm)
        );
        assert_eq!(
            texture_format(PixelFormat::B8G8R8A8),
            Some(wgpu::TextureFormat::Bgra8Unorm)
        );
        assert_eq!(
            texture_format(PixelFormat::D32Float),
            Some(wgpu::TextureFormat::Depth32Float)
        );
        assert_eq!(texture_format(PixelFormat::R32Float), None);
    }
}
