use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use frame_sink::UserId;
use qaros::PeerId;
use qaros::{
    AppVolumeConfig, AppVolumeId, AppVolumeLifetime, AppVolumeSize, BlockingOptions,
    GraphicsAdapter, Library, LibraryConfig, OnboardMethod, OnboardingId, PeerInfo,
    PeerPresentation, PendingRenderSender, RenderRequest, RenderSenderConfig, Runtime,
    RuntimeConfig, Session, StatusCode, Subscription,
};
use xr_math::AppPlacement;

use crate::peer_devices::{PeerDevice, read_peer_device};
use crate::runtime_version::{MINIMUM_RUNTIME_VERSION, is_older_than};
use crate::stop_watchdog::{TeardownStep, enter_teardown_step};

/// When a render sender's creation gives up. A creation finishes once the target answers,
/// which can take a while for a target that is still starting up. The creation runs in the
/// background and holds no user slot, so the timeout only bounds how long the creation for a
/// target that does not answer runs before the target is held back as unanswered.
pub(crate) const SENDER_CREATION_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a local app's first start waits for a person to allow it in the Hub; `None`
/// leaves it to the Hub.
const LOCAL_APP_APPROVAL_TIMEOUT: Option<Duration> = None;

/// What the Hub waits when `approval_timeout_ms` is 0 (`qar_onboard_local_app_ext_default`
/// in `qar_streaming.h`).
const HUB_DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_mins(2);

/// `QAROS_DLL`, else `None`: the binding's default lookup ([`Library::load_default`]).
pub fn library_path_from_environment() -> Option<PathBuf> {
    std::env::var_os("QAROS_DLL").map(PathBuf::from)
}

/// `QAROS_RUNTIME_DIR`, else `None` (the library's own directory).
pub fn runtime_directory_from_environment() -> Option<PathBuf> {
    std::env::var_os("QAROS_RUNTIME_DIR").map(PathBuf::from)
}

/// `QAROS_STORAGE_DIR`, else `None` (the library default). Tests point it at a scratch
/// directory so the runtime's certificates and session state stay out of the user's.
pub fn storage_directory_from_environment() -> Option<PathBuf> {
    std::env::var_os("QAROS_STORAGE_DIR").map(PathBuf::from)
}

/// Wakes the viewer from a library thread, for example `egui::Context::request_repaint`.
pub type WakeViewer = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug)]
pub struct ConnectOptions {
    /// `None` finds the runtime with the binding's default lookup: a runtime already loaded,
    /// one beside the executable, `QAROS_RUNTIME_DIR`, the installed QAROS, then `PATH`.
    pub library_path: Option<PathBuf>,
    /// The package's `bin/`; `None` uses the library's directory, as the C samples do.
    pub runtime_binaries_directory: Option<PathBuf>,
    /// `None` keeps the library default (`%LOCALAPPDATA%\Quaternar\QarOS` on Windows).
    pub storage_directory: Option<PathBuf>,
    /// Holds the onboarding id in the library's UUID text form; written after onboarding.
    pub onboarding_id_file: PathBuf,
    /// Remembers the targets whose sender creation timed out, so the next runs hold them back
    /// too (`TargetPeers`); `None` remembers them for this run only.
    pub unanswered_peers_file: Option<PathBuf>,
    /// The Hub's current pairing code; used when there is no stored id or rejoin fails.
    pub pairing_code: Option<String>,
    /// The first stdin line when QAROS launches the app ("Launched by QAROS" in
    /// `onboarding-and-sessions.mdx`): a JSON launch invite (starts with `{`), or a code typed
    /// by hand. Valid for this start only, so it is always onboarded with, never replaced by
    /// a rejoin, and the resulting id is not stored.
    pub launch_line: Option<String>,
    /// Join the Hub installed on this PC as a local app (`QarOnboardLocalAppExt`), with no
    /// code or invite. The Hub asks a person to allow the app on its first start. Stateless,
    /// so it takes precedence over the launch line, a stored id and a pairing code.
    pub local_app: bool,
    /// How the viewer appears in the peer list.
    pub peer_display_name: String,
    /// The app volume's common name: QAROS hands an app that asks for an existing name that
    /// volume and its render stream.
    pub volume_name: String,
    /// How the app volume is labelled for people; unlike the common name it may repeat.
    pub volume_display_name: String,
    /// Width, length, height in room meters.
    pub volume_size: [f32; 3],
    /// Volume pose, app pose and app scale of the volume; the renderer places Rerun's world
    /// with the same values.
    pub placement: AppPlacement,
    pub console_logging: bool,
    /// Sets the process's GPU scheduling priority to high on Windows (a diagnostic).
    pub raise_gpu_priority: bool,
    /// The GPU the session's devices and every render sender's images live on
    /// (`QarGraphicsDeviceId`, chained into onboard and rejoin); `None` leaves it to QAROS
    /// (the GPU in the launch invite) or the library, and the viewer creates its device on
    /// the adapter the session reports. Set only when the viewer's device exists first
    /// (`--vulkan-device`).
    pub adapter: Option<GraphicsAdapter>,
}

