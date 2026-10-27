//! The QAROS sink's lifecycle against `qaros-stub` (from the QAROS repository): load, onboard,
//! app volume, render requests, one sender per user, frame submit and show, users joining,
//! waiting and leaving, panels, teardown, and rejoin with the stored onboarding id. The stub's
//! simulated visualizer requests a stream as soon as one is subscribed, with 64x64 views and
//! eyes at x = -0.032 and 0.032, y = 1.6; peers joined later stand 0.5 m apart along x. The
//! stub decodes the frame stamp of every shown frame and logs whether it matches the frame
//! the sender issued ("paired ..." or "ERROR pairing ..."). With the `vulkan` feature, the
//! Vulkan render senders too, on MoltenVK: the stub creates its images on the test's device
//! and checks the write-back and the semaphore at `show_frame`.
//!
//! The library is process-wide, so the tests run one at a time.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, unsafe_code)]

#[cfg(feature = "vulkan")]
mod common;

use std::ffi::{CString, c_char};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use frame_sink::{
    DepthEncoding, DepthRange, EyeImage, FrameKey, FrameRequest, FrameSink, FrameStamp, MaxUsers,
    ShowInfo, UserId, stamp_eye,
};
use glam::{Quat, Vec3};
use qaros::{GuiPanelState, PeerId, PixelFormat};
use qaros_sink::{
    BEGIN_WAIT_DRAIN_LIMIT, ConnectOptions, DeadPages, FRAME_FAILURE_MINIMUM, FRAME_FAILURE_PERIOD,
    HEADSET_WAIT, PAGE_SILENCE_LIMIT, PanelActivity, PanelView, PanelsAtStop, PeerEvent, PoseSpace,
    QarosConnection, QarosPanels, QarosSink, QarosSinkOptions, ReleaseCause, TargetPeers,
    TeardownStep, UserEventSource, WakeViewer, connect, current_teardown_step, enter_teardown_step,
};
use xr_math::{AppPlacement, Pose};

static SERIAL: Mutex<()> = Mutex::new(());

const VISUALIZER_PEER: PeerId = PeerId([0x56; 16]);
const HEADSET_A: PeerId = PeerId([0xa1; 16]);
const HEADSET_B: PeerId = PeerId([0xb2; 16]);

const UNANSWERED_PEERS_FILE_NAME: &str = "qaros-rerun.unanswered-peers.txt";

/// How far past [`BEGIN_WAIT_DRAIN_LIMIT`] a Stop with a wait that never ends may take on a
/// loaded machine; well inside the stop watchdog's 1 s plus the bound.
const DRAIN_TIMING_SLACK: Duration = Duration::from_secs(1);

/// `QAR_STATUS_UNCLASSIFIED`, `QAR_STATUS_TIMEOUT` and `QAR_STATUS_LOGIC_ERROR`, for injected
/// failures.
const UNCLASSIFIED: i64 = 1;
const TIMEOUT: i64 = 6;
const LOGIC_ERROR: i64 = 7;

/// `QAR_STATUS_RENDERING_PRODUCER_APP_VOLUME_UNRESOLVED`, recoverable.
const APP_VOLUME_UNRESOLVED: i64 = 805;

/// Cargo names the stub `qaros_stub` when it comes from a path and `qaros_stub-<hash>` when it
/// comes from git; the newest match next to the test executable wins.
fn stub_path() -> PathBuf {
    let prefix = format!("{}qaros_stub", std::env::consts::DLL_PREFIX);
    let suffix = std::env::consts::DLL_SUFFIX;
    let executable = std::env::current_exe().expect("test executable path");
    let deps_directory = executable.parent().expect("deps directory");
    [
        deps_directory,
        deps_directory.parent().expect("profile directory"),
    ]
    .iter()
    .filter_map(|directory| std::fs::read_dir(directory).ok())
    .flatten()
    .filter_map(Result::ok)
    .filter(|entry| is_stub_library(&entry.file_name().to_string_lossy(), &prefix, suffix))
    .max_by_key(|entry| {
        entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    })
    .unwrap_or_else(|| panic!("{prefix}{suffix} not found next to the test executable"))
    .path()
}

fn is_stub_library(file_name: &str, prefix: &str, suffix: &str) -> bool {
    let Some(stem) = file_name
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
    else {
        return false;
    };
    stem.is_empty()
        || stem.strip_prefix('-').is_some_and(|hash| {
            !hash.is_empty() && hash.chars().all(|character| character.is_ascii_hexdigit())
        })
}

