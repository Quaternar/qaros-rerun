//! The stream route's raw calls against `qaros-stub`, which implements no video sender,
//! `set_stream`, content type or gesture call: the listing that reads the content type first
//! agrees with the `qaros` crate's listing for website panels, and the stream route refuses
//! gracefully, creating no panel. The real behaviour is checked on a QAROS PC.
//!
//! The library is process-wide, so the tests run one at a time.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, unsafe_code)]

use std::ffi::c_char;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use frame_sink::UserId;
use glam::Vec3;
use qaros::GuiPanelConfig;
use qaros_sink::{
    ConnectOptions, HEADSET_WAIT, PanelContent, PanelImages, PanelView, QarosConnection,
    QarosPanels, connect, list_panels,
};
use xr_math::{AppPlacement, Pose};

static SERIAL: Mutex<()> = Mutex::new(());

/// Cargo names the stub `qaros_stub` from a path and `qaros_stub-<hash>` from git; the newest
/// match next to the test executable wins.
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
    .filter(|entry| {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        file_name
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .is_some_and(|stem| {
                stem.is_empty()
                    || stem.strip_prefix('-').is_some_and(|hash| {
                        !hash.is_empty()
                            && hash.chars().all(|character| character.is_ascii_hexdigit())
                    })
            })
    })
    .max_by_key(|entry| {
        entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
    })
    .unwrap_or_else(|| panic!("{prefix}{suffix} not found next to the test executable"))
    .path()
}

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
        let scratch = std::env::temp_dir().join(format!("qaros-sink-stream-test-{unique}"));
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

    fn connect(&self) -> Rc<QarosConnection> {
        let options = ConnectOptions {
            library_path: Some(stub_path()),
            runtime_binaries_directory: None,
            storage_directory: Some(self.scratch.join("storage")),
            onboarding_id_file: self.scratch.join("qaros-rerun.onboarding-id.txt"),
            unanswered_peers_file: None,
            pairing_code: Some("ABCD-EFGH".to_owned()),
            launch_line: None,
            local_app: false,
            peer_display_name: "qaros-rerun stream test viewer".to_owned(),
            volume_name: "rerun".to_owned(),
            volume_display_name: "qaros-rerun stream test".to_owned(),
            volume_size: [2.0, 2.0, 1.5],
            placement: AppPlacement {
                volume_pose: Pose::from_translation(Vec3::new(0.0, 0.5, -1.6)),
                ..AppPlacement::default()
            },
            console_logging: false,
            raise_gpu_priority: false,
            adapter: None,
        };
        Rc::new(connect(&options, Arc::new(|| {})).expect("connect to the stub"))
    }

    fn assert_clean_teardown(&self, connection: Rc<QarosConnection>) {
        assert_eq!(Rc::strong_count(&connection), 1, "panels dropped first");
        drop(connection);
        let errors = self.calls_starting_with("ERROR");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(self.live_handle_count(), 0, "every handle released");
    }
}

/// The stub names no device category, so a head near the volume places panels once it has
/// streamed for [`HEADSET_WAIT`].
fn place_panels(panels: &mut QarosPanels) {
    let head = Pose::from_translation(Vec3::new(0.0, 1.6, 0.0));
    let first_seen = Instant::now();
    panels.consider_head(UserId(1), head, first_seen);
    panels.consider_head(UserId(1), head, first_seen + HEADSET_WAIT);
}

fn open_website_panel(connection: &QarosConnection, common_name: &str, uri: &str) {
    let gui_panels = connection.session().gui_panels();
    let mut config = GuiPanelConfig::new(common_name);
    config.display_name = Some(common_name.to_owned());
    config.size.height = 0.3;
    let panel = gui_panels.get_or_create(&config).unwrap();
    gui_panels.navigate_to_uri(panel, uri).unwrap();
}