/// The session and what hangs off it. Fields drop in declaration order: the subscriptions are
/// cancelled before the session is destroyed, and the library goes last.
pub struct QarosConnection {
    request_subscription: Subscription,
    peer_subscription: Subscription,
    requests: Receiver<RenderRequest>,
    peer_updates: Receiver<PeerInfo>,
    wake_viewer: WakeViewer,
    volume: AppVolumeId,
    volume_config: AppVolumeConfig,
    adapter: Option<GraphicsAdapter>,
    unanswered_peers_file: Option<PathBuf>,
    /// The peer each user the sink created a sender for streams to. User ids are never reused,
    /// so an entry stays right after its user leaves.
    peers_by_user: RefCell<BTreeMap<UserId, PeerId>>,
    session: Session,
    _library_dropped_last: LibraryDroppedLast,
}

/// Holds the library past the session, so its destruction is a teardown step of its own.
struct LibraryDroppedLast {
    _library: Library,
}

impl Drop for LibraryDroppedLast {
    fn drop(&mut self) {
        enter_teardown_step(TeardownStep::LibraryDestroy);
    }
}

impl std::fmt::Debug for QarosConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QarosConnection")
            .field("request_subscription", &self.request_subscription)
            .field("peer_subscription", &self.peer_subscription)
            .field("volume", &self.volume)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

/// The volume is closed while the session is still alive: a session that just goes away leaves
/// its volume in the room. The connection drops last, after the sink and the panels that hold
/// it, so the panels are hidden (not closed) before the volume closes.
impl Drop for QarosConnection {
    fn drop(&mut self) {
        enter_teardown_step(TeardownStep::CloseVolume);
        match self.session.app_volumes().close(self.volume) {
            Ok(()) => log::info!(
                "qaros: closed app volume {:?}",
                self.volume_config.common_name
            ),
            Err(error) => log::warn!(
                "qaros: cannot close app volume {:?}: {error}",
                self.volume_config.common_name
            ),
        }
        // The fields drop after this body: the subscriptions, the session, then the library.
        enter_teardown_step(TeardownStep::SessionDestroy);
    }
}

impl QarosConnection {
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn volume(&self) -> AppVolumeId {
        self.volume
    }

    pub fn volume_config(&self) -> &AppVolumeConfig {
        &self.volume_config
    }

    /// The adapter the session's GPU devices live on (`qar_session_get_graphics_device_id`),
    /// else the one chained at onboarding; `None` when neither is known.
    pub fn adapter(&self) -> Option<GraphicsAdapter> {
        self.adapter
    }

    pub fn unanswered_peers_file(&self) -> Option<&Path> {
        self.unanswered_peers_file.as_deref()
    }

    /// The sink starts creating `user`'s render sender for `peer`.
    pub(crate) fn note_user_peer(&self, user: UserId, peer: PeerId) {
        self.peers_by_user.borrow_mut().insert(user, peer);
    }

    pub fn peer_of_user(&self, user: UserId) -> Option<PeerId> {
        self.peers_by_user.borrow().get(&user).copied()
    }

    /// What QAROS says `peer`'s device is; `Ok(None)` when the peer list does not hold it.
    pub fn peer_device(&self, peer: PeerId) -> Result<Option<PeerDevice>, String> {
        read_peer_device(self, peer)
    }

    /// Every render stream request since the last call, oldest first.
    pub fn take_render_requests(&self) -> Vec<RenderRequest> {
        self.requests.try_iter().collect()
    }

    /// Every peer update since the last call, oldest first.
    pub fn take_peer_updates(&self) -> Vec<PeerInfo> {
        self.peer_updates.try_iter().collect()
    }

    /// Wakes the viewer from any thread, for example when an async frame arrives.
    pub fn wake_viewer(&self) -> WakeViewer {
        Arc::clone(&self.wake_viewer)
    }