/// The stub's control exports, opened from the same file as the library under test so they
/// share the stub's state, plus a scratch directory for storage and the onboarding id.
struct Fixture {
    stub: libloading::Library,
    scratch: PathBuf,
    _serial: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: the stub has no initialization side effects.
        let stub = unsafe { libloading::Library::new(stub_path()) }.expect("open qaros-stub");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let scratch = std::env::temp_dir().join(format!("qaros-sink-test-{unique}"));
        std::fs::create_dir_all(&scratch).expect("scratch directory");
        let fixture = Self {
            stub,
            scratch,
            _serial: serial,
        };
        let reset: unsafe extern "C" fn() = fixture.symbol("qar_stub_reset");
        // SAFETY: no arguments.
        unsafe { reset() };
        fixture
    }

    fn symbol<T: Copy>(&self, name: &str) -> T {
        // SAFETY: the caller names a stub export with this signature.
        unsafe { *self.stub.get::<T>(name.as_bytes()).expect(name) }
    }

    fn calls(&self) -> Vec<String> {
        let call_log: unsafe extern "C" fn(*mut c_char, usize) -> usize =
            self.symbol("qar_stub_call_log");
        // SAFETY: a null buffer only queries the length.
        let length = unsafe { call_log(std::ptr::null_mut(), 0) };
        let mut buffer = vec![0 as c_char; length + 1];
        // SAFETY: the buffer holds length + 1 bytes.
        unsafe { call_log(buffer.as_mut_ptr(), buffer.len()) };
        let bytes: Vec<u8> = buffer
            .iter()
            .take_while(|&&character| character != 0)
            .map(|&character| character.to_ne_bytes()[0])
            .collect();
        String::from_utf8(bytes)
            .expect("UTF-8 log")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn calls_starting_with(&self, prefix: &str) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|call| call.starts_with(prefix))
            .collect()
    }

    fn live_handle_count(&self) -> usize {
        let count: unsafe extern "C" fn() -> usize = self.symbol("qar_stub_live_handle_count");
        // SAFETY: no arguments.
        unsafe { count() }
    }

    fn peer_join(&self, peer: PeerId, display_name: &str) {
        let join: unsafe extern "C" fn(*const u8, *const c_char) -> usize =
            self.symbol("qar_stub_peer_join");
        let name = CString::new(display_name).expect("display name");
        // SAFETY: 16 readable bytes and a NUL-terminated name.
        unsafe { join(peer.0.as_ptr(), name.as_ptr()) };
    }

    fn peer_leave(&self, peer: PeerId) {
        let leave: unsafe extern "C" fn(*const u8) = self.symbol("qar_stub_peer_leave");
        // SAFETY: 16 readable bytes.
        unsafe { leave(peer.0.as_ptr()) };
    }

    fn peer_retire(&self, peer: PeerId) {
        let retire: unsafe extern "C" fn(*const u8) = self.symbol("qar_stub_peer_retire");
        // SAFETY: 16 readable bytes.
        unsafe { retire(peer.0.as_ptr()) };
    }

    /// The peer's receiver pauses its stream (`paused`) or resumes it.
    fn set_peer_paused(&self, peer: PeerId, paused: bool) {
        let set_paused: unsafe extern "C" fn(*const u8, u8) =
            self.symbol("qar_stub_set_peer_paused");
        // SAFETY: 16 readable bytes and a plain flag.
        unsafe { set_paused(peer.0.as_ptr(), u8::from(paused)) };
    }

    /// The peer is listed as running but does not answer: its sender creations time
    /// out after `milliseconds`; 0 makes it answer again.
    fn set_peer_unanswering(&self, peer: PeerId, milliseconds: u64) {
        let set_unanswering: unsafe extern "C" fn(*const u8, u64) =
            self.symbol("qar_stub_set_peer_unanswering");
        // SAFETY: 16 readable bytes and a plain value.
        unsafe { set_unanswering(peer.0.as_ptr(), milliseconds) };
    }

    /// The peer is listed under `version_id` from now on, as an app restarted under its peer
    /// id, announced with a peer update.
    fn set_peer_version_id(&self, peer: PeerId, version_id: &str) {
        let set_version_id: unsafe extern "C" fn(*const u8, *const c_char) =
            self.symbol("qar_stub_set_peer_version_id");
        let version_id = CString::new(version_id).expect("version id");
        // SAFETY: 16 readable bytes and a NUL-terminated version id.
        unsafe { set_version_id(peer.0.as_ptr(), version_id.as_ptr()) };
    }

    /// How long the begin-frame wait `try_begin_frame` starts takes to end.
    fn set_try_begin_frame_delay(&self, milliseconds: u64) {
        let set_delay: extern "C" fn(u64) = self.symbol("qar_stub_set_try_begin_frame_delay_ms");
        set_delay(milliseconds);
    }

    /// Whether subscribing to render requests brings the visualizer's request at once.
    fn set_auto_render_request(&self, enabled: bool) {
        let set_auto: extern "C" fn(bool) = self.symbol("qar_stub_set_auto_render_request");
        set_auto(enabled);
    }

    fn senders_destroyed_during_a_begin_wait(&self) -> usize {
        self.calls_starting_with("render sender destroyed with a begin-frame wait pending")
            .len()
    }

    fn emit_render_request(&self, peer: PeerId) {
        let emit_request: unsafe extern "C" fn(*const u8) -> usize =
            self.symbol("qar_stub_emit_render_request");
        // SAFETY: 16 readable bytes.
        unsafe { emit_request(peer.0.as_ptr()) };
    }

    #[cfg(feature = "vulkan")]
    fn set_reported_adapter(&self, adapter: Option<qaros::GraphicsAdapter>) {
        let set_reported: unsafe extern "C" fn(bool, *const u8) =
            self.symbol("qar_stub_set_reported_adapter");
        // SAFETY: null, or 8 (LUID) or 16 (UUID) readable bytes.
        unsafe {
            match &adapter {
                None => set_reported(false, std::ptr::null()),
                Some(qaros::GraphicsAdapter::Luid(luid)) => set_reported(true, luid.as_ptr()),
                Some(qaros::GraphicsAdapter::Uuid(uuid)) => set_reported(false, uuid.as_ptr()),
            }
        }
    }

    /// Makes the next call to `function` (without `qar_`) fail with `code`.
    fn fail_next(&self, function: &str, code: i64) {
        let fail_next: unsafe extern "C" fn(*const c_char, i64) = self.symbol("qar_stub_fail_next");
        let function = CString::new(function).expect("function name");
        // SAFETY: a NUL-terminated name and a plain value.
        unsafe { fail_next(function.as_ptr(), code) };
    }

    fn set_render_sender_create_delay(&self, milliseconds: u64) {
        let set_delay: extern "C" fn(u64) =
            self.symbol("qar_stub_set_render_sender_create_delay_ms");
        set_delay(milliseconds);
    }

    fn set_render_sender_create_cancel_delay(&self, milliseconds: u64) {
        let set_delay: extern "C" fn(u64) =
            self.symbol("qar_stub_set_render_sender_create_cancel_delay_ms");
        set_delay(milliseconds);
    }

    fn frames_shown(&self, peer: PeerId) -> u64 {
        let shown: unsafe extern "C" fn(*const u8) -> u64 = self.symbol("qar_stub_frames_shown");
        // SAFETY: 16 readable bytes.
        unsafe { shown(peer.0.as_ptr()) }
    }

    fn connect(&self, pairing_code: Option<&str>) -> Result<Rc<QarosConnection>, String> {
        connect(
            &connect_options(&self.scratch, pairing_code),
            wake_counter().1,
        )
        .map(Rc::new)
    }

    /// Drops the connection last, as the viewer does, and checks the stub saw a clean
    /// teardown: no ordering or pairing errors, every handle released.
    fn assert_clean_teardown(&self, connection: Rc<QarosConnection>) {
        assert_eq!(
            Rc::strong_count(&connection),
            1,
            "sink and panels dropped first"
        );
        drop(connection);
        let errors = self.calls_starting_with("ERROR");
        assert!(errors.is_empty(), "{errors:?}");
        self.assert_volume_closed_after_its_contents_and_before_the_session();
        assert_eq!(self.live_handle_count(), 0, "every handle released");
    }

    /// In the latest teardown, the volume goes after every sender and panel in it, and before
    /// the session. The call log spans every session of the test.
    fn assert_volume_closed_after_its_contents_and_before_the_session(&self) {
        let calls = self.calls();
        let last_of = |name: &str| calls.iter().rposition(|call| call == name);
        let volume_closed = last_of("app_volumes_close_volume").expect("the app volume is closed");
        let session_destroyed =
            last_of("session_handle_destroy").expect("the session is destroyed");
        assert!(
            volume_closed < session_destroyed,
            "volume closed at call {volume_closed}, session destroyed at {session_destroyed}"
        );
        for contents in [
            "render_stream_handle_destroy",
            "gui_panels_close_panel",
            "gui_panels_set_state",
        ] {
            if let Some(last) = last_of(contents) {
                assert!(
                    last < volume_closed,
                    "{contents} at call {last} comes after the volume closed at {volume_closed}"
                );
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

fn placement() -> AppPlacement {
    AppPlacement {
        volume_pose: Pose::from_translation(Vec3::new(0.0, 0.5, -1.6)),
        app_scale: 2.0,
        ..AppPlacement::default()
    }
    .with_up_axis_rotation(std::f32::consts::FRAC_PI_2)
}

fn connect_options(scratch: &Path, pairing_code: Option<&str>) -> ConnectOptions {
    ConnectOptions {
        library_path: Some(stub_path()),
        runtime_binaries_directory: None,
        storage_directory: Some(scratch.join("storage")),
        onboarding_id_file: scratch.join("qaros-rerun.onboarding-id.txt"),
        unanswered_peers_file: Some(scratch.join(UNANSWERED_PEERS_FILE_NAME)),
        pairing_code: pairing_code.map(str::to_owned),
        launch_line: None,
        local_app: false,
        peer_display_name: "qaros-rerun test viewer".to_owned(),
        volume_name: "rerun".to_owned(),
        volume_display_name: "qaros-rerun test".to_owned(),
        volume_size: [2.0, 2.0, 1.5],
        placement: placement(),
        console_logging: false,
        raise_gpu_priority: false,
        adapter: None,
    }
}

fn depth_range() -> DepthRange {
    DepthRange::new(0.05, 20.0, DepthEncoding::ForwardNormalized).unwrap()
}

fn show_info() -> ShowInfo {
    ShowInfo::from_depth_range(depth_range())
}

fn sink_options(max_users: usize) -> QarosSinkOptions {
    QarosSinkOptions {
        depth_range: depth_range(),
        pose_space: PoseSpace::Room,
        color_format: PixelFormat::B8G8R8A8,
        max_users,
        stop_after_frames: None,
    }
}

fn new_sink(connection: &Rc<QarosConnection>, pose_space: PoseSpace) -> QarosSink {
    QarosSink::new(
        Rc::clone(connection),
        QarosSinkOptions {
            pose_space,
            stop_after_frames: Some(2),
            ..sink_options(2)
        },
    )
}

fn eye_image(resolution_in_pixel: [u32; 2]) -> EyeImage {
    let pixel_count = resolution_in_pixel[0] as usize * resolution_in_pixel[1] as usize;
    EyeImage {
        resolution_in_pixel,
        color_rgba8: [200, 100, 50, 255].repeat(pixel_count),
        depth: vec![0.5; pixel_count],
    }
}

fn stamped_eye(key: FrameKey, eye_index: usize) -> EyeImage {
    let mut image = eye_image([64, 64]);
    stamp_eye(
        &mut image,
        FrameStamp {
            user: key.user,
            frame_id: key.frame_id,
            eye_index,
        },
        0.5,
    );
    image
}

fn wake_counter() -> (Arc<AtomicUsize>, WakeViewer) {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let wake: WakeViewer = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    (wakes, wake)
}

fn submit_both_eyes(sink: &mut QarosSink, key: FrameKey) {
    for eye_index in 0..2 {
        sink.submit_view(key, eye_index, stamped_eye(key, eye_index))
            .unwrap();
    }
}

/// The only request of the next `begin_frames`, as with one user and blocking begins.
/// The next frame request, after any sender still being created has arrived: senders are
/// created in the background and picked up by a later `begin_frames`.
fn single_request(sink: &mut QarosSink) -> Option<FrameRequest> {
    let started = Instant::now();
    loop {
        let mut requests = sink.begin_frames().unwrap();
        assert!(requests.len() <= 1, "{requests:?}");
        if !requests.is_empty() || !sink.is_creating_senders() {
            return requests.pop();
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a sender still being created after 5 s"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Plays the renderer: every viewer frame, begins, stamps, submits and shows each user's
/// frame, until `done` holds. Returns every request shown.
fn run_viewer_frames(
    sink: &mut QarosSink,
    mut between_frames: impl FnMut(&mut QarosSink, &[FrameRequest]),
    done: impl Fn(&QarosSink, &[FrameRequest]) -> bool,
) -> Vec<FrameRequest> {
    let started = Instant::now();
    let mut shown = Vec::new();
    while !done(sink, &shown) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "gave up after {} shown frames",
            shown.len()
        );
        for request in sink.begin_frames().unwrap() {
            submit_both_eyes(sink, request.key);
            sink.show_frame(request.key, show_info()).unwrap();
            shown.push(request);
        }
        between_frames(sink, &shown);
        std::thread::sleep(Duration::from_millis(1));
    }
    shown
}

fn frames_of(shown: &[FrameRequest], user: UserId) -> Vec<&FrameRequest> {
    shown
        .iter()
        .filter(|request| request.key.user == user)
        .collect()
}

#[test]
fn onboards_streams_and_tears_down_in_order() {
    let fixture = Fixture::new();
    let missing_code = fixture.connect(None);
    assert!(
        missing_code
            .as_ref()
            .is_err_and(|error| error.contains("--pairing-code")),
        "{missing_code:?}"
    );

    let (wakes, wake) = wake_counter();
    let connection =
        Rc::new(connect(&connect_options(&fixture.scratch, Some("ABCD-EFGH")), wake).unwrap());
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        1,
        "the request woke the viewer"
    );
    assert!(
        fixture
            .scratch
            .join("qaros-rerun.onboarding-id.txt")
            .exists()
    );

    let mut sink = new_sink(&connection, PoseSpace::Room);
    assert!(
        !sink.is_streaming() && !sink.is_finished(),
        "idle until the first poll finds a user"
    );
    let request = single_request(&mut sink).expect("a frame for the visualizer");
    assert!(sink.is_streaming());
    assert_eq!(request.key.user, UserId(1));
    assert_eq!(request.resolution_in_pixel, [64, 64]);
    assert!((request.views[0].pose.position - Vec3::new(-0.032, 1.6, 0.0)).length() < 1e-6);
    assert!((request.views[1].pose.position - Vec3::new(0.032, 1.6, 0.0)).length() < 1e-6);
    assert!((request.views[0].fov.angle_left + 0.785).abs() < 1e-3);
    assert!(single_request(&mut sink).is_none(), "one frame in flight");

    submit_both_eyes(&mut sink, request.key);
    assert!(
        sink.submit_view(request.key, 2, eye_image([64, 64]))
            .is_err()
    );
    assert!(
        sink.submit_view(request.key, 0, eye_image([32, 64]))
            .is_err()
    );
    sink.show_frame(request.key, show_info()).unwrap();
    assert_eq!(sink.shown_frame_count(), 1);

    let abandoned = single_request(&mut sink).expect("second frame");
    sink.abandon_frame(abandoned.key);
    assert!(sink.show_frame(abandoned.key, show_info()).is_err());
    let incomplete = single_request(&mut sink).expect("third frame");
    sink.submit_view(incomplete.key, 0, stamped_eye(incomplete.key, 0))
        .unwrap();
    assert!(
        sink.show_frame(incomplete.key, show_info()).is_err(),
        "a frame with one eye is not shown"
    );
    let last = single_request(&mut sink).expect("fourth frame");
    assert_eq!(
        last.key.frame_id, 3,
        "abandoned frames count, as in the stub"
    );
    submit_both_eyes(&mut sink, last.key);
    sink.show_frame(last.key, show_info()).unwrap();
    assert!(!sink.is_streaming(), "stops after two shown frames");
    assert!(sink.is_finished());
    assert!(single_request(&mut sink).is_none());

    let calls = fixture.calls();
    assert!(
        calls
            .iter()
            .any(|call| call == "shown near=0.05 far=20 depth_scale=1")
    );
    assert_eq!(
        fixture.calls_starting_with("paired"),
        [
            "paired peer=56565656 frame=0 stamped=0",
            "paired peer=56565656 frame=3 stamped=3",
        ]
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_local_app_joins_without_a_code_and_stores_no_id() {
    let fixture = Fixture::new();
    let (_wakes, wake) = wake_counter();
    let options = ConnectOptions {
        local_app: true,
        ..connect_options(&fixture.scratch, None)
    };
    let without_local_app = ConnectOptions {
        local_app: false,
        ..options.clone()
    };
    assert!(
        connect(&without_local_app, wake_counter().1).is_err(),
        "without --local-app and a code there is no way in"
    );
    let connection = Rc::new(connect(&options, wake).unwrap());
    assert!(
        !fixture
            .scratch
            .join("qaros-rerun.onboarding-id.txt")
            .exists(),
        "local-app onboarding is stateless"
    );
    assert_eq!(
        fixture.calls_starting_with("local app approval_timeout_ms="),
        vec!["local app approval_timeout_ms=0".to_owned()],
        "the onboarding chained the local-app extension, leaving the timeout to the Hub"
    );

    let mut sink = new_sink(&connection, PoseSpace::Room);
    let request = single_request(&mut sink).expect("a frame for the visualizer");
    assert_eq!(request.key.user, UserId(1));
    assert_eq!(request.resolution_in_pixel, [64, 64]);
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn targets_are_found_in_the_peer_list_without_render_requests() {
    let fixture = Fixture::new();
    let set_auto_request: unsafe extern "C" fn(bool) =
        fixture.symbol("qar_stub_set_auto_render_request");
    // SAFETY: plain value argument.
    unsafe { set_auto_request(false) };
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    assert!(
        connection.take_render_requests().is_empty(),
        "the visualizer asks for nothing"
    );
    let mut sink = new_sink(&connection, PoseSpace::Room);
    let request = single_request(&mut sink).expect("a frame for the listed visualizer");
    assert_eq!(request.key.user, UserId(1));
    assert_eq!(
        sink.rendered_users()
            .iter()
            .map(|(user, peer)| (user.label.as_str(), *peer))
            .collect::<Vec<_>>(),
        [("Stub Visualizer", VISUALIZER_PEER)],
        "the visualizer, a running target app; not this application's own peer"
    );
    submit_both_eyes(&mut sink, request.key);
    sink.show_frame(request.key, show_info()).unwrap();

    fixture.peer_leave(VISUALIZER_PEER);
    assert!(
        single_request(&mut sink).is_none(),
        "a target that stops running is released"
    );
    assert!(sink.rendered_users().is_empty());
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn the_connection_knows_each_user_peer_and_reads_peer_devices_from_the_peer_list() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    assert_eq!(connection.peer_of_user(UserId(1)), None);
    let mut sink = new_sink(&connection, PoseSpace::Room);
    let request = single_request(&mut sink).expect("a frame for the listed visualizer");
    assert_eq!(request.key.user, UserId(1));
    assert_eq!(connection.peer_of_user(UserId(1)), Some(VISUALIZER_PEER));

    assert_eq!(
        connection.peer_device(PeerId([0x99; 16])),
        Ok(None),
        "a peer the list does not hold"
    );
    let unreadable = connection
        .peer_device(VISUALIZER_PEER)
        .expect_err("qaros-stub implements no device category");
    assert!(
        unreadable.starts_with("qar_peer_spec_get_category: ")
            && unreadable.contains("not implemented in qaros-stub"),
        "{unreadable}"
    );
    submit_both_eyes(&mut sink, request.key);
    sink.show_frame(request.key, show_info()).unwrap();
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A peer can ask for our stream without being in the peer list. It stays rendered across
/// peer list reads instead of leaving at each read and joining again at its next request.
#[test]
fn an_unlisted_peer_that_requests_a_stream_keeps_it_across_peer_list_reads() {
    const UNLISTED_MIXER: PeerId = PeerId([0xbf; 16]);
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(4));
    let emit_request: unsafe extern "C" fn(*const u8) -> usize =
        fixture.symbol("qar_stub_emit_render_request");
    // SAFETY: 16 readable bytes.
    unsafe { emit_request(UNLISTED_MIXER.0.as_ptr()) };
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, UNLISTED_MIXER).is_some(),
    );
    let first = sink_users_by_peer(&sink, UNLISTED_MIXER);

    // The headset's join is a peer update, which makes the sink read the peer list, where
    // the requester is missing.
    fixture.peer_join(HEADSET_A, "Stub Headset A");
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, HEADSET_A).is_some(),
    );
    assert_eq!(
        sink_users_by_peer(&sink, UNLISTED_MIXER),
        first,
        "the unlisted requester keeps its user after a peer list read"
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A headset whose sender is slow to arrive does not hold up the frames of a user already
/// streaming: creation runs in the background instead of blocking the viewer's frame.
#[test]
fn a_slow_sender_creation_does_not_stall_the_other_users_frames() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, VISUALIZER_PEER).is_some(),
    );

    fixture.set_render_sender_create_delay(600);
    fixture.peer_join(HEADSET_A, "Stub Headset A");
    let started = Instant::now();
    let shown = run_viewer_frames(&mut sink, |_, _| {}, |_, shown| shown.len() >= 20);
    let elapsed = started.elapsed();
    fixture.set_render_sender_create_delay(0);

    let visualizer = sink_users_by_peer(&sink, VISUALIZER_PEER).expect("still rendered");
    assert_eq!(
        frames_of(&shown, visualizer).len(),
        20,
        "every frame went to the visualizer"
    );
    assert!(
        sink.is_creating_senders(),
        "the headset's sender is still on its way"
    );
    assert_eq!(sink_users_by_peer(&sink, HEADSET_A), None);
    assert!(
        elapsed < Duration::from_millis(400),
        "20 frames took {elapsed:?} while a creation was slow"
    );
    // Let the creation land, so the teardown has nothing unanswered.
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, HEADSET_A).is_some(),
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A user whose peer leaves while its sender is still being created is released at once; its
/// creation runs on, and the sender that arrives later is destroyed, never used for anybody.
#[test]
fn a_peer_that_leaves_while_its_sender_is_created_gets_no_stream() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, VISUALIZER_PEER).is_some(),
    );
    // A creation that still delivers its sender after the cancel, as the runtime may.
    let ignores_cancel: extern "C" fn(u8) =
        fixture.symbol("qar_stub_set_render_sender_create_ignores_cancel");
    ignores_cancel(1);
    fixture.set_render_sender_create_delay(300);
    fixture.peer_join(HEADSET_A, "Stub Headset A");
    run_viewer_frames(&mut sink, |_, _| {}, |sink, _| sink.is_creating_senders());
    fixture.set_render_sender_create_delay(0);

    fixture.peer_leave(HEADSET_A);
    run_viewer_frames(&mut sink, |_, _| {}, |sink, _| !sink.is_creating_senders());
    assert_eq!(sink_users_by_peer(&sink, HEADSET_A), None);
    // The cancelled creation's late sender, then a few frames that release it.
    std::thread::sleep(Duration::from_millis(400));
    ignores_cancel(0);
    let shown = run_viewer_frames(&mut sink, |_, _| {}, |_, shown| shown.len() >= 5);
    assert!(
        shown
            .iter()
            .all(|request| Some(request.key.user) == sink_users_by_peer(&sink, VISUALIZER_PEER)),
        "only the visualizer gets frames"
    );
    let calls = fixture.calls();
    let delivered = calls
        .iter()
        .rposition(|call| call == "render_sender_create_async delivered")
        .expect("the late sender was delivered");
    assert!(
        calls[delivered..]
            .iter()
            .any(|call| call == "render_stream_handle_destroy"),
        "the viewer's frames destroyed the late sender, before any teardown"
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A Stop while a target's sender is still being created (a headset listed as running that
/// does not answer): the sink's drop cancels the creation, which calls back shortly after, and
/// the connection's drop then leaves the session cleanly instead of leaking it with the library.
#[test]
fn a_stop_while_a_sender_is_created_still_destroys_the_session() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, VISUALIZER_PEER).is_some(),
    );
    fixture.set_render_sender_create_delay(10_000);
    fixture.set_render_sender_create_cancel_delay(50);
    fixture.peer_join(HEADSET_A, "Stub Headset A");
    run_viewer_frames(&mut sink, |_, _| {}, |sink, _| sink.is_creating_senders());

    drop(sink);
    fixture.assert_clean_teardown(connection);
    let calls = fixture.calls();
    let cancelled = calls
        .iter()
        .position(|call| call == "render_sender_create_async cancelled")
        .expect("the headset's creation called back cancelled");
    let session_destroyed = calls
        .iter()
        .position(|call| call == "session_handle_destroy")
        .expect("the session is destroyed");
    assert!(cancelled < session_destroyed, "{calls:?}");
}

/// A creation that fails through its callback releases its user and puts the target on its
/// back-off; it joins again and streams once the back-off has passed.
#[test]
fn a_creation_that_fails_in_its_callback_backs_off_and_retries() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    fixture.fail_next("render_sender_create_async_callback", UNCLASSIFIED);
    sink.begin_frames().unwrap();
    run_viewer_frames(&mut sink, |_, _| {}, |sink, _| !sink.is_creating_senders());
    assert_eq!(
        sink_users_by_peer(&sink, VISUALIZER_PEER),
        None,
        "the failed creation released the visualizer"
    );
    assert_eq!(
        fixture
            .calls_starting_with("render_sender_create_async failed")
            .len(),
        1
    );

    let retried = Instant::now();
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, VISUALIZER_PEER).is_some(),
    );
    assert!(
        retried.elapsed() >= Duration::from_millis(1_500),
        "rejoined after {:?}, before its 2 s back-off",
        retried.elapsed()
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A creation in flight holds no slot: a target that joins later and answers takes the only
/// one. A creation that does not answer within the deadline is given up and its target held
/// back as unanswered, instead of being waited for for good.
#[test]
fn a_slow_creation_holds_no_slot_and_one_past_its_deadline_is_given_up() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(1));
    sink.set_sender_creation_deadline(Duration::from_millis(200));

    fixture.set_render_sender_create_delay(3_000);
    sink.begin_frames().unwrap();
    assert!(
        sink.is_creating_senders(),
        "the visualizer's creation is slow"
    );
    fixture.set_render_sender_create_delay(0);
    fixture.peer_join(HEADSET_A, "Stub Headset A");
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, HEADSET_A).is_some(),
    );
    let waiting: Vec<String> = sink
        .waiting_users()
        .into_iter()
        .map(|user| user.label)
        .collect();
    assert_eq!(waiting, ["Stub Visualizer"], "its creation still runs");

    run_viewer_frames(&mut sink, |_, _| {}, |sink, _| !sink.is_creating_senders());
    assert_eq!(
        sink_users_by_peer(&sink, VISUALIZER_PEER),
        None,
        "the visualizer was given up"
    );
    assert!(sink.waiting_users().is_empty());
    assert!(
        unanswered_peers_file_text(&fixture).contains(&VISUALIZER_PEER.to_string()),
        "held back as unanswered"
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

fn unanswered_peers_file_text(fixture: &Fixture) -> String {
    let file = fixture.scratch.join(UNANSWERED_PEERS_FILE_NAME);
    match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => panic!("{}: {error}", file.display()),
    }
}

/// Without panels the viewer's sink holds the last reference to the connection; its senders
/// still go before the volume closes.
#[test]
fn a_sink_holding_the_last_connection_closes_the_volume_after_its_senders() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = new_sink(&connection, PoseSpace::Room);
    single_request(&mut sink).expect("a sender exists");
    drop(connection);
    drop(sink);
    let errors = fixture.calls_starting_with("ERROR");
    assert!(errors.is_empty(), "{errors:?}");
    fixture.assert_volume_closed_after_its_contents_and_before_the_session();
    assert_eq!(fixture.live_handle_count(), 0, "every handle released");
}

fn left(events: &[PeerEvent]) -> Vec<PeerId> {
    events
        .iter()
        .filter_map(|event| match event {
            PeerEvent::Left { peer } => Some(*peer),
            PeerEvent::Joined { .. } | PeerEvent::AskedForAStream { .. } => None,
        })
        .collect()
}

fn joined(events: &[PeerEvent]) -> Vec<PeerId> {
    events
        .iter()
        .filter_map(|event| match event {
            PeerEvent::Joined { peer, .. } => Some(*peer),
            PeerEvent::Left { .. } | PeerEvent::AskedForAStream { .. } => None,
        })
        .collect()
}