#[test]
fn the_content_type_first_listing_matches_the_crate_listing_for_website_panels() {
    let fixture = Fixture::new();
    let connection = fixture.connect();
    open_website_panel(
        &connection,
        "camera.rerun.rerun",
        "http://192.168.50.40:9877/views/camera",
    );
    open_website_panel(
        &connection,
        "plot.rerun.rerun",
        "http://192.168.50.40:9877/views/plot",
    );
    let plot = connection
        .session()
        .gui_panels()
        .list()
        .unwrap()
        .into_iter()
        .find(|panel| panel.display_name == "plot.rerun.rerun")
        .unwrap();
    connection
        .session()
        .gui_panels()
        .set_state(plot.id, qaros::GuiPanelState::Hidden)
        .unwrap();

    let crate_listing = connection.session().gui_panels().list().unwrap();
    let raw_listing = list_panels(&connection).unwrap();
    assert_eq!(raw_listing.len(), 2);
    assert_eq!(raw_listing.len(), crate_listing.len());
    for (raw, listed) in raw_listing.iter().zip(&crate_listing) {
        assert_eq!(raw.id, listed.id);
        assert_eq!(raw.display_name, listed.display_name);
        assert_eq!(raw.pose, listed.pose);
        assert_eq!(raw.size, listed.size);
        assert_eq!(raw.state, listed.state);
        assert_eq!(raw.content, PanelContent::Uri(listed.content_uri.clone()));
    }
    assert!(
        raw_listing
            .iter()
            .any(|panel| panel.state == qaros::GuiPanelState::Hidden)
    );
    assert_eq!(
        fixture
            .calls_starting_with("gui_panel_get_content_type")
            .len(),
        2,
        "the content type is asked first, per panel"
    );
    fixture.assert_clean_teardown(connection);
}

#[test]
fn the_stream_route_without_video_senders_refuses_once_and_creates_no_panel() {
    let fixture = Fixture::new();
    let connection = fixture.connect();
    let images = PanelImages::default();
    images
        .publish_rgba8("camera", 4, 2, vec![128; 4 * 2 * 4])
        .unwrap();
    let mut panels = QarosPanels::streaming(Rc::clone(&connection), images);
    place_panels(&mut panels);
    let views = [PanelView {
        name: "camera".to_owned(),
        width_in_pixel: 4,
        height_in_pixel: 2,
    }];
    let published = vec!["camera".to_owned()];

    panels.sync(&views, &published);
    panels.pump_streams(Instant::now());
    panels.sync(&views, &published);

    assert!(panels.panels().is_empty());
    assert_eq!(
        fixture.calls_starting_with("video_sender_create").len(),
        1,
        "refused once, not retried"
    );
    assert!(
        fixture
            .calls_starting_with("gui_panels_get_or_create")
            .is_empty(),
        "no panel without a sender"
    );
    assert!(
        fixture
            .calls_starting_with("gui_panels_navigate_to_uri")
            .is_empty()
    );
    drop(panels);
    assert!(
        fixture
            .calls_starting_with("gui_panels_set_state")
            .is_empty(),
        "nothing held, nothing hidden"
    );
    fixture.assert_clean_teardown(connection);
}

#[test]
fn the_stream_route_leaves_web_panels_of_earlier_runs_untouched() {
    let fixture = Fixture::new();
    let connection = fixture.connect();
    open_website_panel(
        &connection,
        "camera.rerun.rerun",
        "http://192.168.50.40:9877/views/camera",
    );
    let calls_before = fixture.calls().len();
    let mut panels = QarosPanels::streaming(Rc::clone(&connection), PanelImages::default());
    place_panels(&mut panels);
    let views = [PanelView {
        name: "camera".to_owned(),
        width_in_pixel: 4,
        height_in_pixel: 2,
    }];
    panels.sync(&views, &["camera".to_owned()]);
    drop(panels);
    let panel_changes: Vec<String> = fixture.calls()[calls_before..]
        .iter()
        .filter(|call| call.starts_with("gui_panels_"))
        .cloned()
        .collect();
    assert!(panel_changes.is_empty(), "{panel_changes:?}");
    fixture.assert_clean_teardown(connection);
}