    /// Starts creating a CPU render sender in the background; the viewer is woken when it
    /// finishes and picks it up with `PendingRenderSender::poll`.
    pub fn start_render_sender(
        &self,
        config: &RenderSenderConfig,
    ) -> qaros::Result<PendingRenderSender> {
        let wake_viewer = self.wake_viewer();
        self.session
            .start_render_sender(config, Some(SENDER_CREATION_TIMEOUT), move || {
                wake_viewer();
            })
    }

    /// Starts creating a Vulkan render sender in the background, as
    /// [`Self::start_render_sender`].
    ///
    /// # Safety
    ///
    /// As `qaros::Session::start_vulkan_render_sender`: `vulkan` names a device on the
    /// session's adapter with QAROS's extensions, which outlives the sender.
    #[cfg(feature = "vulkan")]
    #[allow(unsafe_code)]
    pub unsafe fn start_vulkan_render_sender(
        &self,
        config: &RenderSenderConfig,
        vulkan: qaros::vulkan::VulkanStreamParams,
    ) -> qaros::Result<PendingRenderSender> {
        let wake_viewer = self.wake_viewer();
        // SAFETY: forwarded to the caller.
        unsafe {
            self.session.start_vulkan_render_sender(
                config,
                vulkan,
                Some(SENDER_CREATION_TIMEOUT),
                move || wake_viewer(),
            )
        }
    }
}

pub fn connect(
    options: &ConnectOptions,
    wake_viewer: WakeViewer,
) -> Result<QarosConnection, String> {
    crate::host_timing::keep_process_waits_precise();
    if options.raise_gpu_priority {
        crate::host_timing::raise_gpu_priority();
    }
    let library_config = LibraryConfig {
        console_logging: options.console_logging,
        ..LibraryConfig::default()
    };
    // SAFETY: the path, or the default lookup, names a qar-streaming-c build matching the
    // header vendored in qaros-sys (the QAROS package, or qaros-stub).
    #[allow(unsafe_code)]
    let library = match &options.library_path {
        Some(path) => unsafe { Library::load(path, &library_config) }
            .map_err(|error| format!("{}: {error}", path.display()))?,
        None => unsafe { Library::load_default(&library_config) }
            .map_err(|error| format!("the QAROS runtime: {error}"))?,
    };
    if let Some(loaded) = library.library_path() {
        log::info!("qaros: loaded {}", loaded.display());
    }
    match library.runtime_info() {
        Ok(info) => {
            log::info!(
                "qaros: runtime {} ({}, C API {})",
                info.version,
                info.flavor,
                info.c_api
            );
            if is_older_than(&info.version, MINIMUM_RUNTIME_VERSION) == Some(true) {
                log::warn!(
                    "qaros: runtime {} is older than {MINIMUM_RUNTIME_VERSION}, the oldest this \
                     viewer supports; its mixer can fail when stream panels restart",
                    info.version
                );
            }
        }
        Err(error) => log::warn!("qaros: cannot read the runtime's version: {error}"),
    }
    let runtime = library
        .create_runtime(&RuntimeConfig {
            runtime_binaries_directory: options.runtime_binaries_directory.clone().or_else(|| {
                library
                    .library_path()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
            }),
            storage_directory: options.storage_directory.clone(),
            ephemeral_storage: false,
        })
        .map_err(|error| error.to_string())?;
    let presentation = PeerPresentation {
        display_name: Some(options.peer_display_name.clone()),
        app_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        custom_info: None,
        device_identity: None,
    };
    let session = join_session(&library, &runtime, options, &presentation)?;
    let adapter = session_adapter(&session, options.adapter);

    let volume_config = volume_config(options);
    let volume = session
        .app_volumes()
        .get_or_create(&volume_config)
        .map_err(|error| error.to_string())?;
    warn_if_volume_closed(&session, volume, &volume_config.common_name);

    // Peer updates first: a peer that requests a stream right away is then already listed.
    let (peer_subscription, peer_updates) = forward_and_wake(&wake_viewer, |forward| {
        session.subscribe_peer_updates(forward)
    })?;
    let (request_subscription, requests) = forward_and_wake(&wake_viewer, |forward| {
        session.subscribe_render_requests(forward)
    })?;

    let library_dropped_last = LibraryDroppedLast {
        _library: session.library(),
    };
    Ok(QarosConnection {
        request_subscription,
        peer_subscription,
        requests,
        peer_updates,
        wake_viewer,
        volume,
        volume_config,
        adapter,
        unanswered_peers_file: options.unanswered_peers_file.clone(),
        peers_by_user: RefCell::new(BTreeMap::new()),
        session,
        _library_dropped_last: library_dropped_last,
    })
}