/// Calls `leave_stalled` every `interval` until it reports someone, as the sink does before
/// each begin, with no user waiting for a slot; `begun` is told about each round, to play the
/// frames that still begin.
fn leave_stalled_until_someone_leaves(
    peers: &mut TargetPeers,
    interval: Duration,
    mut begun: impl FnMut(&mut TargetPeers),
) -> (Vec<PeerId>, Duration) {
    let started = Instant::now();
    loop {
        begun(peers);
        let gone = left(&peers.leave_stalled(false));
        if !gone.is_empty() {
            return (gone, started.elapsed());
        }
        assert!(started.elapsed() < Duration::from_secs(5), "nobody stalled");
        std::thread::sleep(interval);
    }
}

/// A requester outside the peer list that asks for no frame in the stall timeout is taken for
/// gone. A listed target that stays listed as running is kept then (a paused stream), until a
/// user waits for its slot. Only the time spent asking for frames counts. A listed target taken
/// for gone is not joined again by the unchanged listing; a render request of its own brings it
/// back.
#[test]
fn a_stalled_requester_is_taken_for_gone_and_a_listed_target_only_for_a_waiting_user() {
    const UNLISTED_MIXER: PeerId = PeerId([0xbf; 16]);
    const STALL_TIMEOUT: Duration = Duration::from_millis(200);
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut peers = TargetPeers::new(Rc::clone(&connection)).with_stall_timeout(STALL_TIMEOUT);
    assert_eq!(joined(&peers.poll_events()), [VISUALIZER_PEER]);
    fixture.emit_render_request(UNLISTED_MIXER);
    assert_eq!(joined(&peers.poll_events()), [UNLISTED_MIXER]);
    peers.stream_started(VISUALIZER_PEER);
    peers.stream_started(UNLISTED_MIXER);

    // No frames asked for (no 3D view drawn, a blocked viewer) is not held against them.
    std::thread::sleep(STALL_TIMEOUT * 3);
    assert_eq!(left(&peers.leave_stalled(true)), []);

    let (gone, after) =
        leave_stalled_until_someone_leaves(&mut peers, Duration::from_millis(20), |peers| {
            peers.frame_begun(VISUALIZER_PEER);
        });
    assert_eq!(
        gone,
        [UNLISTED_MIXER],
        "the visualizer still asks for frames"
    );
    // The pause counted its first half stall timeout (the pause threshold), so the rest.
    assert!(after >= STALL_TIMEOUT / 2, "taken for gone after {after:?}");

    // Slow checks, each longer than the pause threshold, still count part of every gap; with
    // nobody waiting the stalled visualizer keeps its stream.
    let kept_since = Instant::now();
    while kept_since.elapsed() < STALL_TIMEOUT * 5 {
        assert_eq!(
            left(&peers.leave_stalled(false)),
            [],
            "nobody waits for its slot"
        );
        std::thread::sleep(Duration::from_millis(150));
    }
    assert_eq!(left(&peers.leave_stalled(true)), [VISUALIZER_PEER]);

    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(2_500) {
        assert_eq!(
            joined(&peers.poll_events()),
            [],
            "the unchanged running listing does not bring it back"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    fixture.emit_render_request(VISUALIZER_PEER);
    assert_eq!(joined(&peers.poll_events()), [VISUALIZER_PEER]);
    drop(peers);
    fixture.assert_clean_teardown(connection);
}

/// The peer list as `TargetPeers` reports it, with stalls on demand.
struct StallsOnDemand {
    peers: TargetPeers,
    stalls: Rc<std::cell::RefCell<Vec<PeerId>>>,
}

impl UserEventSource for StallsOnDemand {
    fn poll_events(&mut self) -> Vec<PeerEvent> {
        self.peers.poll_events()
    }

    fn forget(&mut self, peer: PeerId, cause: ReleaseCause) {
        self.peers.forget(peer, cause);
    }

    fn leave_stalled(&mut self, _slot_wanted: bool) -> Vec<PeerEvent> {
        self.stalls
            .borrow_mut()
            .drain(..)
            .map(|peer| PeerEvent::Left { peer })
            .collect()
    }
}

/// A receiver that pauses its stream for longer than the stall timeout (a headset taken off, a
/// minimised visualizer) keeps its sender while nobody waits, and its frames resume at the next
/// begin. Paused again with a user waiting for the slot, it is released and the waiting user
/// gets the slot; its sender goes once its begin-frame wait ends.
#[test]
fn a_paused_stream_keeps_its_sender_until_a_user_waits_for_its_slot() {
    const STALL_TIMEOUT: Duration = Duration::from_millis(200);
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::with_user_events(
        Rc::clone(&connection),
        sink_options(1),
        Box::new(TargetPeers::new(Rc::clone(&connection)).with_stall_timeout(STALL_TIMEOUT)),
        Box::new(MaxUsers { maximum: 1 }),
    );
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| sink_users_by_peer(sink, VISUALIZER_PEER).is_some() && !shown.is_empty(),
    );
    let visualizer = sink_users_by_peer(&sink, VISUALIZER_PEER).unwrap();
    let created_before = count_calls(&fixture, "render_sender_create_async");
    let destroyed_before = count_calls(&fixture, "render_stream_handle_destroy");

    fixture.set_peer_paused(VISUALIZER_PEER, true);
    let paused_since = Instant::now();
    while paused_since.elapsed() < STALL_TIMEOUT * 5 {
        assert!(
            sink.begin_frames().unwrap().is_empty(),
            "no frame while paused"
        );
        assert_eq!(sink_users_by_peer(&sink, VISUALIZER_PEER), Some(visualizer));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        count_calls(&fixture, "render_stream_handle_destroy"),
        destroyed_before,
        "the paused sender stays"
    );

    fixture.set_peer_paused(VISUALIZER_PEER, false);
    let resumed = sink.begin_frames().unwrap();
    let resumed_users: Vec<UserId> = resumed.iter().map(|request| request.key.user).collect();
    assert_eq!(
        resumed_users,
        [visualizer],
        "frames resume at the next begin"
    );
    for request in resumed {
        submit_both_eyes(&mut sink, request.key);
        sink.show_frame(request.key, show_info()).unwrap();
    }
    assert_eq!(
        count_calls(&fixture, "render_sender_create_async"),
        created_before,
        "no new sender"
    );

    fixture.set_peer_paused(VISUALIZER_PEER, true);
    fixture.peer_join(HEADSET_A, "Stub Headset A");
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| {
            sink_users_by_peer(sink, HEADSET_A)
                .is_some_and(|headset| shown.iter().any(|request| request.key.user == headset))
        },
    );
    assert_eq!(sink_users_by_peer(&sink, VISUALIZER_PEER), None);
    assert_eq!(
        count_calls(&fixture, "render_stream_handle_destroy"),
        destroyed_before,
        "the paused sender gives up its slot, parked while its begin-frame wait runs"
    );
    fixture.set_peer_paused(VISUALIZER_PEER, false);
    sink.update_users();
    assert_eq!(
        count_calls(&fixture, "render_stream_handle_destroy"),
        destroyed_before + 1,
        "it goes once the receiver resumes and the wait ends"
    );
    assert_eq!(fixture.senders_destroyed_during_a_begin_wait(), 0);
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A stalled target's sender goes at the next `begin_frames`; user updates without begins do
/// not check for stalls, since nobody asks the target for frames then.
#[test]
fn a_stalled_target_is_released_by_the_next_begin_not_by_user_updates() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let stalls = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut sink = QarosSink::with_user_events(
        Rc::clone(&connection),
        sink_options(2),
        Box::new(StallsOnDemand {
            peers: TargetPeers::new(Rc::clone(&connection)),
            stalls: Rc::clone(&stalls),
        }),
        Box::new(MaxUsers { maximum: 2 }),
    );
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| sink_users_by_peer(sink, VISUALIZER_PEER).is_some() && !shown.is_empty(),
    );

    stalls.borrow_mut().push(VISUALIZER_PEER);
    sink.update_users();
    assert!(sink_users_by_peer(&sink, VISUALIZER_PEER).is_some());
    assert_eq!(*stalls.borrow(), [VISUALIZER_PEER], "not asked for stalls");

    let destroyed_before = fixture
        .calls_starting_with("render_stream_handle_destroy")
        .len();
    assert!(sink.begin_frames().unwrap().is_empty());
    assert_eq!(sink_users_by_peer(&sink, VISUALIZER_PEER), None);
    assert_eq!(
        fixture
            .calls_starting_with("render_stream_handle_destroy")
            .len(),
        destroyed_before + 1
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

fn count_calls(fixture: &Fixture, call: &str) -> usize {
    fixture
        .calls()
        .iter()
        .filter(|recorded| *recorded == call)
        .count()
}

/// A creation the runtime times out (a peer listed as running that does not answer) holds the target
/// back: during the startup window its one background retry, 2 s later, finds the only slot
/// taken by the live headset and starts no creation; the peer list re-read meanwhile does not
/// bring it back, a render request from it does.
#[test]
fn a_creation_that_times_out_holds_the_target_back_until_it_asks_for_a_stream() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(1));
    fixture.fail_next("render_sender_create_async_callback", TIMEOUT);
    sink.begin_frames().unwrap();
    run_viewer_frames(&mut sink, |_, _| {}, |sink, _| !sink.is_creating_senders());
    assert_eq!(sink_users_by_peer(&sink, VISUALIZER_PEER), None);

    fixture.peer_join(HEADSET_A, "Stub Headset A");
    let started = Instant::now();
    run_viewer_frames(
        &mut sink,
        |sink, _| {
            assert_eq!(
                sink_users_by_peer(sink, VISUALIZER_PEER),
                None,
                "the timed-out target came back"
            );
        },
        |_, _| started.elapsed() >= Duration::from_secs(3),
    );
    assert!(
        sink_users_by_peer(&sink, HEADSET_A).is_some(),
        "the live headset has the only slot"
    );
    let waiting: Vec<String> = sink
        .waiting_users()
        .into_iter()
        .map(|user| user.label)
        .collect();
    assert_eq!(
        waiting,
        ["Stub Visualizer"],
        "its background retry waits for a slot behind the headset"
    );
    assert_eq!(
        count_calls(&fixture, "render_sender_create_async"),
        2,
        "one creation for the visualizer, one for the headset"
    );

    fixture.peer_leave(HEADSET_A);
    fixture.emit_render_request(VISUALIZER_PEER);
    let requested = Instant::now();
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, VISUALIZER_PEER).is_some(),
    );
    assert!(requested.elapsed() < Duration::from_secs(1));
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A live receiver listed behind unanswering ones (listed as running) streams at
/// once: their creations run out in the background instead of holding the two slots for their
/// timeouts. In the next run the dead ones are held back, no creation started for them before
/// their retry period, until one asks for a stream, which admits it at once.
#[test]
fn a_live_target_listed_behind_dead_ones_streams_at_once_and_the_next_run_skips_them() {
    const DEAD_RECEIVER: PeerId = PeerId([0xd1; 16]);
    const DEAD_TIMEOUT_MILLISECONDS: u64 = 1_500;
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    fixture.peer_join(DEAD_RECEIVER, "Gone Receiver");
    fixture.peer_join(HEADSET_A, "Stub Headset A");
    for dead in [VISUALIZER_PEER, DEAD_RECEIVER] {
        fixture.set_peer_unanswering(dead, DEAD_TIMEOUT_MILLISECONDS);
    }
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    let started = Instant::now();
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| {
            sink_users_by_peer(sink, HEADSET_A)
                .is_some_and(|headset| !frames_of(shown, headset).is_empty())
        },
    );
    let live_after = started.elapsed();
    assert!(
        live_after < Duration::from_millis(DEAD_TIMEOUT_MILLISECONDS / 2),
        "the live headset streamed after {live_after:?}, behind dead targets"
    );
    assert!(sink.is_creating_senders(), "the dead creations still run");

    run_viewer_frames(&mut sink, |_, _| {}, |sink, _| !sink.is_creating_senders());
    let rendered: Vec<PeerId> = sink
        .rendered_users()
        .into_iter()
        .map(|(_, peer)| peer)
        .collect();
    assert_eq!(rendered, [HEADSET_A]);
    assert_eq!(
        fixture
            .calls_starting_with("render_sender_create_async timed out")
            .len(),
        2
    );
    let remembered = unanswered_peers_file_text(&fixture);
    for dead in [VISUALIZER_PEER, DEAD_RECEIVER] {
        assert!(remembered.contains(&dead.to_string()), "{remembered:?}");
    }
    assert!(!remembered.contains(&HEADSET_A.to_string()));
    drop(sink);
    fixture.assert_clean_teardown(connection);

    // Its own request would lift the visualizer's hold.
    fixture.set_auto_render_request(false);
    let connection = fixture.connect(None).unwrap();
    let creations_before = count_calls(&fixture, "render_sender_create_async");
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    let next_start = Instant::now();
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| {
            sink_users_by_peer(sink, HEADSET_A)
                .is_some_and(|headset| !frames_of(shown, headset).is_empty())
                && next_start.elapsed() >= Duration::from_millis(300)
        },
    );
    assert_eq!(
        count_calls(&fixture, "render_sender_create_async"),
        creations_before + 1,
        "only the live headset is tried"
    );
    assert!(sink.waiting_users().is_empty());

    fixture.set_peer_unanswering(DEAD_RECEIVER, 0);
    fixture.emit_render_request(DEAD_RECEIVER);
    let requested = Instant::now();
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, DEAD_RECEIVER).is_some(),
    );
    assert!(
        requested.elapsed() < Duration::from_secs(1),
        "its request lifted the hold"
    );
    assert!(!unanswered_peers_file_text(&fixture).contains(&DEAD_RECEIVER.to_string()));
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// Senders created together go to the peers that asked for a stream first: with one slot, a
/// requester outside the peer list gets it ahead of a listed target, which waits.
#[test]
fn a_target_that_asked_for_a_stream_takes_the_slot_ahead_of_a_listed_one() {
    const UNLISTED_RECEIVER: PeerId = PeerId([0xc3; 16]);
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(1));
    fixture.set_render_sender_create_delay(100);
    fixture.emit_render_request(UNLISTED_RECEIVER);
    sink.update_users();
    let waiting: Vec<String> = sink
        .waiting_users()
        .into_iter()
        .map(|user| user.label)
        .collect();
    assert_eq!(
        waiting,
        [UNLISTED_RECEIVER.to_string(), "Stub Visualizer".to_owned()],
        "the requester first"
    );
    assert_eq!(
        count_calls(&fixture, "render_sender_create_async"),
        2,
        "both tried at once"
    );

    std::thread::sleep(Duration::from_millis(400));
    fixture.set_render_sender_create_delay(0);
    sink.update_users();
    assert!(sink_users_by_peer(&sink, UNLISTED_RECEIVER).is_some());
    assert_eq!(sink_users_by_peer(&sink, VISUALIZER_PEER), None);
    let waiting: Vec<String> = sink
        .waiting_users()
        .into_iter()
        .map(|user| user.label)
        .collect();
    assert_eq!(waiting, ["Stub Visualizer"], "answered, but no slot");
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A receiver that pauses for longer than the stall timeout is taken for gone; its resume
/// comes with a render request, which admits it again at once, without the back-off of a
/// target that did not answer. A stall is no creation timeout and is not remembered. The
/// released sender, whose begin-frame wait runs while the receiver is paused, is not destroyed
/// during that wait: it goes once the receiver resumes and the wait ends.
#[test]
fn a_paused_receiver_taken_for_gone_is_admitted_at_once_by_its_render_request() {
    const UNLISTED_RECEIVER: PeerId = PeerId([0xc4; 16]);
    const STALL_TIMEOUT: Duration = Duration::from_millis(200);
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::with_user_events(
        Rc::clone(&connection),
        sink_options(2),
        Box::new(TargetPeers::new(Rc::clone(&connection)).with_stall_timeout(STALL_TIMEOUT)),
        Box::new(MaxUsers { maximum: 2 }),
    );
    fixture.emit_render_request(UNLISTED_RECEIVER);
    let streams_to_the_receiver = |sink: &QarosSink, shown: &[FrameRequest]| {
        sink_users_by_peer(sink, UNLISTED_RECEIVER)
            .is_some_and(|receiver| !frames_of(shown, receiver).is_empty())
    };
    run_viewer_frames(&mut sink, |_, _| {}, streams_to_the_receiver);

    fixture.set_peer_paused(UNLISTED_RECEIVER, true);
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, _| sink_users_by_peer(sink, UNLISTED_RECEIVER).is_none(),
    );
    let destroyed_at_release = count_calls(&fixture, "render_stream_handle_destroy");
    sink.update_users();
    assert_eq!(
        count_calls(&fixture, "render_stream_handle_destroy"),
        destroyed_at_release,
        "the released sender waits for its begin-frame wait to end"
    );

    fixture.set_peer_paused(UNLISTED_RECEIVER, false);
    fixture.emit_render_request(UNLISTED_RECEIVER);
    let resumed = Instant::now();
    run_viewer_frames(&mut sink, |_, _| {}, streams_to_the_receiver);
    assert!(
        resumed.elapsed() < Duration::from_secs(1),
        "streamed again {:?} after its request",
        resumed.elapsed()
    );
    assert_eq!(
        count_calls(&fixture, "render_stream_handle_destroy"),
        destroyed_at_release + 1,
        "the released sender went once its wait ended"
    );
    assert!(!unanswered_peers_file_text(&fixture).contains(&UNLISTED_RECEIVER.to_string()));
    drop(sink);
    assert_eq!(
        fixture.senders_destroyed_during_a_begin_wait(),
        0,
        "no sender destroyed with its begin-frame wait pending"
    );
    fixture.assert_clean_teardown(connection);
}

/// A sink whose visualizer stream was released (taken as stalled) while a begin-frame wait of
/// the stub's `try_begin_frame` delay ran, its sender parked.
fn sink_with_a_parked_sender(fixture: &Fixture, connection: &Rc<QarosConnection>) -> QarosSink {
    let stalls = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut sink = QarosSink::with_user_events(
        Rc::clone(connection),
        sink_options(1),
        Box::new(StallsOnDemand {
            peers: TargetPeers::new(Rc::clone(connection)),
            stalls: Rc::clone(&stalls),
        }),
        Box::new(MaxUsers { maximum: 1 }),
    );
    stream_and_leave_a_begin_waiting(fixture, &mut sink);
    let destroyed_before = count_calls(fixture, "render_stream_handle_destroy");
    stalls.borrow_mut().push(VISUALIZER_PEER);
    assert!(sink.begin_frames().unwrap().is_empty());
    assert_eq!(sink_users_by_peer(&sink, VISUALIZER_PEER), None, "released");
    assert_eq!(
        count_calls(fixture, "render_stream_handle_destroy"),
        destroyed_before,
        "parked while its wait runs"
    );
    sink
}

/// A sender released while its begin-frame wait runs is not destroyed during the wait: it is polled every viewer frame and goes once
/// the wait ends, its frame unshown. A Stop drains the parked senders within the bound too.
#[test]
fn a_sender_released_during_a_begin_wait_goes_once_the_wait_ends_or_at_stop() {
    let fixture = Fixture::new();
    fixture.set_try_begin_frame_delay(300);
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = sink_with_a_parked_sender(&fixture, &connection);
    let destroyed_at_release = count_calls(&fixture, "render_stream_handle_destroy");
    let started = Instant::now();
    while count_calls(&fixture, "render_stream_handle_destroy") == destroyed_at_release {
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the parked sender never went"
        );
        sink.update_users();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fixture.senders_destroyed_during_a_begin_wait(), 0);
    drop(sink);
    fixture.assert_clean_teardown(connection);

    let connection = fixture.connect(None).unwrap();
    let sink = sink_with_a_parked_sender(&fixture, &connection);
    let stop = Instant::now();
    drop(sink);
    let stopped_after = stop.elapsed();
    assert_eq!(
        fixture.senders_destroyed_during_a_begin_wait(),
        0,
        "the Stop drained the parked sender's wait"
    );
    assert!(stopped_after < BEGIN_WAIT_DRAIN_LIMIT, "{stopped_after:?}");
    fixture.assert_clean_teardown(connection);
}

/// A target whose creation timed out is held back, yet retried in the background once per
/// retry period: switched on later under the same listing (a headset off at start), it is
/// admitted within one period, and its hold is lifted.
#[test]
fn a_held_back_target_that_comes_alive_is_admitted_within_one_retry_period() {
    const RETRY_PERIOD: Duration = Duration::from_millis(1_500);
    let fixture = Fixture::new();
    fixture.set_auto_render_request(false);
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    fixture.set_peer_unanswering(VISUALIZER_PEER, 200);
    let mut sink = QarosSink::with_user_events(
        Rc::clone(&connection),
        sink_options(1),
        Box::new(
            TargetPeers::new(Rc::clone(&connection)).with_unanswered_retry_period(RETRY_PERIOD),
        ),
        Box::new(MaxUsers { maximum: 1 }),
    );
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |_, _| unanswered_peers_file_text(&fixture).contains(&VISUALIZER_PEER.to_string()),
    );
    let timed_out = Instant::now();
    fixture.set_peer_unanswering(VISUALIZER_PEER, 0);
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| {
            sink_users_by_peer(sink, VISUALIZER_PEER)
                .is_some_and(|visualizer| !frames_of(shown, visualizer).is_empty())
        },
    );
    let admitted_after = timed_out.elapsed();
    assert!(
        admitted_after >= RETRY_PERIOD / 2
            && admitted_after < RETRY_PERIOD + Duration::from_secs(1),
        "admitted {admitted_after:?} after its timeout"
    );
    assert_eq!(
        count_calls(&fixture, "render_sender_create_async"),
        2,
        "one creation that timed out, one background retry"
    );
    assert!(!unanswered_peers_file_text(&fixture).contains(&VISUALIZER_PEER.to_string()));
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A creation that times out in the first seconds of a run (QAROS still wiring the session
/// up) holds the target back, but its first
/// background retry comes after 2 s instead of the 60 s retry period.
#[test]
fn a_creation_that_times_out_while_starting_up_is_retried_after_two_seconds() {
    let fixture = Fixture::new();
    fixture.set_auto_render_request(false);
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    fixture.set_peer_unanswering(VISUALIZER_PEER, 200);
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(1));
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |_, _| unanswered_peers_file_text(&fixture).contains(&VISUALIZER_PEER.to_string()),
    );
    let timed_out = Instant::now();
    fixture.set_peer_unanswering(VISUALIZER_PEER, 0);
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| {
            sink_users_by_peer(sink, VISUALIZER_PEER)
                .is_some_and(|visualizer| !frames_of(shown, visualizer).is_empty())
        },
    );
    let admitted_after = timed_out.elapsed();
    assert!(
        admitted_after >= Duration::from_millis(1_500) && admitted_after < Duration::from_secs(5),
        "admitted {admitted_after:?} after its timeout; the retry period is 60 s"
    );
    assert_eq!(
        count_calls(&fixture, "render_sender_create_async"),
        2,
        "one creation that timed out, one background retry"
    );
    assert!(!unanswered_peers_file_text(&fixture).contains(&VISUALIZER_PEER.to_string()));
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A target listed under a new version id while a creation for its earlier app runs has
/// restarted: when that creation times out, the target is not held back (the restarted app
/// never had a chance to answer) and is tried again at the next peer list read.
#[test]
fn a_creation_that_times_out_after_its_target_restarted_holds_nothing_back() {
    let fixture = Fixture::new();
    fixture.set_auto_render_request(false);
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    fixture.set_peer_unanswering(VISUALIZER_PEER, 500);
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(1));
    run_viewer_frames(&mut sink, |_, _| {}, |sink, _| sink.is_creating_senders());
    fixture.set_peer_unanswering(VISUALIZER_PEER, 0);
    fixture.set_peer_version_id(VISUALIZER_PEER, "restarted");
    run_viewer_frames(&mut sink, |_, _| {}, |sink, _| !sink.is_creating_senders());
    assert_eq!(
        fixture
            .calls_starting_with("render_sender_create_async timed out")
            .len(),
        1
    );
    assert!(
        !unanswered_peers_file_text(&fixture).contains(&VISUALIZER_PEER.to_string()),
        "not held back under its new version id"
    );
    let timed_out = Instant::now();
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| {
            sink_users_by_peer(sink, VISUALIZER_PEER)
                .is_some_and(|visualizer| !frames_of(shown, visualizer).is_empty())
        },
    );
    assert!(
        timed_out.elapsed() < Duration::from_secs(3),
        "admitted {:?} after the timeout",
        timed_out.elapsed()
    );
    assert_eq!(count_calls(&fixture, "render_sender_create_async"), 2);
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A begin refused for an unresolved app volume is not ready, not failed: the header calls it
/// recoverable, so the user keeps its sender however long it lasts, and streams once the
/// volume resolves.
#[test]
fn begins_refused_for_an_unresolved_app_volume_do_not_release_the_user() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(1));
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| sink_users_by_peer(sink, VISUALIZER_PEER).is_some() && !shown.is_empty(),
    );
    let visualizer = sink_users_by_peer(&sink, VISUALIZER_PEER).unwrap();
    let unresolved_since = Instant::now();
    let mut refused_begins = 0;
    while unresolved_since.elapsed() < FRAME_FAILURE_PERIOD + Duration::from_millis(200) {
        fixture.fail_next("render_sender_try_begin_frame", APP_VOLUME_UNRESOLVED);
        assert!(sink.begin_frames().unwrap().is_empty());
        refused_begins += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(refused_begins > FRAME_FAILURE_MINIMUM);
    assert_eq!(sink_users_by_peer(&sink, VISUALIZER_PEER), Some(visualizer));
    let request = single_request(&mut sink).expect("the volume resolved");
    submit_both_eyes(&mut sink, request.key);
    sink.show_frame(request.key, show_info()).unwrap();
    assert_eq!(count_calls(&fixture, "render_stream_handle_destroy"), 0);
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// Shows that keep failing with something other than a closed stream release the user like
/// begins that keep failing.
#[test]
fn a_user_whose_shows_keep_failing_is_released() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(1));
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| sink_users_by_peer(sink, VISUALIZER_PEER).is_some() && !shown.is_empty(),
    );
    let failing_since = Instant::now();
    let mut failed_shows = 0;
    while sink_users_by_peer(&sink, VISUALIZER_PEER).is_some() {
        assert!(
            failing_since.elapsed() < FRAME_FAILURE_PERIOD + Duration::from_secs(2),
            "still rendered after {failed_shows} failed shows"
        );
        if let Some(request) = sink.begin_frames().unwrap().pop() {
            submit_both_eyes(&mut sink, request.key);
            fixture.fail_next("render_sender_show_frame", LOGIC_ERROR);
            assert!(sink.show_frame(request.key, show_info()).is_err());
            failed_shows += 1;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(failing_since.elapsed() >= FRAME_FAILURE_PERIOD);
    assert!(failed_shows >= FRAME_FAILURE_MINIMUM);
    assert_eq!(count_calls(&fixture, "render_stream_handle_destroy"), 1);
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// Streams the visualizer until two frames were shown, then begins once more, which starts a
/// begin-frame wait the stub ends only after its delay (or never, for a paused receiver).
fn stream_and_leave_a_begin_waiting(fixture: &Fixture, sink: &mut QarosSink) {
    run_viewer_frames(sink, |_, _| {}, |_, shown| shown.len() >= 2);
    assert!(
        sink.begin_frames().unwrap().is_empty(),
        "the begin waits for the stub's delay"
    );
    assert_eq!(fixture.senders_destroyed_during_a_begin_wait(), 0);
}

/// At Stop the sink lets a running begin-frame wait end before the sender goes, so the
/// sender does not outlive its handle into the session's destruction. A paused receiver's wait never ends; the Stop waits for it no longer than
/// [`BEGIN_WAIT_DRAIN_LIMIT`].
#[test]
fn a_stop_lets_begin_waits_end_before_the_senders_go_within_a_bound() {
    let fixture = Fixture::new();
    fixture.set_try_begin_frame_delay(40);
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(1));
    stream_and_leave_a_begin_waiting(&fixture, &mut sink);
    let stop = Instant::now();
    drop(sink);
    let stopped_after = stop.elapsed();
    assert_eq!(
        fixture.senders_destroyed_during_a_begin_wait(),
        0,
        "the wait ended first"
    );
    assert!(stopped_after < BEGIN_WAIT_DRAIN_LIMIT, "{stopped_after:?}");
    fixture.assert_clean_teardown(connection);

    let connection = fixture.connect(None).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(1));
    stream_and_leave_a_begin_waiting(&fixture, &mut sink);
    fixture.set_peer_paused(VISUALIZER_PEER, true);
    let stop = Instant::now();
    drop(sink);
    let stopped_after = stop.elapsed();
    assert_eq!(
        fixture.senders_destroyed_during_a_begin_wait(),
        1,
        "a paused receiver's wait outlasts the bound"
    );
    assert!(
        stopped_after >= BEGIN_WAIT_DRAIN_LIMIT
            && stopped_after < BEGIN_WAIT_DRAIN_LIMIT + DRAIN_TIMING_SLACK,
        "{stopped_after:?}"
    );
    fixture.assert_clean_teardown(connection);
}