/// A stopped viewer closes its volume, and the header does not say whether `get_or_create`
/// with the same common name reopens it or hands back the closed one. The latter would leave
/// this run with nothing on screen, so it is reported where the run's log starts.
fn warn_if_volume_closed(session: &Session, volume: AppVolumeId, common_name: &str) {
    match session.app_volumes().list() {
        Ok(volumes) => {
            let closed = volumes
                .iter()
                .any(|listed| listed.id == volume && listed.lifetime == AppVolumeLifetime::Closed);
            if closed {
                log::warn!(
                    "qaros: QAROS handed back app volume {common_name:?} closed by an earlier run; content may not show (use another --volume-name)"
                );
            }
        }
        Err(error) => log::warn!("qaros: cannot list app volumes: {error}"),
    }
}

/// The session's report wins over the chained adapter: it is where QAROS actually created the
/// session's devices.
fn session_adapter(session: &Session, chained: Option<GraphicsAdapter>) -> Option<GraphicsAdapter> {
    match session.graphics_adapter() {
        Ok(Some(reported)) => {
            if chained.is_some_and(|chained| chained != reported) {
                log::warn!("qaros: the session reports adapter {reported}, not the chained one");
            }
            log::info!("qaros: the session's GPU is adapter {reported}");
            Some(reported)
        }
        Ok(None) => chained,
        Err(error) => {
            log::warn!("qaros: cannot read the session's adapter: {error}");
            chained
        }
    }
}

/// Subscribes with a callback that forwards each event into a channel and wakes the viewer.
fn forward_and_wake<Event: Send + 'static>(
    wake_viewer: &WakeViewer,
    subscribe: impl FnOnce(Box<dyn Fn(Event) + Send + Sync>) -> qaros::Result<Subscription>,
) -> Result<(Subscription, Receiver<Event>), String> {
    let (forward, events) = mpsc::channel();
    let wake_viewer = Arc::clone(wake_viewer);
    let subscription = subscribe(Box::new(move |event| {
        // A disconnected receiver means the sink is gone; nobody needs waking.
        if forward.send(event).is_ok() {
            wake_viewer();
        }
    }))
    .map_err(|error| error.to_string())?;
    Ok((subscription, events))
}

fn join_session(
    library: &Library,
    runtime: &Runtime,
    options: &ConnectOptions,
    presentation: &PeerPresentation,
) -> Result<Session, String> {
    let blocking = BlockingOptions::default();
    if options.local_app {
        let approval_seconds = LOCAL_APP_APPROVAL_TIMEOUT
            .unwrap_or(HUB_DEFAULT_APPROVAL_TIMEOUT)
            .as_secs();
        // `qaros::CancelToken` cannot be cancelled from another thread, so the stdin watcher
        // cannot end this wait.
        log::info!(
            "qaros: onboarding as a local app; on its first start the Hub asks to allow it (up to {approval_seconds} s), and a Stop meanwhile takes effect only once the wait ends"
        );
        return runtime
            .onboard_on_adapter(
                &OnboardMethod::LocalApp {
                    approval_timeout: LOCAL_APP_APPROVAL_TIMEOUT,
                    app_tag: None,
                },
                presentation,
                options.adapter,
                &blocking,
            )
            .map(|onboarded| onboarded.session)
            .map_err(|error| format!("local-app onboarding: {error}"));
    }
    if let Some(launch_line) = options.launch_line.as_deref() {
        return onboard_with_launch_line(library, runtime, launch_line, presentation, options);
    }
    match rejoin_with_stored_id(library, runtime, options, presentation, &blocking) {
        Ok(Some(session)) => return Ok(session),
        Ok(None) => {}
        Err(error) if options.pairing_code.is_some() => {
            log::warn!("qaros: cannot rejoin ({error}); pairing with --pairing-code instead");
        }
        Err(error) => return Err(error),
    }

    let Some(pairing_code) = options.pairing_code.as_deref() else {
        return Err(format!(
            "no usable onboarding id in {}; pass --pairing-code with the Hub's current code",
            options.onboarding_id_file.display()
        ));
    };
    let onboarded = runtime
        .onboard_on_adapter(
            &OnboardMethod::Code {
                code: pairing_code,
                hub: None,
            },
            presentation,
            options.adapter,
            &blocking,
        )
        .map_err(|error| error.to_string())?;
    let id_text = library
        .uuid_to_string(&onboarded.onboarding_id.0)
        .map_err(|error| error.to_string())?;
    match std::fs::write(&options.onboarding_id_file, format!("{id_text}\n")) {
        Ok(()) => log::info!(
            "qaros: onboarded; id stored in {} for the next start",
            options.onboarding_id_file.display()
        ),
        Err(error) => log::warn!(
            "qaros: onboarded, but cannot store the id in {} ({error}); the next start needs a new pairing code",
            options.onboarding_id_file.display()
        ),
    }
    Ok(onboarded.session)
}