/// User updates alone admit users, create their senders and release users who leave, with no
/// frame begun: the viewer applies them every frame, also while no 3D view is drawn.
#[test]
fn user_updates_admit_and_release_users_without_beginning_frames() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    let update_until = |sink: &mut QarosSink, done: &dyn Fn(&QarosSink) -> bool| {
        let started = Instant::now();
        while !done(sink) {
            assert!(started.elapsed() < Duration::from_secs(5), "never happened");
            sink.update_users();
            std::thread::sleep(Duration::from_millis(1));
        }
    };

    update_until(&mut sink, &|sink| {
        sink_users_by_peer(sink, VISUALIZER_PEER).is_some()
    });
    fixture.peer_join(HEADSET_A, "Stub Headset A");
    update_until(&mut sink, &|sink| {
        sink_users_by_peer(sink, HEADSET_A).is_some()
    });
    fixture.peer_leave(VISUALIZER_PEER);
    update_until(&mut sink, &|sink| {
        sink_users_by_peer(sink, VISUALIZER_PEER).is_none()
    });

    let rendered: Vec<String> = sink
        .rendered_users()
        .into_iter()
        .map(|(user, _)| user.label)
        .collect();
    assert_eq!(rendered, ["Stub Headset A"]);
    assert_eq!(count_calls(&fixture, "render_stream_handle_destroy"), 1);
    assert_eq!(count_calls(&fixture, "render_sender_try_begin_frame"), 0);
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// Begins that keep failing with something other than a closed stream release the user once
/// they have failed for [`FRAME_FAILURE_PERIOD`], at least [`FRAME_FAILURE_MINIMUM`] in a row; a
/// frame shown in between starts over. Many failures within the period are not enough.
#[test]
fn a_user_whose_begins_keep_failing_is_released() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    run_viewer_frames(
        &mut sink,
        |_, _| {},
        |sink, shown| sink_users_by_peer(sink, VISUALIZER_PEER).is_some() && !shown.is_empty(),
    );
    let visualizer = sink_users_by_peer(&sink, VISUALIZER_PEER).unwrap();
    let fail_begins = |sink: &mut QarosSink, count: u32| {
        for _ in 0..count {
            fixture.fail_next("render_sender_try_begin_frame", LOGIC_ERROR);
            assert!(sink.begin_frames().unwrap().is_empty());
        }
    };

    fail_begins(&mut sink, 3 * FRAME_FAILURE_MINIMUM);
    assert_eq!(sink_users_by_peer(&sink, VISUALIZER_PEER), Some(visualizer));
    std::thread::sleep(FRAME_FAILURE_PERIOD);
    let request = single_request(&mut sink).expect("the begin works again");
    submit_both_eyes(&mut sink, request.key);
    sink.show_frame(request.key, show_info()).unwrap();

    fail_begins(&mut sink, FRAME_FAILURE_MINIMUM);
    std::thread::sleep(FRAME_FAILURE_PERIOD / 2);
    assert_eq!(
        sink_users_by_peer(&sink, VISUALIZER_PEER),
        Some(visualizer),
        "the shown frame started over"
    );
    std::thread::sleep(FRAME_FAILURE_PERIOD / 2);
    let destroyed_before = count_calls(&fixture, "render_stream_handle_destroy");
    fail_begins(&mut sink, 1);
    assert_eq!(sink_users_by_peer(&sink, VISUALIZER_PEER), None);
    assert_eq!(
        count_calls(&fixture, "render_stream_handle_destroy"),
        destroyed_before + 1
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

fn sink_users_by_peer(sink: &QarosSink, peer: PeerId) -> Option<UserId> {
    sink.rendered_users()
        .iter()
        .find(|(_, rendered_peer)| *rendered_peer == peer)
        .map(|(user, _)| user.id)
}

/// A target the sink gave up on (its sender failed or its stream closed) waits out its
/// back-off whatever brings news about it: an update or a render request does not let it skip
/// the wait, as each attempt can block the UI thread up to the sender creation timeout.
#[test]
fn peer_updates_and_requests_wait_for_a_released_targets_back_off() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut peers = TargetPeers::new(Rc::clone(&connection));
    let joined = |events: &[PeerEvent]| {
        events.iter().any(
            |event| matches!(event, PeerEvent::Joined { peer, .. } if *peer == VISUALIZER_PEER),
        )
    };
    assert!(joined(&peers.poll_events()));

    peers.forget(VISUALIZER_PEER, ReleaseCause::Failed);
    let forgotten_at = Instant::now();
    fixture.peer_join(VISUALIZER_PEER, "Stub Visualizer");
    assert!(
        !joined(&peers.poll_events()),
        "the update and the request about it arrived, but the back-off holds"
    );
    let rejoined_after = loop {
        let elapsed = forgotten_at.elapsed();
        assert!(elapsed < Duration::from_secs(8), "never joined again");
        if joined(&peers.poll_events()) {
            break elapsed;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        rejoined_after >= Duration::from_secs(2),
        "joined again after {rejoined_after:?}, before the first back-off of 2 s"
    );
    drop(peers);
    fixture.assert_clean_teardown(connection);
}

/// A peer that joined by its own render request and is then listed as gone (shutting down or
/// retired) leaves, although requesters are otherwise kept when the peer list does not name
/// them as targets. Its next request joins it as a newcomer, so it was no longer present.
fn a_requester_that_departs_leaves(depart: impl Fn(&Fixture, PeerId)) {
    const UNLISTED_MIXER: PeerId = PeerId([0xbf; 16]);
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    // Any stall clock left over would count as a stall at once.
    let mut peers = TargetPeers::new(Rc::clone(&connection)).with_stall_timeout(Duration::ZERO);
    let joined_mixer = |events: &[PeerEvent]| {
        events
            .iter()
            .any(|event| matches!(event, PeerEvent::Joined { peer, .. } if *peer == UNLISTED_MIXER))
    };
    peers.poll_events();
    fixture.emit_render_request(UNLISTED_MIXER);
    assert!(joined_mixer(&peers.poll_events()), "joined as a requester");
    peers.stream_started(UNLISTED_MIXER);

    depart(&fixture, UNLISTED_MIXER);
    let events = peers.poll_events();
    assert!(
        events.contains(&PeerEvent::Left {
            peer: UNLISTED_MIXER
        }),
        "the departed requester leaves: {events:?}"
    );

    fixture.emit_render_request(UNLISTED_MIXER);
    assert!(
        joined_mixer(&peers.poll_events()),
        "no longer present, so its next request joins it again"
    );
    assert!(
        peers.leave_stalled(false).is_empty(),
        "the stream it held left with it, so no stall clock runs for the newcomer"
    );
    drop(peers);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_requester_listed_as_retired_leaves() {
    a_requester_that_departs_leaves(Fixture::peer_retire);
}

#[test]
fn a_requester_listed_as_shutting_down_leaves() {
    a_requester_that_departs_leaves(Fixture::peer_leave);
}

/// A target that leaves and comes back running (a restarted visualizer) is a
/// fresh receiver: it joins at once, without waiting out the back-off of its earlier failure.
#[test]
fn a_target_that_restarts_joins_again_without_its_back_off() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut peers = TargetPeers::new(Rc::clone(&connection));
    let joined = |events: &[PeerEvent]| {
        events.iter().any(
            |event| matches!(event, PeerEvent::Joined { peer, .. } if *peer == VISUALIZER_PEER),
        )
    };
    assert!(joined(&peers.poll_events()));

    peers.forget(VISUALIZER_PEER, ReleaseCause::Failed);
    fixture.peer_leave(VISUALIZER_PEER);
    assert!(!joined(&peers.poll_events()), "gone and backing off");

    fixture.peer_join(VISUALIZER_PEER, "Stub Visualizer");
    assert!(
        joined(&peers.poll_events()),
        "a target that comes back running joins at once"
    );
    drop(peers);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn users_joining_at_different_times_get_their_own_streams_poses_and_pairing() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));

    let shown = run_viewer_frames(
        &mut sink,
        |_, shown| {
            if shown.len() == 5 {
                fixture.peer_join(HEADSET_A, "Headset A");
            }
        },
        |_, shown| frames_of(shown, UserId(2)).len() >= 5,
    );
    let visualizer_frames = frames_of(&shown, UserId(1));
    let headset_frames = frames_of(&shown, UserId(2));
    assert!(
        visualizer_frames.len() > 5,
        "the first user keeps streaming after the second joins: {}",
        visualizer_frames.len()
    );
    for (expected_id, request) in (0..).zip(&visualizer_frames) {
        assert_eq!(request.key.frame_id, expected_id);
        #[allow(clippy::cast_precision_loss)]
        let step_back = 0.001 * expected_id as f32;
        let expected = Vec3::new(-0.032, 1.6, step_back);
        assert!((request.views[0].pose.position - expected).length() < 1e-6);
    }
    for (expected_id, request) in (0..).zip(&headset_frames) {
        assert_eq!(request.key.frame_id, expected_id, "frame ids are per user");
        assert!((request.views[0].pose.position.x - (0.5 - 0.032)).abs() < 1e-6);
    }
    assert_eq!(
        sink.rendered_users()
            .iter()
            .map(|(user, peer)| (user.id, user.label.as_str(), *peer))
            .collect::<Vec<_>>(),
        [
            (UserId(1), "Stub Visualizer", VISUALIZER_PEER),
            (UserId(2), "Headset A", HEADSET_A)
        ]
    );
    assert_eq!(
        fixture.calls_starting_with("paired").len(),
        shown.len(),
        "every shown frame carries the stamp of the frame its sender issued"
    );
    assert_eq!(fixture.frames_shown(HEADSET_A), headset_frames.len() as u64);
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_user_who_leaves_is_released_and_a_waiting_user_takes_the_slot() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    fixture.peer_join(HEADSET_A, "Headset A");
    fixture.peer_join(HEADSET_B, "Headset B");
    let shown = run_viewer_frames(&mut sink, |_, _| {}, |_, shown| shown.len() >= 4);
    assert!(
        frames_of(&shown, UserId(3)).is_empty(),
        "the third user waits"
    );
    assert_eq!(sink.waiting_users()[0].label, "Headset B");
    let senders_before = fixture
        .calls_starting_with("render_sender_create_async delivered")
        .len();

    fixture.peer_leave(VISUALIZER_PEER);
    let shown_after_leave = run_viewer_frames(
        &mut sink,
        |_, _| {},
        |_, shown| frames_of(shown, UserId(3)).len() >= 3,
    );
    assert!(frames_of(&shown_after_leave, UserId(1)).is_empty());
    let third_user_ids: Vec<u64> = frames_of(&shown_after_leave, UserId(3))
        .iter()
        .map(|request| request.key.frame_id)
        .collect();
    assert_eq!(third_user_ids[..3], [0, 1, 2]);
    assert!(
        (frames_of(&shown_after_leave, UserId(3))[0].views[0]
            .pose
            .position
            .x
            - (1.0 - 0.032))
            .abs()
            < 1e-6
    );
    assert_eq!(
        fixture
            .calls_starting_with("render_sender_create_async delivered")
            .len(),
        senders_before + 1,
        "one new sender, for the user who waited"
    );
    let users: Vec<UserId> = sink
        .rendered_users()
        .iter()
        .map(|(user, _)| user.id)
        .collect();
    assert_eq!(users, [UserId(2), UserId(3)]);
    assert!(sink.waiting_users().is_empty());
    let calls = fixture.calls();
    let leave = calls
        .iter()
        .position(|call| call == "peer left 56565656")
        .unwrap();
    assert!(
        calls[leave..]
            .iter()
            .any(|call| call == "render_stream_handle_destroy"),
        "the departed user's sender is released"
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A peer that leaves and joins again before the next poll is released and admitted in one
/// poll; its new sender counts frames from 0 again, so it must come back under a new user id
/// for a frame of the old sender never to pass for one of the new.
#[test]
fn a_peer_released_and_admitted_in_one_poll_gets_fresh_frame_keys() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    let stale = single_request(&mut sink).expect("a frame for the visualizer");
    assert_eq!(
        stale.key,
        FrameKey {
            user: UserId(1),
            frame_id: 0
        }
    );

    fixture.peer_leave(VISUALIZER_PEER);
    fixture.peer_join(VISUALIZER_PEER, "Stub Visualizer");
    let fresh = single_request(&mut sink).expect("a frame for the returning visualizer");
    assert_eq!(
        fresh.key,
        FrameKey {
            user: UserId(2),
            frame_id: 0
        },
        "a new user, its frame ids from 0"
    );
    assert!(
        sink.submit_view(stale.key, 0, stamped_eye(stale.key, 0))
            .is_err(),
        "the old sender's frame does not reach the new one"
    );
    assert!(sink.show_frame(stale.key, show_info()).is_err());
    submit_both_eyes(&mut sink, fresh.key);
    sink.show_frame(fresh.key, show_info()).unwrap();
    assert_eq!(
        fixture.calls_starting_with("paired"),
        ["paired peer=56565656 frame=0 stamped=0"],
        "the new sender shows exactly its own frame"
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn the_sink_streams_only_while_somebody_is_rendered() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    run_viewer_frames(&mut sink, |_, _| {}, |_, shown| shown.len() >= 2);
    assert!(sink.is_streaming());

    fixture.peer_leave(VISUALIZER_PEER);
    let started = Instant::now();
    while sink.is_streaming() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the leave never arrived"
        );
        for request in sink.begin_frames().unwrap() {
            sink.abandon_frame(request.key);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(sink.rendered_users().is_empty());
    assert!(
        !sink.is_finished(),
        "idle, not finished: a user may still come"
    );

    fixture.peer_join(HEADSET_A, "Headset A");
    let shown = run_viewer_frames(&mut sink, |_, _| {}, |_, shown| !shown.is_empty());
    assert_eq!(shown[0].key.user, UserId(2));
    assert!(sink.is_streaming(), "streams again for the new user");
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn polled_begins_stream_two_users_and_tear_down_with_a_wait_pending() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = QarosSink::new(Rc::clone(&connection), sink_options(2));
    fixture.peer_join(HEADSET_A, "Headset A");
    let shown = run_viewer_frames(
        &mut sink,
        |_, _| {},
        |_, shown| frames_of(shown, UserId(2)).len() >= 5,
    );
    for user in [UserId(1), UserId(2)] {
        let frame_ids: Vec<u64> = frames_of(&shown, user)
            .iter()
            .map(|request| request.key.frame_id)
            .collect();
        assert_eq!(frame_ids[..5], [0, 1, 2, 3, 4], "{user}");
    }
    assert_eq!(
        fixture.calls_starting_with("paired").len(),
        shown.len(),
        "every polled frame carries the stamp of the frame its sender issued"
    );
    // The next begin of each user may be waiting; dropping the sink must not hang on it.
    assert!(sink.begin_frames().unwrap().len() <= 2);
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_wrongly_stamped_frame_shows_up_in_the_stub_pairing_check() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut sink = new_sink(&connection, PoseSpace::Room);
    let request = single_request(&mut sink).expect("a frame");
    let previous_frame = FrameKey {
        frame_id: request.key.frame_id + 1,
        ..request.key
    };
    for eye_index in 0..2 {
        sink.submit_view(
            request.key,
            eye_index,
            stamped_eye(previous_frame, eye_index),
        )
        .unwrap();
    }
    sink.show_frame(request.key, show_info()).unwrap();
    let mismatches = fixture.calls_starting_with("ERROR pairing");
    assert_eq!(mismatches.len(), 1, "{mismatches:?}");
    assert!(mismatches[0].starts_with("ERROR pairing peer=56565656 issued frame 0"));
}

#[test]
fn opens_a_panel_per_view_within_reach_and_hides_them_at_stop() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let time_series = PanelView {
        name: "time_series".to_owned(),
        width_in_pixel: 800,
        height_in_pixel: 400,
    };
    let image = PanelView {
        name: "image".to_owned(),
        width_in_pixel: 400,
        height_in_pixel: 400,
    };

    let mut panels = panels_with_a_viewer(Rc::clone(&connection), "http://127.0.0.1:9877/");
    let published = view_names(&["time_series", "image"]);
    panels.sync(&[time_series.clone(), image], &published);
    panels.sync(&[time_series], &published);
    assert_eq!(panels.panels().len(), 2);
    assert_eq!(
        fixture
            .calls_starting_with("gui_panels_navigate_to_uri")
            .len(),
        2,
        "each new panel is navigated once"
    );
    let listed = connection.session().gui_panels().list().unwrap();
    let time_series_panel = listed
        .iter()
        .find(|panel| panel.content_uri == "http://127.0.0.1:9877/views/time_series")
        .expect("time series panel navigated to its view page");
    assert_eq!(time_series_panel.display_name, "time_series.rerun.rerun");
    assert!((time_series_panel.size.width - 0.45).abs() < 1e-6);
    assert!((time_series_panel.size.height - 0.225).abs() < 1e-6);
    // The viewer stands at the origin, eyes 1.6 m up, looking along -Z: the first panel's top
    // edge 0.8 m away on the floor plane, 0.1 m below the eyes, in front and to the right
    // (0.25 m start plus half a panel along the arc: 0.59375 rad right of the gaze).
    let pose = time_series_panel.pose;
    let angle: f32 = (0.25 + 0.225) / 0.8;
    assert!(
        (pose.position.x - 0.8 * angle.sin()).abs() < 1e-5,
        "{pose:?}"
    );
    assert!((pose.position.y - 1.5).abs() < 1e-5, "{pose:?}");
    assert!(
        (pose.position.z + 0.8 * angle.cos()).abs() < 1e-5,
        "{pose:?}"
    );
    let image_panel = listed
        .iter()
        .find(|panel| panel.content_uri == "http://127.0.0.1:9877/views/image")
        .expect("image panel navigated to its view page");
    let image_position = Vec3::new(
        image_panel.pose.position.x,
        image_panel.pose.position.y,
        image_panel.pose.position.z,
    );
    assert!(
        (Vec3::new(image_position.x, 0.0, image_position.z).length() - 0.8).abs() < 1e-5
            && image_position.x > pose.position.x,
        "the second panel within reach too, further right: {image_position}"
    );

    drop(panels);
    assert_eq!(
        panel_states(&connection),
        [
            ("image.rerun.rerun".to_owned(), GuiPanelState::Hidden),
            ("time_series.rerun.rerun".to_owned(), GuiPanelState::Hidden),
        ],
        "Stop hides the panels and keeps them in the session"
    );
    assert_eq!(fixture.calls_starting_with("gui_panels_set_state").len(), 2);
    assert!(
        fixture
            .calls_starting_with("gui_panels_close_panel")
            .is_empty()
    );
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_timeline_panel_opens_without_views_in_front_of_and_below_the_head_facing_it() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut panels = QarosPanels::new(Rc::clone(&connection), "http://127.0.0.1:9877/")
        .with_timeline_panel("/timeline-panel");
    panels.sync(&[], &[]);
    assert!(
        panels.panels().is_empty(),
        "no head yet: the timeline waits"
    );

    place_panels_by(&mut panels, viewer_head());
    panels.sync(&[], &[]);
    panels.sync(&[], &[]);
    assert_eq!(panels.panels().len(), 1);
    assert_eq!(navigation_count(&fixture), 1, "navigated once");
    assert_eq!(
        open_panels(&connection),
        [(
            "timeline.panel.rerun.rerun".to_owned(),
            "http://127.0.0.1:9877/timeline-panel".to_owned()
        )]
    );
    let listed = connection.session().gui_panels().list().unwrap();
    let timeline = &listed[0];
    assert!((timeline.size.width - 0.45).abs() < 1e-6);
    assert!((timeline.size.height - 0.12).abs() < 1e-6);
    // The head at (0, 1.6, 0) looks along -Z: the panel's centre at (0, 1.4, -0.5), tilted
    // back to face the eyes, its top edge 0.06 m up the panel from the centre.
    let orientation = Quat::from_xyzw(
        timeline.pose.orientation.x,
        timeline.pose.orientation.y,
        timeline.pose.orientation.z,
        timeline.pose.orientation.w,
    );
    let top_centre = Vec3::new(
        timeline.pose.position.x,
        timeline.pose.position.y,
        timeline.pose.position.z,
    );
    let centre = top_centre - orientation * Vec3::Y * 0.06;
    assert!(
        centre.abs_diff_eq(Vec3::new(0.0, 1.4, -0.5), 1e-5),
        "centre {centre}"
    );
    let front_normal = orientation * Vec3::NEG_Z;
    let to_the_eyes = (viewer_head().position - centre).normalize();
    assert!(
        front_normal.abs_diff_eq(to_the_eyes, 1e-5),
        "front {front_normal} faces the eyes along {to_the_eyes}"
    );

    // A view settling later takes the first place of the row: the timeline holds none.
    panels.sync(&panel_views(&["map"]), &view_names(&["map"]));
    let map_position = panel_position(&connection, "map");
    let first_place_angle: f32 = (0.25 + 0.225) / 0.8;
    assert!(
        (map_position.x - 0.8 * first_place_angle.sin()).abs() < 1e-5,
        "{map_position}"
    );

    drop(panels);
    assert_eq!(
        panel_states(&connection),
        [
            ("map.rerun.rerun".to_owned(), GuiPanelState::Hidden),
            (
                "timeline.panel.rerun.rerun".to_owned(),
                GuiPanelState::Hidden
            ),
        ],
        "Stop hides the timeline panel like the others"
    );
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_restart_reclaims_the_timeline_panel_where_it_is() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let views = panel_views(&["map"]);
    let published = view_names(&["map"]);
    let mut first_run = panels_with_a_viewer(Rc::clone(&connection), "http://127.0.0.1:9877")
        .with_timeline_panel("/timeline-panel");
    first_run.sync(&views, &published);
    let first_run_panels = first_run.panels().clone();
    let map_position = panel_position(&connection, "map");
    let timeline_position = panel_position(&connection, "timeline.panel");
    drop(first_run);
    let navigations = navigation_count(&fixture);

    let mut second_run = QarosPanels::new(Rc::clone(&connection), "http://127.0.0.1:9877")
        .with_timeline_panel("/timeline-panel");
    second_run.sync(&views, &published);
    assert_eq!(
        second_run.panels(),
        &first_run_panels,
        "both reattached before any head"
    );
    assert_eq!(navigation_count(&fixture), navigations, "pages kept");
    assert_eq!(panel_position(&connection, "map"), map_position);
    assert_eq!(
        panel_position(&connection, "timeline.panel"),
        timeline_position
    );
    assert!(
        panel_states(&connection)
            .iter()
            .all(|(_, state)| *state == GuiPanelState::Visible),
        "shown again, none hidden as a leftover: {:?}",
        panel_states(&connection)
    );

    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

fn view_names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Display name and state of every panel in the session, sorted.
fn panel_states(connection: &QarosConnection) -> Vec<(String, GuiPanelState)> {
    let mut states: Vec<(String, GuiPanelState)> = connection
        .session()
        .gui_panels()
        .list()
        .unwrap()
        .into_iter()
        .map(|panel| (panel.display_name, panel.state))
        .collect();
    states.sort_by(|first, second| first.0.cmp(&second.0));
    states
}

fn panel_views(names: &[&str]) -> Vec<PanelView> {
    names
        .iter()
        .map(|name| PanelView {
            name: (*name).to_owned(),
            width_in_pixel: 800,
            height_in_pixel: 600,
        })
        .collect()
}

fn navigation_count(fixture: &Fixture) -> usize {
    fixture
        .calls_starting_with("gui_panels_navigate_to_uri")
        .len()
}

/// Display name and URL of every open panel, sorted.
fn open_panels(connection: &QarosConnection) -> Vec<(String, String)> {
    let mut open: Vec<(String, String)> = connection
        .session()
        .gui_panels()
        .list()
        .unwrap()
        .into_iter()
        .filter(|panel| panel.state != GuiPanelState::Closed)
        .map(|panel| (panel.display_name, panel.content_uri))
        .collect();
    open.sort();
    open
}

fn open_foreign_panel(connection: &QarosConnection, common_name: &str, display_name: &str) {
    let mut config = qaros::GuiPanelConfig::new(common_name);
    config.display_name = Some(display_name.to_owned());
    connection
        .session()
        .gui_panels()
        .get_or_create(&config)
        .unwrap();
}

#[test]
fn a_stop_then_start_shows_the_hidden_panels_again_without_navigating_them() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let views = panel_views(&["map", "camera"]);
    let published = view_names(&["map", "camera"]);
    let mut first_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    first_run.sync(&views, &published);
    let first_run_panels = first_run.panels().clone();
    let first_run_open = open_panels(&connection);
    assert_eq!(
        first_run_open,
        [
            (
                "camera.rerun.rerun".to_owned(),
                "http://192.168.50.40:9877/views/camera".to_owned()
            ),
            (
                "map.rerun.rerun".to_owned(),
                "http://192.168.50.40:9877/views/map".to_owned()
            ),
        ]
    );
    assert_eq!(navigation_count(&fixture), 2);

    let first_run_map_url = first_run.view_url("map");

    // Someone moves the map panel, then Stop hides the panels.
    let gui_panels = connection.session().gui_panels();
    let mut moved_away = qaros::GuiPanelConfig::new("unused").pose;
    moved_away.position.y = 7.0;
    gui_panels
        .set_pose(first_run_panels["map"], moved_away)
        .unwrap();
    drop(first_run);
    assert_eq!(
        panel_states(&connection),
        [
            ("camera.rerun.rerun".to_owned(), GuiPanelState::Hidden),
            ("map.rerun.rerun".to_owned(), GuiPanelState::Hidden),
        ]
    );

    let mut second_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    second_run.sync(&views, &published);
    assert_eq!(second_run.panels(), &first_run_panels);
    assert_eq!(second_run.view_url("map"), first_run_map_url);
    assert_eq!(
        open_panels(&connection),
        first_run_open,
        "same panels, same URLs"
    );
    assert_eq!(
        panel_states(&connection),
        [
            ("camera.rerun.rerun".to_owned(), GuiPanelState::Visible),
            ("map.rerun.rerun".to_owned(), GuiPanelState::Visible),
        ],
        "shown again"
    );
    assert_eq!(
        navigation_count(&fixture),
        2,
        "the restart navigates nothing"
    );
    assert_eq!(
        fixture.calls_starting_with("gui_panels_set_state").len(),
        4,
        "hidden at Stop, shown at the restart, one call each"
    );
    assert!(
        fixture
            .calls_starting_with("gui_panels_close_panel")
            .is_empty(),
        "neither Stop nor the restart closes a panel"
    );
    let map_panel = gui_panels
        .list()
        .unwrap()
        .into_iter()
        .find(|panel| panel.id == first_run_panels["map"])
        .expect("the map panel is still open");
    assert!(
        (map_panel.pose.position.y - 7.0).abs() < 1e-5,
        "still where it was moved: the place belongs to the user, {map_panel:?}"
    );

    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn new_panels_wait_for_a_viewer_near_the_volume_and_open_within_reach() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut panels = QarosPanels::new(Rc::clone(&connection), "http://127.0.0.1:9877");
    let published = view_names(&["map", "camera"]);
    panels.sync(&panel_views(&["map"]), &published);
    assert!(
        panels.panels().is_empty(),
        "no head yet: the new panel waits"
    );

    // A receiver's camera far across the room places nothing, however long it streams.
    let first_seen = Instant::now();
    let distant_receiver = Pose::from_translation(Vec3::new(1.2, 2.1, 7.0));
    panels.consider_head(UserId(1), distant_receiver, first_seen);
    panels.consider_head(UserId(1), distant_receiver, first_seen + HEADSET_WAIT * 2);
    panels.sync(&panel_views(&["map"]), &published);
    assert!(panels.panels().is_empty(), "a distant head places nothing");

    // The stub names no device category, so a near head is no known headset: it places the
    // panels only once no headset came for HEADSET_WAIT after it first streamed.
    let head = viewer_head();
    panels.consider_head(UserId(2), head, first_seen);
    panels.consider_head(
        UserId(2),
        head,
        first_seen + HEADSET_WAIT.saturating_sub(Duration::from_millis(1)),
    );
    panels.sync(&panel_views(&["map"]), &published);
    assert!(
        panels.panels().is_empty(),
        "no headset yet, and the wait for one not over"
    );
    panels.consider_head(UserId(2), head, first_seen + HEADSET_WAIT);
    panels.sync(&panel_views(&["map"]), &published);
    let map_position = panel_position(&connection, "map");
    let to_map = map_position - head.position;
    assert!(
        (Vec3::new(to_map.x, 0.0, to_map.z).length() - 0.8).abs() < 1e-4,
        "within reach: {map_position}"
    );
    assert!(
        (to_map.y + 0.1).abs() < 1e-4,
        "top edge below the eyes: {map_position}"
    );
    assert!(
        to_map.x > 0.0 && to_map.z < 0.0,
        "in front, to the right: {map_position}"
    );

    // A later head changes nothing; a panel opened later joins the same row.
    panels.consider_head(
        UserId(3),
        Pose::from_translation(Vec3::new(1.0, 1.6, 1.0)),
        first_seen + HEADSET_WAIT * 2,
    );
    panels.sync(&panel_views(&["map", "camera"]), &published);
    assert_eq!(panel_position(&connection, "map"), map_position);
    let camera_position = panel_position(&connection, "camera");
    let to_camera = camera_position - head.position;
    assert!(
        (Vec3::new(to_camera.x, 0.0, to_camera.z).length() - 0.8).abs() < 1e-4,
        "the later panel stands within reach of the first head: {camera_position}"
    );
    assert!(
        to_camera.x > to_map.x,
        "further right than the first panel: {camera_position}"
    );
    assert!(
        fixture
            .calls_starting_with("gui_panels_update_pose")
            .is_empty(),
        "panels are created in place, never moved"
    );

    drop(panels);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_restart_leaves_a_panel_where_the_user_put_it() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let views = panel_views(&["map"]);
    let published = view_names(&["map"]);
    let mut first_run = panels_with_a_viewer(Rc::clone(&connection), "http://127.0.0.1:9877");
    first_run.sync(&views, &published);
    let map = first_run.panels()["map"];
    let gui_panels = connection.session().gui_panels();
    let mut put_by_the_user = qaros::GuiPanelConfig::new("unused").pose;
    put_by_the_user.position.x = -2.0;
    put_by_the_user.position.y = 1.2;
    gui_panels.set_pose(map, put_by_the_user).unwrap();
    let user_calls = fixture.calls_starting_with("gui_panels_update_pose").len();
    drop(first_run);

    let mut second_run = QarosPanels::new(Rc::clone(&connection), "http://127.0.0.1:9877");
    place_panels_by(
        &mut second_run,
        Pose::from_translation(Vec3::new(1.0, 1.6, 0.5)),
    );
    second_run.sync(&views, &published);
    assert_eq!(
        second_run.panels()["map"],
        map,
        "the same panel, reattached"
    );
    assert_eq!(
        panel_position(&connection, "map"),
        Vec3::new(-2.0, 1.2, put_by_the_user.position.z),
        "where the user put it"
    );
    assert_eq!(
        fixture.calls_starting_with("gui_panels_update_pose").len(),
        user_calls,
        "the restart sets no pose"
    );

    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

/// A viewer standing at the room origin, eyes 1.6 m up, near the stub's app volume.
fn viewer_head() -> Pose {
    Pose::from_translation(Vec3::new(0.0, 1.6, 0.0))
}

/// Panels for a run whose viewer stands near the app volume, so new panels open at once.
fn panels_with_a_viewer(connection: Rc<QarosConnection>, base_url: &str) -> QarosPanels {
    let mut panels = QarosPanels::new(connection, base_url);
    place_panels_by(&mut panels, viewer_head());
    panels
}

/// The stub names no device category, so `head` places the panels once it has streamed for
/// [`HEADSET_WAIT`] without a headset.
fn place_panels_by(panels: &mut QarosPanels, head: Pose) {
    let first_seen = Instant::now();
    panels.consider_head(UserId(1), head, first_seen);
    panels.consider_head(UserId(1), head, first_seen + HEADSET_WAIT);
}

fn panel_position(connection: &QarosConnection, view_name: &str) -> Vec3 {
    let display_name = format!("{view_name}.rerun.rerun");
    let panel = connection
        .session()
        .gui_panels()
        .list()
        .unwrap()
        .into_iter()
        .find(|panel| panel.display_name == display_name)
        .unwrap_or_else(|| panic!("no panel {display_name}"));
    Vec3::new(
        panel.pose.position.x,
        panel.pose.position.y,
        panel.pose.position.z,
    )
}

#[test]
fn a_failed_panel_listing_opens_nothing_and_the_next_sync_reattaches() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let views = panel_views(&["map", "camera"]);
    let published = view_names(&["map", "camera"]);
    let mut first_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    first_run.sync(&views, &published);
    let first_run_panels = first_run.panels().clone();
    drop(first_run);
    let calls_before_restart = fixture.calls().len();

    let mut second_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    fixture.fail_next("query_gui_panels_count", UNCLASSIFIED);
    second_run.sync(&views, &published);
    assert!(second_run.panels().is_empty(), "{:?}", second_run.panels());
    let calls_of_failed_sync: Vec<String> = fixture.calls()[calls_before_restart..]
        .iter()
        .filter(|call| call.starts_with("gui_panels_") || call.starts_with("gui_panel_"))
        .cloned()
        .collect();
    assert!(
        calls_of_failed_sync.is_empty(),
        "no panel call without the list: {calls_of_failed_sync:?}"
    );
    assert_eq!(
        panel_states(&connection),
        [
            ("camera.rerun.rerun".to_owned(), GuiPanelState::Hidden),
            ("map.rerun.rerun".to_owned(), GuiPanelState::Hidden),
        ]
    );

    second_run.sync(&views, &published);
    assert!(
        second_run.panels().is_empty(),
        "a sync right after the failure waits for the half-second retry"
    );
    std::thread::sleep(std::time::Duration::from_millis(550));
    second_run.sync(&views, &published);
    assert_eq!(second_run.panels(), &first_run_panels);
    assert_eq!(navigation_count(&fixture), 2, "the retry navigates nothing");
    assert_eq!(
        panel_states(&connection),
        [
            ("camera.rerun.rerun".to_owned(), GuiPanelState::Visible),
            ("map.rerun.rerun".to_owned(), GuiPanelState::Visible),
        ],
        "shown again by the retry"
    );

    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn panels_kept_at_stop_get_no_call_and_are_reattached_as_they_are() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let views = panel_views(&["map"]);
    let published = view_names(&["map"]);
    let mut first_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877")
        .with_panels_at_stop(PanelsAtStop::Keep);
    first_run.sync(&views, &published);
    let map_panel = first_run.panels()["map"];
    drop(first_run);
    assert_eq!(
        panel_states(&connection),
        [("map.rerun.rerun".to_owned(), GuiPanelState::Visible)]
    );

    let mut second_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877")
        .with_panels_at_stop(PanelsAtStop::Keep);
    second_run.sync(&views, &published);
    assert_eq!(second_run.panels()["map"], map_panel);
    assert_eq!(navigation_count(&fixture), 1);
    assert!(
        fixture
            .calls_starting_with("gui_panels_set_state")
            .is_empty(),
        "no state change at Stop or at the restart"
    );
    assert!(
        fixture
            .calls_starting_with("gui_panels_close_panel")
            .is_empty()
    );
    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_view_that_settles_later_keeps_its_hidden_panel_for_its_own_claim() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let both = view_names(&["map", "camera"]);
    let mut first_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    first_run.sync(&panel_views(&["map", "camera"]), &both);
    let first_run_panels = first_run.panels().clone();
    drop(first_run);

    let mut second_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    second_run.sync(&panel_views(&["map"]), &both);
    assert_eq!(
        panel_states(&connection),
        [
            ("camera.rerun.rerun".to_owned(), GuiPanelState::Hidden),
            ("map.rerun.rerun".to_owned(), GuiPanelState::Visible),
        ],
        "the camera view publishes but has not settled: its panel waits, hidden"
    );
    second_run.sync(&panel_views(&["map", "camera"]), &both);
    assert_eq!(second_run.panels(), &first_run_panels);
    assert_eq!(
        panel_states(&connection),
        [
            ("camera.rerun.rerun".to_owned(), GuiPanelState::Visible),
            ("map.rerun.rerun".to_owned(), GuiPanelState::Visible),
        ]
    );
    assert_eq!(
        navigation_count(&fixture),
        2,
        "only the first run navigates"
    );
    assert!(
        fixture
            .calls_starting_with("gui_panels_close_panel")
            .is_empty()
    );
    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

/// A restart whose second view publishes only after the first one claimed its panel (a
/// background tab, data that arrives late) hides that view's leftover panel instead of
/// closing it, and the later claim shows it again without navigating it.
#[test]
fn a_view_that_publishes_late_after_a_restart_reclaims_its_hidden_panel() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let both = view_names(&["map", "camera"]);
    let mut first_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877")
        .with_panels_at_stop(PanelsAtStop::Keep);
    first_run.sync(&panel_views(&["map", "camera"]), &both);
    let first_run_panels = first_run.panels().clone();
    drop(first_run);
    assert_eq!(navigation_count(&fixture), 2);

    let mut second_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    second_run.sync(&panel_views(&["map"]), &view_names(&["map"]));
    assert_eq!(
        panel_states(&connection),
        [
            ("camera.rerun.rerun".to_owned(), GuiPanelState::Hidden),
            ("map.rerun.rerun".to_owned(), GuiPanelState::Visible),
        ],
        "the camera view is not published yet: its panel is hidden"
    );

    second_run.sync(&panel_views(&["map", "camera"]), &both);
    assert_eq!(second_run.panels(), &first_run_panels);
    assert_eq!(
        panel_states(&connection),
        [
            ("camera.rerun.rerun".to_owned(), GuiPanelState::Visible),
            ("map.rerun.rerun".to_owned(), GuiPanelState::Visible),
        ],
        "shown again by its own claim"
    );
    assert_eq!(
        navigation_count(&fixture),
        2,
        "the restart navigates nothing"
    );
    assert!(
        fixture
            .calls_starting_with("gui_panels_close_panel")
            .is_empty(),
        "no panel is closed"
    );
    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

/// A first run opens map, camera and image panels and stops; between runs a user minimises
/// the image panel. The second run reattaches all three without navigation.
fn restart_with_kept_pages(
    fixture: &Fixture,
    connection: &Rc<QarosConnection>,
    dead_pages: DeadPages,
) -> QarosPanels {
    let views = panel_views(&["map", "camera", "image"]);
    let published = view_names(&["map", "camera", "image"]);
    let mut first_run = panels_with_a_viewer(Rc::clone(connection), "http://192.168.50.40:9877");
    first_run.sync(&views, &published);
    let image_panel = first_run.panels()["image"];
    drop(first_run);
    connection
        .session()
        .gui_panels()
        .set_state(image_panel, GuiPanelState::Minimized)
        .unwrap();

    let mut second_run = panels_with_a_viewer(Rc::clone(connection), "http://192.168.50.40:9877")
        .with_dead_pages(dead_pages);
    second_run.sync(&views, &published);
    assert_eq!(
        navigation_count(fixture),
        3,
        "the restart navigates nothing"
    );
    second_run
}

/// What the panel server and the sink have seen in a test: the views whose pages asked for
/// something, and whether a user stream started.
#[derive(Default)]
struct SeenActivity {
    pages_asking: Vec<&'static str>,
    user_stream_started: bool,
}

impl PanelActivity for SeenActivity {
    fn last_page_request(&self, view_name: &str) -> Option<Instant> {
        self.pages_asking.contains(&view_name).then(Instant::now)
    }

    fn any_page_request(&self) -> bool {
        !self.pages_asking.is_empty()
    }

    fn user_stream_started(&self) -> bool {
        self.user_stream_started
    }
}

/// The headset is off: no page asks for anything and no user streams.
fn browser_not_running() -> SeenActivity {
    SeenActivity::default()
}

/// A user streams; the map page asks for nothing (a document that failed to load while no
/// panel server ran) and neither does any other page yet.
fn stream_started_all_pages_silent() -> SeenActivity {
    SeenActivity {
        pages_asking: Vec::new(),
        user_stream_started: true,
    }
}

/// No user streams, but the camera page asks for frames: the browser runs the pages' timers,
/// so the silent map page is dead.
fn camera_page_alive() -> SeenActivity {
    SeenActivity {
        pages_asking: vec!["camera"],
        user_stream_started: false,
    }
}

#[test]
fn a_reattached_page_is_not_reloaded_while_the_headset_is_off() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut second_run = restart_with_kept_pages(&fixture, &connection, DeadPages::Reload);
    let claimed = Instant::now();

    for silent_for in [
        PAGE_SILENCE_LIMIT,
        PAGE_SILENCE_LIMIT * 6,
        PAGE_SILENCE_LIMIT * 60,
    ] {
        assert!(
            second_run
                .reload_dead_pages(&browser_not_running(), claimed + silent_for)
                .is_empty(),
            "silence while the browser is not seen running proves nothing"
        );
    }
    assert_eq!(navigation_count(&fixture), 3, "no navigation");
    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_page_still_silent_after_a_stream_started_is_loaded_again_once_at_the_same_url() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut second_run = restart_with_kept_pages(&fixture, &connection, DeadPages::Reload);
    let open_before = open_panels(&connection);
    let claimed = Instant::now();
    let stream_started = claimed + PAGE_SILENCE_LIMIT * 3;

    assert!(
        second_run
            .reload_dead_pages(&browser_not_running(), claimed + PAGE_SILENCE_LIMIT * 2)
            .is_empty()
    );
    assert!(
        second_run
            .reload_dead_pages(&stream_started_all_pages_silent(), stream_started)
            .is_empty(),
        "the silence clock starts at the stream, not at the claim"
    );
    assert!(
        second_run
            .reload_dead_pages(
                &stream_started_all_pages_silent(),
                stream_started + PAGE_SILENCE_LIMIT.mul_f32(0.9)
            )
            .is_empty()
    );
    assert_eq!(navigation_count(&fixture), 3);

    assert_eq!(
        second_run.reload_dead_pages(
            &stream_started_all_pages_silent(),
            stream_started + PAGE_SILENCE_LIMIT
        ),
        ["camera", "map"],
        "the silent visible pages; not the minimised image panel"
    );
    assert_eq!(navigation_count(&fixture), 5);
    assert_eq!(
        open_panels(&connection),
        open_before,
        "the same URLs, so the next run finds them unchanged"
    );

    assert!(
        second_run
            .reload_dead_pages(
                &stream_started_all_pages_silent(),
                stream_started + PAGE_SILENCE_LIMIT * 3
            )
            .is_empty(),
        "once per run"
    );
    assert_eq!(navigation_count(&fixture), 5);
    assert!(
        fixture
            .calls_starting_with("gui_panels_close_panel")
            .is_empty()
    );
    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn a_silent_page_beside_a_live_one_is_loaded_again_once() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut second_run = restart_with_kept_pages(&fixture, &connection, DeadPages::Reload);
    let claimed = Instant::now();

    assert!(
        second_run
            .reload_dead_pages(&camera_page_alive(), claimed)
            .is_empty()
    );
    assert_eq!(
        navigation_count(&fixture),
        3,
        "not before the silence limit"
    );

    assert_eq!(
        second_run.reload_dead_pages(&camera_page_alive(), claimed + PAGE_SILENCE_LIMIT),
        ["map"],
        "only the silent, visible map page; not the live camera page nor the minimised image panel"
    );
    assert_eq!(navigation_count(&fixture), 4);
    assert!(
        second_run
            .reload_dead_pages(&camera_page_alive(), claimed + PAGE_SILENCE_LIMIT * 3)
            .is_empty(),
        "once per run"
    );
    assert_eq!(navigation_count(&fixture), 4);
    drop(second_run);

    let mut third_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    third_run.sync(
        &panel_views(&["map", "camera", "image"]),
        &view_names(&["map", "camera", "image"]),
    );
    assert_eq!(
        navigation_count(&fixture),
        4,
        "the third run navigates nothing"
    );
    drop(third_run);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn dead_pages_left_as_they_are_get_no_navigation() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut second_run = restart_with_kept_pages(&fixture, &connection, DeadPages::Leave);
    let claimed = Instant::now();

    assert!(
        second_run
            .reload_dead_pages(&stream_started_all_pages_silent(), claimed)
            .is_empty()
    );
    assert!(
        second_run
            .reload_dead_pages(
                &stream_started_all_pages_silent(),
                claimed + PAGE_SILENCE_LIMIT
            )
            .is_empty()
    );
    assert_eq!(navigation_count(&fixture), 3);
    drop(second_run);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn the_teardown_marks_each_step_for_the_stop_watchdog() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut panels = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    panels.sync(&panel_views(&["map"]), &view_names(&["map"]));
    enter_teardown_step(TeardownStep::LeaveSink);

    drop(panels);
    assert_eq!(current_teardown_step(), TeardownStep::HidePanels);

    fixture.assert_clean_teardown(connection);
    assert_eq!(
        current_teardown_step(),
        TeardownStep::LibraryDestroy,
        "the library goes last, after the session"
    );
    enter_teardown_step(TeardownStep::NotStarted);
}

#[test]
fn a_changed_panel_server_address_still_finds_the_panels_by_name() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    open_foreign_panel(&connection, "status.other.example.com", "status");
    open_foreign_panel(
        &connection,
        "map.rerun-air_traffic.rerun",
        "map.rerun-air_traffic.rerun",
    );
    open_foreign_panel(
        &connection,
        "lookalike.other.example.com",
        "map.other.rerun.rerun",
    );
    let mut killed_run = panels_with_a_viewer(Rc::clone(&connection), "http://192.168.50.40:9877");
    killed_run.sync(
        &panel_views(&["map", "camera"]),
        &view_names(&["map", "camera"]),
    );
    let killed_run_panels = killed_run.panels().clone();
    assert_eq!(navigation_count(&fixture), 2);

    // The next run publishes no camera view (another blueprint): its panel is a leftover.
    let mut next_run = panels_with_a_viewer(Rc::clone(&connection), "http://10.0.0.5:9900");
    next_run.sync(&panel_views(&["map"]), &view_names(&["map"]));
    assert_eq!(next_run.panels()["map"], killed_run_panels["map"]);
    assert_eq!(
        navigation_count(&fixture),
        3,
        "only the map panel moves to the new address"
    );
    assert!(
        fixture
            .calls_starting_with("gui_panels_close_panel")
            .is_empty(),
        "no leftover is closed"
    );
    assert!(
        panel_states(&connection)
            .contains(&("camera.rerun.rerun".to_owned(), GuiPanelState::Hidden)),
        "only the camera panel, left on the old address for a view not published, is hidden: {:?}",
        panel_states(&connection)
    );
    assert_eq!(
        open_panels(&connection),
        [
            (
                "camera.rerun.rerun".to_owned(),
                "http://192.168.50.40:9877/views/camera".to_owned()
            ),
            ("map.other.rerun.rerun".to_owned(), String::new(),),
            ("map.rerun-air_traffic.rerun".to_owned(), String::new(),),
            (
                "map.rerun.rerun".to_owned(),
                "http://10.0.0.5:9900/views/map".to_owned()
            ),
            ("status".to_owned(), String::new()),
        ],
        "panels of other apps and other volumes stay"
    );

    drop(next_run);
    drop(killed_run);
    fixture.assert_clean_teardown(connection);
}