/// The session rejoined with the id stored in the onboarding id file; `None` when there is no
/// file or the runtime no longer knows the id.
fn rejoin_with_stored_id(
    library: &Library,
    runtime: &Runtime,
    options: &ConnectOptions,
    presentation: &PeerPresentation,
    blocking: &BlockingOptions<'_>,
) -> Result<Option<Session>, String> {
    let Some(onboarding_id) = read_onboarding_id(library, &options.onboarding_id_file)? else {
        return Ok(None);
    };
    match runtime.rejoin_on_adapter(onboarding_id, presentation, options.adapter, blocking) {
        Ok(session) => {
            log::info!(
                "qaros: rejoined with the onboarding id in {}",
                options.onboarding_id_file.display()
            );
            Ok(Some(session))
        }
        Err(error) if error.has_code(StatusCode::ONBOARDING_SESSION_NOT_FOUND) => {
            log::warn!("qaros: the stored onboarding id is unknown to the runtime; onboarding");
            Ok(None)
        }
        Err(error) => Err(format!(
            "rejoining with the onboarding id in {}: {error}",
            options.onboarding_id_file.display()
        )),
    }
}

/// A line starting with `{` is the launch invite, passed unchanged to
/// `qar_onboarding_invite_deserialize`; anything else is a code typed by hand.
fn onboard_with_launch_line(
    library: &Library,
    runtime: &Runtime,
    launch_line: &str,
    presentation: &PeerPresentation,
    options: &ConnectOptions,
) -> Result<Session, String> {
    let blocking = BlockingOptions::default();
    let onboarded = if launch_line.starts_with('{') {
        log::info!("qaros: onboarding with the launch invite from stdin");
        let invite = library
            .deserialize_invite(launch_line.as_bytes())
            .map_err(|error| format!("the launch invite from stdin: {error}"))?;
        runtime.onboard_on_adapter(
            &OnboardMethod::Invite(&invite),
            presentation,
            options.adapter,
            &blocking,
        )
    } else {
        log::info!("qaros: onboarding with the code from stdin");
        runtime.onboard_on_adapter(
            &OnboardMethod::Code {
                code: launch_line,
                hub: None,
            },
            presentation,
            options.adapter,
            &blocking,
        )
    };
    onboarded
        .map(|onboarded| onboarded.session)
        .map_err(|error| format!("onboarding with the line from stdin: {error}"))
}

fn read_onboarding_id(library: &Library, path: &Path) -> Result<Option<OnboardingId>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let bytes = library
        .uuid_from_string(&text)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(Some(OnboardingId(bytes)))
}

fn volume_config(options: &ConnectOptions) -> AppVolumeConfig {
    let [width, length, height] = options.volume_size;
    AppVolumeConfig {
        display_name: Some(options.volume_display_name.clone()),
        pose: to_qaros_pose(options.placement.volume_pose),
        size: AppVolumeSize {
            width,
            length,
            height,
        },
        app_pose: to_qaros_pose(options.placement.app_pose),
        app_scale: options.placement.app_scale,
        ..AppVolumeConfig::new(options.volume_name.clone())
    }
}

pub(crate) fn to_qaros_pose(pose: xr_math::Pose) -> qaros::Pose {
    qaros::Pose {
        orientation: qaros::Quat {
            x: pose.orientation.x,
            y: pose.orientation.y,
            z: pose.orientation.z,
            w: pose.orientation.w,
        },
        position: qaros::Vec3::new(pose.position.x, pose.position.y, pose.position.z),
    }
}

pub(crate) fn from_qaros_pose(pose: qaros::Pose) -> xr_math::Pose {
    let orientation = pose.orientation;
    let position = pose.position;
    xr_math::Pose::from_xyzw_and_position(
        [orientation.x, orientation.y, orientation.z, orientation.w],
        [position.x, position.y, position.z],
    )
}