#[test]
fn views_named_apart_get_panels_of_their_own() {
    let fixture = Fixture::new();
    let connection = fixture.connect(Some("ABCD-EFGH")).unwrap();
    let mut panels = panels_with_a_viewer(Rc::clone(&connection), "http://127.0.0.1:9877");
    panels.sync(
        &panel_views(&["plot", "plot-2"]),
        &view_names(&["plot", "plot-2"]),
    );
    assert_ne!(panels.panels()["plot"], panels.panels()["plot-2"]);
    assert_eq!(
        open_panel_uris(&connection),
        [
            "http://127.0.0.1:9877/views/plot",
            "http://127.0.0.1:9877/views/plot-2"
        ]
    );
    drop(panels);
    fixture.assert_clean_teardown(connection);
}

fn open_panel_uris(connection: &QarosConnection) -> Vec<String> {
    let mut uris: Vec<String> = connection
        .session()
        .gui_panels()
        .list()
        .unwrap()
        .into_iter()
        .filter(|panel| panel.state != GuiPanelState::Closed)
        .map(|panel| panel.content_uri)
        .collect();
    uris.sort();
    uris
}

const ZERO_ONBOARDING_ID: &str = "00000000-0000-0000-0000-000000000000";

fn onboarding_id_file(fixture: &Fixture) -> PathBuf {
    fixture.scratch.join("qaros-rerun.onboarding-id.txt")
}

/// Starts with `stored` in the onboarding id file and a pairing code; returns the id the
/// file holds afterwards.
fn pairs_instead_of_rejoining(fixture: &Fixture, stored: &str) -> String {
    std::fs::write(onboarding_id_file(fixture), stored).unwrap();
    let connection = fixture
        .connect(Some("ABCD-EFGH"))
        .expect("pairs with the code");
    fixture.assert_clean_teardown(connection);
    std::fs::read_to_string(onboarding_id_file(fixture))
        .unwrap()
        .trim_end()
        .to_owned()
}

fn is_nonzero_onboarding_id(text: &str) -> bool {
    text.len() == ZERO_ONBOARDING_ID.len() && text != ZERO_ONBOARDING_ID
}

#[test]
fn a_damaged_onboarding_id_file_falls_back_to_the_pairing_code() {
    let fixture = Fixture::new();
    let stored = pairs_instead_of_rejoining(&fixture, "not an onboarding id\n");
    assert!(is_nonzero_onboarding_id(&stored), "{stored:?}");
    let calls = fixture.calls();
    assert!(
        !calls.iter().any(|call| call == "runtime_rejoin"),
        "{calls:?}"
    );
}

#[test]
fn a_zero_onboarding_id_falls_back_to_the_pairing_code() {
    let fixture = Fixture::new();
    let stored = pairs_instead_of_rejoining(&fixture, &format!("{ZERO_ONBOARDING_ID}\n"));
    assert!(is_nonzero_onboarding_id(&stored), "{stored:?}");
}

#[test]
fn a_failed_rejoin_falls_back_to_the_pairing_code() {
    let fixture = Fixture::new();
    let first = fixture.connect(Some("ABCD-EFGH")).unwrap();
    fixture.assert_clean_teardown(first);
    let first_id = std::fs::read_to_string(onboarding_id_file(&fixture)).unwrap();

    fixture.fail_next("runtime_rejoin", UNCLASSIFIED);
    let stored = pairs_instead_of_rejoining(&fixture, &first_id);
    let calls = fixture.calls();
    assert!(
        calls.iter().any(|call| call == "runtime_rejoin"),
        "{calls:?}"
    );
    assert!(is_nonzero_onboarding_id(&stored), "{stored:?}");
    assert_ne!(stored, first_id.trim_end(), "a new onboarding id is stored");
}

#[test]
fn without_a_pairing_code_a_damaged_onboarding_id_file_stops_the_start() {
    let fixture = Fixture::new();
    std::fs::write(onboarding_id_file(&fixture), "not an onboarding id\n").unwrap();
    let error = fixture.connect(None).expect_err("no code to fall back to");
    assert!(error.contains("qaros-rerun.onboarding-id.txt"), "{error}");
}

#[test]
fn rejoins_with_the_stored_id_and_maps_app_space_poses_into_the_room() {
    let fixture = Fixture::new();
    let first = fixture.connect(Some("ABCD-EFGH")).unwrap();
    // The first connection's request is consumed; only a new subscription brings a new one.
    fixture.assert_clean_teardown(first);

    let connection = fixture.connect(None).unwrap();
    let calls = fixture.calls();
    assert!(
        calls.iter().any(|call| call == "runtime_rejoin"),
        "{calls:?}"
    );
    let mut sink = new_sink(&connection, PoseSpace::App);
    let request = single_request(&mut sink).expect("frame after rejoin");

    let room_from_app = placement().volume_pose * placement().app_pose;
    let expected_left = room_from_app.transform_point(Vec3::new(-0.032, 1.6, 0.0) * 2.0);
    assert!(
        (request.views[0].pose.position - expected_left).length() < 1e-5,
        "{:?} against {expected_left:?}",
        request.views[0].pose.position
    );
    let expected_orientation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
    assert!(
        request.views[0]
            .pose
            .orientation
            .abs_diff_eq(expected_orientation, 1e-5)
    );

    submit_both_eyes(&mut sink, request.key);
    sink.show_frame(request.key, show_info()).unwrap();
    assert!(
        fixture
            .calls()
            .iter()
            .any(|call| call == "shown near=0.025 far=10 depth_scale=1"),
        "near and far go back in app units, room meters / app scale 2: {:?}",
        fixture.calls_starting_with("shown")
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

#[cfg(feature = "vulkan")]
fn raw_image(texture: &wgpu::Texture) -> vulkan_device::vk::Image {
    // SAFETY: only the handle is read; the texture is alive.
    unsafe {
        texture
            .as_hal::<vulkan_device::VulkanApi>()
            .expect("a Vulkan texture")
            .raw_handle()
    }
}

/// A Vulkan sink on the test's device, for a session on the device's adapter, up to two users.
#[cfg(feature = "vulkan")]
fn vulkan_sink_on_stub(
    fixture: &Fixture,
    vulkan: &vulkan_device::VulkanDevice,
) -> (
    Rc<QarosConnection>,
    qaros_sink::vulkan::VulkanSink<qaros_sink::vulkan::QarosVulkanStream>,
) {
    use qaros_sink::vulkan::{
        QarosVulkanStream, QarosVulkanStreamOptions, VulkanSink, VulkanSinkOptions,
    };
    let mut options = connect_options(&fixture.scratch, Some("ABCD-EFGH"));
    options.adapter = Some(qaros::GraphicsAdapter::Uuid(
        vulkan.physical_device.device_uuid,
    ));
    let connection = Rc::new(connect(&options, wake_counter().1).unwrap());
    let stream = QarosVulkanStream::on_device(
        Rc::clone(&connection),
        vulkan,
        QarosVulkanStreamOptions {
            pose_space: PoseSpace::Room,
            color_format: PixelFormat::R8G8B8A8,
            max_users: 2,
        },
    )
    .unwrap();
    let sink = VulkanSink::new(
        &vulkan.device,
        &vulkan.queue,
        stream,
        VulkanSinkOptions {
            depth_range: depth_range(),
            stop_after_frames: None,
        },
    )
    .unwrap();
    (connection, sink)
}

/// Updates the Vulkan sink's users until the visualizer has its sender, without beginning.
#[cfg(feature = "vulkan")]
fn update_until_the_visualizer_has_a_sender(
    sink: &mut qaros_sink::vulkan::VulkanSink<qaros_sink::vulkan::QarosVulkanStream>,
) {
    use frame_sink::GpuFrameSink;
    let started = Instant::now();
    while sink.stream().rendered_users().is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no sender in 5 s"
        );
        sink.update_users();
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A begun Vulkan frame whose record cannot be read has no semaphore the viewer could signal,
/// so the sender would wait for good: the user's sender is released instead, and the target
/// comes back after its back-off.
#[cfg(feature = "vulkan")]
#[test]
fn a_vulkan_frame_whose_record_cannot_be_read_releases_the_users_sender() {
    use frame_sink::GpuFrameSink;
    let Some(vulkan) = common::create_device() else {
        return;
    };
    let fixture = Fixture::new();
    let (connection, mut sink) = vulkan_sink_on_stub(&fixture, &vulkan);
    update_until_the_visualizer_has_a_sender(&mut sink);

    let destroyed_before = count_calls(&fixture, "render_stream_handle_destroy");
    fixture.fail_next("render_sender_frame_vulkan", LOGIC_ERROR);
    assert!(sink.begin_frames().unwrap().is_empty());
    assert_eq!(
        count_calls(&fixture, "render_stream_handle_destroy"),
        destroyed_before + 1,
        "the user's sender is released"
    );
    assert!(sink.stream().rendered_users().is_empty());
    assert!(
        sink.begin_frames().unwrap().is_empty(),
        "the target waits out its back-off"
    );
    let errors = fixture.calls_starting_with("ERROR");
    assert!(errors.is_empty(), "{errors:?}");
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// On the Vulkan path too, begins that keep failing release the user after
/// [`FRAME_FAILURE_PERIOD`], at least [`FRAME_FAILURE_MINIMUM`] in a row.
#[cfg(feature = "vulkan")]
#[test]
fn a_vulkan_user_whose_begins_keep_failing_is_released() {
    use frame_sink::GpuFrameSink;
    let Some(vulkan) = common::create_device() else {
        return;
    };
    let fixture = Fixture::new();
    let (connection, mut sink) = vulkan_sink_on_stub(&fixture, &vulkan);
    update_until_the_visualizer_has_a_sender(&mut sink);

    for _ in 0..FRAME_FAILURE_MINIMUM {
        fixture.fail_next("render_sender_try_begin_frame", LOGIC_ERROR);
        assert!(sink.begin_frames().unwrap().is_empty());
    }
    assert_eq!(sink.stream().rendered_users().len(), 1, "still rendered");
    std::thread::sleep(FRAME_FAILURE_PERIOD);
    let destroyed_before = count_calls(&fixture, "render_stream_handle_destroy");
    fixture.fail_next("render_sender_try_begin_frame", LOGIC_ERROR);
    assert!(sink.begin_frames().unwrap().is_empty());
    assert!(sink.stream().rendered_users().is_empty());
    assert_eq!(
        count_calls(&fixture, "render_stream_handle_destroy"),
        destroyed_before + 1
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// On the Vulkan path too, a Stop lets the running begin-frame wait end before the sender goes
/// and signals the frame it yields, and waits no longer than [`BEGIN_WAIT_DRAIN_LIMIT`] for a
/// paused receiver's.
#[cfg(feature = "vulkan")]
#[test]
fn a_vulkan_stop_lets_begin_waits_end_before_the_senders_go_within_a_bound() {
    use frame_sink::GpuFrameSink;
    let Some(vulkan) = common::create_device() else {
        return;
    };
    let fixture = Fixture::new();
    fixture.set_try_begin_frame_delay(40);
    let leave_a_begin_waiting = |sink: &mut qaros_sink::vulkan::VulkanSink<_>| {
        for request in begin_until_requested(sink) {
            sink.abandon_frame(request.request.key);
        }
        assert!(
            sink.begin_frames().unwrap().is_empty(),
            "the begin waits for the stub's delay"
        );
    };

    let (connection, mut sink) = vulkan_sink_on_stub(&fixture, &vulkan);
    leave_a_begin_waiting(&mut sink);
    let stop = Instant::now();
    drop(sink);
    let stopped_after = stop.elapsed();
    assert_eq!(fixture.senders_destroyed_during_a_begin_wait(), 0);
    assert!(stopped_after < BEGIN_WAIT_DRAIN_LIMIT, "{stopped_after:?}");
    fixture.assert_clean_teardown(connection);

    let (connection, mut sink) = vulkan_sink_on_stub(&fixture, &vulkan);
    leave_a_begin_waiting(&mut sink);
    fixture.set_peer_paused(VISUALIZER_PEER, true);
    let stop = Instant::now();
    drop(sink);
    let stopped_after = stop.elapsed();
    assert_eq!(fixture.senders_destroyed_during_a_begin_wait(), 1);
    assert!(
        stopped_after >= BEGIN_WAIT_DRAIN_LIMIT
            && stopped_after < BEGIN_WAIT_DRAIN_LIMIT + DRAIN_TIMING_SLACK,
        "{stopped_after:?}"
    );
    fixture.assert_clean_teardown(connection);
}

/// A Vulkan sink whose visualizer left while a begin-frame wait of the stub's
/// `try_begin_frame` delay ran: the user is released, its sender parked.
#[cfg(feature = "vulkan")]
fn vulkan_sink_with_a_parked_sender(
    fixture: &Fixture,
    vulkan: &vulkan_device::VulkanDevice,
) -> (
    Rc<QarosConnection>,
    qaros_sink::vulkan::VulkanSink<qaros_sink::vulkan::QarosVulkanStream>,
) {
    use frame_sink::GpuFrameSink;
    let (connection, mut sink) = vulkan_sink_on_stub(fixture, vulkan);
    for request in begin_until_requested(&mut sink) {
        sink.abandon_frame(request.request.key);
    }
    assert!(
        sink.begin_frames().unwrap().is_empty(),
        "the begin waits for the stub's delay"
    );
    let destroyed_before = count_calls(fixture, "render_stream_handle_destroy");
    fixture.peer_leave(VISUALIZER_PEER);
    let started = Instant::now();
    while !sink.stream().rendered_users().is_empty() {
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "the visualizer's leave was not seen within the wait"
        );
        sink.update_users();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        count_calls(fixture, "render_stream_handle_destroy"),
        destroyed_before,
        "parked while its wait runs"
    );
    (connection, sink)
}

/// On the Vulkan path too, a sender released while its begin-frame wait runs is parked until
/// the wait ends, and a Stop drains the parked senders within the bound.
#[cfg(feature = "vulkan")]
#[test]
fn a_vulkan_sender_released_during_a_begin_wait_goes_once_the_wait_ends_or_at_stop() {
    use frame_sink::GpuFrameSink;
    let Some(vulkan) = common::create_device() else {
        return;
    };
    {
        let fixture = Fixture::new();
        fixture.set_try_begin_frame_delay(400);
        let (connection, mut sink) = vulkan_sink_with_a_parked_sender(&fixture, &vulkan);
        let destroyed_at_release = count_calls(&fixture, "render_stream_handle_destroy");
        let started = Instant::now();
        while count_calls(&fixture, "render_stream_handle_destroy") == destroyed_at_release {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "the parked sender never went"
            );
            sink.update_users();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fixture.senders_destroyed_during_a_begin_wait(), 0);
        drop(sink);
        fixture.assert_clean_teardown(connection);
    }
    let fixture = Fixture::new();
    fixture.set_try_begin_frame_delay(400);
    let (connection, sink) = vulkan_sink_with_a_parked_sender(&fixture, &vulkan);
    let stop = Instant::now();
    drop(sink);
    let stopped_after = stop.elapsed();
    assert_eq!(
        fixture.senders_destroyed_during_a_begin_wait(),
        0,
        "the Stop drained the parked sender's wait"
    );
    assert!(stopped_after < BEGIN_WAIT_DRAIN_LIMIT, "{stopped_after:?}");
    fixture.assert_clean_teardown(connection);
}

/// The first frame requests, once the users' senders, created in the background, arrived.
#[cfg(feature = "vulkan")]
fn begin_until_requested<Stream: qaros_sink::vulkan::VulkanFrameStream>(
    sink: &mut qaros_sink::vulkan::VulkanSink<Stream>,
) -> Vec<frame_sink::GpuFrameRequest> {
    use frame_sink::GpuFrameSink;
    let started = Instant::now();
    loop {
        let requests = sink.begin_frames().unwrap();
        if !requests.is_empty() {
            return requests;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "no frame request within 5 s"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(feature = "vulkan")]
#[test]
#[allow(clippy::too_many_lines)]
fn vulkan_frames_render_into_the_stub_sender_images() {
    use common::{ConsumedImage, color_of, render_eyes};
    use frame_sink::GpuFrameSink;
    use qaros_sink::vulkan::{
        QarosVulkanStream, QarosVulkanStreamOptions, VulkanSink, VulkanSinkOptions,
    };
    use vulkan_device::vk;

    let Some(vulkan) = common::create_device() else {
        return;
    };
    let raw = common::Raw::of(&vulkan.device);
    let fixture = Fixture::new();
    let stream_options = QarosVulkanStreamOptions {
        pose_space: PoseSpace::Room,
        color_format: PixelFormat::R8G8B8A8,
        max_users: 2,
    };

    let without_adapter = fixture.connect(Some("ABCD-EFGH")).unwrap();
    assert_eq!(without_adapter.adapter(), None, "the stub reports none");
    let accepted =
        QarosVulkanStream::on_device(Rc::clone(&without_adapter), &vulkan, stream_options);
    assert!(
        accepted.is_ok(),
        "a session that names no adapter takes the device as it is"
    );
    drop(accepted);
    fixture.assert_clean_teardown(without_adapter);

    let mut elsewhere = connect_options(&fixture.scratch, None);
    elsewhere.adapter = Some(qaros::GraphicsAdapter::Uuid([0xee; 16]));
    let on_another_adapter = Rc::new(connect(&elsewhere, wake_counter().1).unwrap());
    let refused =
        QarosVulkanStream::on_device(Rc::clone(&on_another_adapter), &vulkan, stream_options);
    assert!(refused.is_err(), "a session on another adapter");
    drop(refused);
    fixture.assert_clean_teardown(on_another_adapter);

    let device_adapter = qaros::GraphicsAdapter::Uuid(vulkan.physical_device.device_uuid);
    fixture.set_reported_adapter(Some(device_adapter));
    let reported_by_session = fixture.connect(None).unwrap();
    assert_eq!(
        reported_by_session.adapter(),
        Some(device_adapter),
        "the adapter the session reports, with nothing chained"
    );
    let on_session_adapter =
        QarosVulkanStream::on_device(Rc::clone(&reported_by_session), &vulkan, stream_options);
    assert!(
        on_session_adapter.is_ok(),
        "the device is on the session's adapter"
    );
    drop(on_session_adapter);
    fixture.assert_clean_teardown(reported_by_session);
    fixture.set_reported_adapter(None);

    let mut options = connect_options(&fixture.scratch, Some("ABCD-EFGH"));
    options.adapter = Some(qaros::GraphicsAdapter::Uuid(
        vulkan.physical_device.device_uuid,
    ));
    let connection = Rc::new(connect(&options, wake_counter().1).unwrap());
    assert_eq!(connection.adapter(), options.adapter);
    let adapter_line = format!(
        "adapter uuid={}",
        options.adapter.map(|adapter| adapter.to_string()).unwrap()
    );
    assert!(
        fixture.calls().contains(&adapter_line),
        "the rejoin carried the device's adapter: {:?}",
        fixture.calls()
    );
    let stream =
        QarosVulkanStream::on_device(Rc::clone(&connection), &vulkan, stream_options).unwrap();
    let mut sink = VulkanSink::new(
        &vulkan.device,
        &vulkan.queue,
        stream,
        VulkanSinkOptions {
            depth_range: depth_range(),
            stop_after_frames: Some(3),
        },
    )
    .unwrap();

    let frames = [
        ([[200_u8, 100, 50, 255], [10, 20, 30, 255]], 0.25_f32),
        ([[0, 255, 0, 128], [255, 0, 255, 255]], 0.5),
        ([[1, 2, 3, 4], [250, 251, 252, 253]], 0.75),
    ];
    let only_request = |sink: &mut VulkanSink<QarosVulkanStream>| {
        let mut requests = begin_until_requested(sink);
        assert_eq!(requests.len(), 1, "one visualizer, one frame");
        requests.remove(0)
    };
    for (frame_index, (colors, depth)) in frames.into_iter().enumerate() {
        let request = only_request(&mut sink);
        assert_eq!(request.request.key.user, frame_sink::UserId(1));
        assert_eq!(request.request.resolution_in_pixel, [64, 64]);
        // The stub steps the head back 1 mm per issued frame, along z.
        for (view, eye_x) in request.request.views.iter().zip([-0.032, 0.032]) {
            let position = view.pose.position;
            assert!(
                (position.truncate() - glam::Vec2::new(eye_x, 1.6)).length() < 1e-6,
                "frame {frame_index}: {position:?}"
            );
        }
        let consumed: Vec<ConsumedImage> = request
            .eyes
            .iter()
            .flat_map(|eye| {
                [
                    ConsumedImage {
                        image: raw_image(&eye.color),
                        aspect: vk::ImageAspectFlags::COLOR,
                        resolution_in_pixel: [64, 64],
                    },
                    ConsumedImage {
                        image: raw_image(eye.depth.as_ref().expect("a depth target")),
                        aspect: vk::ImageAspectFlags::DEPTH,
                        resolution_in_pixel: [64, 64],
                    },
                ]
            })
            .collect();
        let commands = render_eyes(&vulkan.device, &request, colors.map(color_of), depth);
        let key = request.request.key;
        drop(request);
        sink.submit_and_show(key, commands, show_info()).unwrap();

        // The stub waited for the frame's semaphore at show; the images are released to the
        // external queue family in TRANSFER_SRC_OPTIMAL, as written back.
        let copies = raw.consume(None, &consumed);
        for (eye_index, color) in colors.iter().enumerate() {
            assert!(
                copies[eye_index * 2]
                    .chunks_exact(4)
                    .all(|pixel| pixel == color),
                "frame {frame_index}, eye {eye_index}: {:?}",
                &copies[eye_index * 2][..4]
            );
            assert!(
                copies[eye_index * 2 + 1].chunks_exact(4).all(|value| {
                    (f32::from_ne_bytes(value.try_into().unwrap()) - depth).abs() < 1e-6
                }),
                "frame {frame_index}, eye {eye_index} depth"
            );
        }
        if frame_index == 0 {
            let abandoned = only_request(&mut sink);
            sink.abandon_frame(abandoned.request.key);
        }
    }
    assert!(!sink.is_streaming(), "stops after three shown frames");

    let calls = fixture.calls();
    let write_backs = calls
        .iter()
        .filter(|call| call.starts_with("vulkan write-back ok images=4"))
        .count();
    assert_eq!(write_backs, 3, "{calls:?}");
    assert!(
        calls
            .iter()
            .any(|call| call == "vulkan discarded frame: semaphore replaced"),
        "{calls:?}"
    );
    drop(sink);
    fixture.assert_clean_teardown(connection);
}

/// A show that fails after the hand-over leaves the frame's semaphore signalled with no show;
/// the header does not say whether the next frame reuses it. The stream gives up the user's
/// sender, as QuaternAR's cube gives up such a target, so that semaphore is never signalled
/// again, and the target comes back as a new user after its back-off.
#[cfg(feature = "vulkan")]
#[test]
fn a_vulkan_show_failing_after_the_hand_over_releases_the_users_sender() {
    use common::{color_of, render_eyes};
    use frame_sink::GpuFrameSink;

    let Some(vulkan) = common::create_device() else {
        return;
    };
    let fixture = Fixture::new();
    let (connection, mut sink) = vulkan_sink_on_stub(&fixture, &vulkan);

    let mut requests = begin_until_requested(&mut sink);
    assert_eq!(requests.len(), 1, "one visualizer, one frame");
    let request = requests.remove(0);
    let key = request.request.key;
    let commands = render_eyes(
        &vulkan.device,
        &request,
        [color_of([200, 100, 50, 255]); 2],
        0.5,
    );
    drop(request);
    let senders_destroyed_before = fixture
        .calls_starting_with("render_stream_handle_destroy")
        .len();
    fixture.fail_next("render_sender_show_frame", LOGIC_ERROR);
    assert!(
        sink.submit_and_show(key, commands, show_info()).is_err(),
        "the injected show failure"
    );
    assert_eq!(
        fixture
            .calls_starting_with("render_stream_handle_destroy")
            .len(),
        senders_destroyed_before + 1,
        "the user's sender is released"
    );
    assert!(sink.stream().rendered_users().is_empty());
    assert!(!sink.is_streaming());
    assert!(
        sink.begin_frames().unwrap().is_empty(),
        "the target waits out its back-off"
    );
    let errors = fixture.calls_starting_with("ERROR");
    assert!(errors.is_empty(), "{errors:?}");
    drop(sink);
    fixture.assert_clean_teardown(connection);
}
