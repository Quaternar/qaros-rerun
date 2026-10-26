//! `--sink qaros`: connects to the QAROS runtime before the viewer starts and builds the sink
//! and the GUI panel hook. All QAROS handles stay on the UI thread from here on.
//!
//! With `--qaros-graphics vulkan` the order is the header's ("Basic flow", `b1338df`): onboard,
//! read the session's adapter (`qar_session_get_graphics_device_id`), then create the viewer's
//! own Vulkan device on it; the sink renders into each user's Vulkan render sender images.
//! `--vulkan-device` overrides that: its device exists first and its adapter is chained into
//! onboarding. With `--launched-by-qaros` the launch invite comes from stdin and the viewer
//! stops when stdin closes ("Launched by QAROS" in `onboarding-and-sessions.mdx`).

use std::collections::HashMap;
use std::io::BufRead as _;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use panel_server::ViewFrames;
use qaros_sink::qaros::{GraphicsAdapter, PixelFormat};
use qaros_sink::vulkan::{
    QarosVulkanStream, QarosVulkanStreamOptions, VulkanSink, VulkanSinkOptions,
};
use qaros_sink::{
    ConnectOptions, PanelActivity, PanelView, PoseSpace, QarosConnection, QarosPanels, QarosSink,
    QarosSinkOptions, WakeViewer, connect, end_process_after_teardown,
    library_path_from_environment, runtime_directory_from_environment, start_stop_watchdog,
    stop_on_console_events, storage_directory_from_environment,
};
use vulkan_device::{PhysicalDeviceIdentity, PhysicalDeviceSelector, VulkanDevice};

use crate::device_setup::GraphicsChoice;
use crate::headset::HeadsetSink;
use crate::options::{ColorFormatKind, Options, PoseSpaceKind, QarosGraphics};
use crate::viewer_app::{FrameHook, HeadsetFrame, StopRequest};

const PANEL_SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// How long a view must have been published before it gets a panel.
const PANEL_SETTLE: Duration = Duration::from_secs(3);
/// How recently a view's frame must have changed for it to count as still published.
const PANEL_LIVE: Duration = Duration::from_millis(1500);

pub struct QarosSetup {
    pub sink: HeadsetSink,
    pub frame_hooks: Vec<Box<dyn FrameHook>>,
    /// Set on Ctrl+C, Ctrl+Break or a closed console, and when QAROS closes stdin with
    /// `--launched-by-qaros`.
    pub stop_request: StopRequest,
    /// The viewer's own Vulkan device: `named_vulkan` passed through, or the one created on
    /// the session's adapter for `--qaros-graphics vulkan`.
    pub vulkan: Option<VulkanDevice>,
}

/// `named_vulkan` is the `--vulkan-device` device, created before the session.
pub async fn build(
    options: &Options,
    graphics: &GraphicsChoice,
    wake_viewer: WakeViewer,
    panel_frames: Option<ViewFrames>,
    named_vulkan: Option<VulkanDevice>,
) -> Result<QarosSetup, String> {
    let qaros_options = &options.qaros;
    let launch_line = if qaros_options.launched_by_qaros && !qaros_options.local_app {
        re_log::info!("qaros: waiting for the launch invite on stdin (--launched-by-qaros)");
        Some(read_launch_line()?)
    } else {
        None
    };
    // Watched from here on, so a Stop during onboarding is seen as soon as `connect` returns.
    // Ctrl+C takes the same path: the console's default handler would exit with the library
    // loaded and run its unload code.
    let stop_request: StopRequest = Arc::new(AtomicBool::new(false));
    if let Err(error) = stop_on_console_events(stop_and_wake(
        &stop_request,
        Arc::clone(&wake_viewer),
        start_stop_watchdog,
    )) {
        re_log::warn!("qaros: Ctrl+C will end the viewer without its QAROS teardown ({error})");
    }
    if qaros_options.launched_by_qaros {
        stop_when_closed(
            std::io::stdin(),
            stop_and_wake(&stop_request, Arc::clone(&wake_viewer), start_stop_watchdog),
        );
    }
    let renders_on_vulkan = qaros_options.graphics() == QarosGraphics::Vulkan;
    let adapter = named_vulkan
        .as_ref()
        .filter(|_| renders_on_vulkan)
        .map(|vulkan| session_adapter(&vulkan.physical_device));
    if let Some(adapter) = adapter {
        re_log::info!("qaros: joining the session on adapter {adapter} (--vulkan-device)");
    }
    let connect_options = ConnectOptions {
        library_path: qaros_options
            .library_path
            .clone()
            .or_else(library_path_from_environment),
        runtime_binaries_directory: runtime_directory_from_environment(),
        storage_directory: storage_directory_from_environment(),
        onboarding_id_file: qaros_options.onboarding_id_file(),
        unanswered_peers_file: Some(qaros_options.unanswered_peers_file()),
        pairing_code: qaros_options.pairing_code.clone(),
        launch_line,
        local_app: qaros_options.local_app,
        peer_display_name: format!("{} viewer", qaros_options.display_name),
        volume_name: qaros_options.volume_name.clone(),
        volume_display_name: qaros_options.display_name.clone(),
        volume_size: qaros_options.volume_size.to_array(),
        placement: options.placement(),
        console_logging: true,
        raise_gpu_priority: qaros_options.raise_gpu_priority,
        adapter,
    };
    if let Some(path) = &connect_options.library_path {
        re_log::info!("qaros: loading {}", path.display());
    } else {
        re_log::info!("qaros: loading the QAROS runtime by the default lookup");
    }
    let connection = Rc::new(connect(&connect_options, Arc::clone(&wake_viewer))?);
    if is_requested(Some(&stop_request)) {
        re_log::info!("qaros: stopped during onboarding; leaving the session and exiting");
        drop(connection);
        end_process_after_teardown(0);
    }
    re_log::info!(
        "qaros: connected; app volume {:?} ready, looking for target peers",
        qaros_options.volume_name
    );

    let vulkan = match named_vulkan {
        Some(vulkan) => Some(vulkan),
        None if renders_on_vulkan => {
            let selector = device_selector(connection.adapter());
            re_log::info!("qaros: creating the viewer's Vulkan device on {selector:?}");
            Some(graphics.create_vulkan_device(&selector).await?)
        }
        None => None,
    };
    let sink = match vulkan.as_ref().filter(|_| renders_on_vulkan) {
        Some(vulkan) => vulkan_sink(options, &connection, vulkan)?,
        None => cpu_sink(options, &connection),
    };

    let mut frame_hooks: Vec<Box<dyn FrameHook>> = Vec::new();
    if let (Some(frames), Some(base_url)) = (panel_frames, options.panel_base_url()) {
        re_log::info!("qaros: 2D views open as GUI panels from {base_url}");
        frame_hooks.push(Box::new(PanelHook {
            panels: QarosPanels::new(connection, &base_url)
                .with_panels_at_stop(panels_at_stop(options))
                .with_dead_pages(dead_pages(options)),
            frames,
            last_sync: None,
            activity: HashMap::new(),
            user_stream_started: false,
        }));
    }
    Ok(QarosSetup {
        sink,
        frame_hooks,
        stop_request,
        vulkan,
    })
}

/// The physical device of the session's adapter. The launch invite carries the GPU only when
/// the target advertised one (`onboarding-and-sessions.mdx` L250 at `b1338df`), so a session
/// may name none; then the guess is the first discrete NVIDIA GPU, else the first discrete GPU,
/// else the first device (the "creating the device on ..." line names it, with its LUID).
fn device_selector(session_adapter: Option<GraphicsAdapter>) -> PhysicalDeviceSelector {
    match session_adapter {
        Some(GraphicsAdapter::Luid(luid)) => PhysicalDeviceSelector::Luid(luid),
        Some(GraphicsAdapter::Uuid(uuid)) => PhysicalDeviceSelector::Uuid(uuid),
        None => {
            re_log::warn!(
                "qaros: the session names no adapter; falling back to the first discrete NVIDIA \
                 GPU, else the first discrete GPU, else the first device"
            );
            PhysicalDeviceSelector::Discrete
        }
    }
}

fn pose_space(options: &Options) -> PoseSpace {
    match options.qaros.pose_space {
        PoseSpaceKind::Room => PoseSpace::Room,
        PoseSpaceKind::App => PoseSpace::App,
    }
}

fn dead_pages(options: &Options) -> qaros_sink::DeadPages {
    match options.qaros.dead_panels {
        crate::options::DeadPanelsKind::Reload => qaros_sink::DeadPages::Reload,
        crate::options::DeadPanelsKind::Leave => qaros_sink::DeadPages::Leave,
    }
}

fn panels_at_stop(options: &Options) -> qaros_sink::PanelsAtStop {
    match options.qaros.panels_at_stop {
        crate::options::PanelsAtStopKind::Hide => qaros_sink::PanelsAtStop::Hide,
        crate::options::PanelsAtStopKind::Keep => qaros_sink::PanelsAtStop::Keep,
    }
}

fn color_format(options: &Options) -> PixelFormat {
    match options.qaros.color_format {
        ColorFormatKind::Bgra => PixelFormat::B8G8R8A8,
        ColorFormatKind::Rgba => PixelFormat::R8G8B8A8,
    }
}

fn cpu_sink(options: &Options, connection: &Rc<QarosConnection>) -> HeadsetSink {
    HeadsetSink::Cpu(Box::new(QarosSink::new(
        Rc::clone(connection),
        QarosSinkOptions {
            depth_range: options.depth_range,
            pose_space: pose_space(options),
            color_format: color_format(options),
            max_users: options.max_users,
            stop_after_frames: options.stop_after_frames,
        },
    )))
}

fn vulkan_sink(
    options: &Options,
    connection: &Rc<QarosConnection>,
    vulkan: &VulkanDevice,
) -> Result<HeadsetSink, String> {
    let stream = QarosVulkanStream::on_device(
        Rc::clone(connection),
        vulkan,
        QarosVulkanStreamOptions {
            pose_space: pose_space(options),
            color_format: color_format(options),
            max_users: options.max_users,
        },
    )
    .map_err(|error| error.to_string())?;
    let sink = VulkanSink::new(
        &vulkan.device,
        &vulkan.queue,
        stream,
        VulkanSinkOptions {
            depth_range: options.depth_range,
            stop_after_frames: options.stop_after_frames,
        },
    )
    .map_err(|error| error.to_string())?;
    re_log::info!("qaros: frames go straight into the Vulkan render senders' images");
    Ok(HeadsetSink::Gpu(Box::new(sink)))
}

/// The adapter id QAROS gets for the viewer's device: the LUID on Windows (where the header
/// accepts LUIDs), the UUID elsewhere.
fn session_adapter(physical_device: &PhysicalDeviceIdentity) -> GraphicsAdapter {
    match physical_device.device_luid {
        Some(luid) if cfg!(target_os = "windows") => GraphicsAdapter::Luid(luid),
        _ => GraphicsAdapter::Uuid(physical_device.device_uuid),
    }
}

/// The first line of stdin: the launch invite QAROS writes there (one line of JSON), or a
/// code typed by hand. Never logged.
fn read_launch_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|error| format!("reading the launch line from stdin: {error}"))?;
    let launch_line = line.trim_end_matches(['\r', '\n']);
    if launch_line.is_empty() {
        return Err("--launched-by-qaros: stdin carried no launch invite or code".to_owned());
    }
    Ok(launch_line.to_owned())
}

fn is_requested(stop_request: Option<&StopRequest>) -> bool {
    stop_request.is_some_and(|stop_request| stop_request.load(Ordering::Acquire))
}

/// Sets `stop_request`, starts the stop's deadline (`start_watchdog`, the process's
/// [`start_stop_watchdog`] outside tests) and wakes the viewer, which then stops at its next
/// frame.
fn stop_and_wake(
    stop_request: &StopRequest,
    wake_viewer: WakeViewer,
    start_watchdog: fn(),
) -> impl Fn() + Send + Sync + 'static {
    let stop_request = Arc::clone(stop_request);
    move || {
        stop_request.store(true, Ordering::Release);
        start_watchdog();
        wake_viewer();
    }
}

/// Watches `input` (stdin) on a thread of its own and calls `stop` when QAROS closes it.
fn stop_when_closed(
    mut input: impl std::io::Read + Send + 'static,
    stop: impl FnOnce() + Send + 'static,
) {
    let spawned = std::thread::Builder::new()
        .name("qaros stdin".to_owned())
        .spawn(move || {
            let drained = std::io::copy(&mut input, &mut std::io::sink());
            match drained {
                Ok(_) => re_log::info!("qaros: stdin closed; stopping"),
                Err(error) => re_log::warn!("qaros: reading stdin failed ({error}); stopping"),
            }
            stop();
        });
    if let Err(error) = spawned {
        re_log::error!("qaros: cannot watch stdin ({error}); the viewer will not stop on its own");
    }
}

/// Opens a GUI panel for each 2D view the panel server keeps publishing, checked once a second.
struct PanelHook {
    panels: QarosPanels,
    frames: ViewFrames,
    last_sync: Option<Instant>,
    activity: HashMap<String, ViewActivity>,
    user_stream_started: bool,
}

/// The panel server's requests and whether a user stream has started in this run.
struct PanelServerActivity<'a> {
    frames: &'a ViewFrames,
    user_stream_started: bool,
}

impl PanelActivity for PanelServerActivity<'_> {
    fn last_page_request(&self, view_name: &str) -> Option<Instant> {
        self.frames.last_page_request(view_name)
    }

    fn any_page_request(&self) -> bool {
        self.frames.any_page_request()
    }

    fn user_stream_started(&self) -> bool {
        self.user_stream_started
    }
}

/// When a published view was first seen and when its frame last changed, as seen by the
/// once-a-second sync.
#[derive(Clone, Copy, Debug)]
struct ViewActivity {
    first_seen: Instant,
    sequence: u64,
    last_published: Instant,
}

impl ViewActivity {
    fn new(sequence: u64, now: Instant) -> Self {
        Self {
            first_seen: now,
            sequence,
            last_published: now,
        }
    }

    fn observe(&mut self, sequence: u64, now: Instant) {
        if sequence != self.sequence {
            self.sequence = sequence;
            self.last_published = now;
        }
    }

    /// A view gets a panel once it has been published for `PANEL_SETTLE` and still is. A
    /// replayed recording can carry its own blueprint, active for a moment while it loads; its
    /// views are published briefly and then never again, so they get no panel.
    fn is_settled(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.first_seen) >= PANEL_SETTLE
            && now.saturating_duration_since(self.last_published) <= PANEL_LIVE
    }
}

impl FrameHook for PanelHook {
    fn on_viewer_frame(&mut self, headset: HeadsetFrame) {
        self.user_stream_started |= headset.is_streaming;
        // New panels open within reach of the first head near the app volume; QarosPanels
        // ignores heads after that one, and heads too far away to reach anything.
        if let Some(head) = headset.head {
            self.panels.consider_head(head);
        }
        let now = Instant::now();
        if self
            .last_sync
            .is_some_and(|last| now.saturating_duration_since(last) < PANEL_SYNC_INTERVAL)
        {
            return;
        }
        self.last_sync = Some(now);
        let mut views = Vec::new();
        let published_view_names = self.frames.view_names();
        for name in published_view_names.iter().cloned() {
            let Some(frame) = self.frames.latest(&name) else {
                continue;
            };
            let activity = self
                .activity
                .entry(name.clone())
                .and_modify(|activity| activity.observe(frame.sequence, now))
                .or_insert_with(|| ViewActivity::new(frame.sequence, now));
            if activity.is_settled(now) {
                views.push(PanelView {
                    name,
                    width_in_pixel: frame.width,
                    height_in_pixel: frame.height,
                });
            }
        }
        self.panels.sync(&views, &published_view_names);
        let activity = PanelServerActivity {
            frames: &self.frames,
            user_stream_started: self.user_stream_started,
        };
        self.panels.reload_dead_pages(&activity, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_watched_input_requests_a_stop_and_wakes_the_viewer() {
        let (reader, mut writer) = std::io::pipe().expect("a pipe");
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let stop_request: StopRequest = Arc::new(AtomicBool::new(false));
        stop_when_closed(
            reader,
            stop_and_wake(
                &stop_request,
                Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                }),
                count_stdin_watchdog_start,
            ),
        );
        std::io::Write::write_all(&mut writer, b"more lines after the launch line\n")
            .expect("writing");
        std::thread::sleep(Duration::from_millis(50));
        assert!(!is_requested(Some(&stop_request)), "open input: no stop");
        assert_eq!(wakes.load(Ordering::SeqCst), 0);

        drop(writer);
        let started = Instant::now();
        while wakes.load(Ordering::SeqCst) == 0 {
            assert!(started.elapsed() < Duration::from_secs(5), "never woken");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            is_requested(Some(&stop_request)),
            "stop set before the wake"
        );
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "the viewer is woken once");
        assert!(!is_requested(None), "no watcher, no stop");
        assert_eq!(
            STDIN_WATCHDOG_STARTS.load(Ordering::SeqCst),
            1,
            "the closed input started the stop's deadline"
        );
    }

    #[test]
    fn a_console_stop_requests_the_same_stop_as_qaros_and_wakes_the_viewer() {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let stop_request: StopRequest = Arc::new(AtomicBool::new(false));
        let on_ctrl_c = stop_and_wake(
            &stop_request,
            Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
            count_console_watchdog_start,
        );
        assert!(!is_requested(Some(&stop_request)));
        assert_eq!(CONSOLE_WATCHDOG_STARTS.load(Ordering::SeqCst), 0);

        std::thread::spawn(on_ctrl_c)
            .join()
            .expect("the handler thread");

        assert!(is_requested(Some(&stop_request)));
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert_eq!(
            CONSOLE_WATCHDOG_STARTS.load(Ordering::SeqCst),
            1,
            "the console stop started the stop's deadline"
        );
    }

    static STDIN_WATCHDOG_STARTS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    static CONSOLE_WATCHDOG_STARTS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    fn count_stdin_watchdog_start() {
        STDIN_WATCHDOG_STARTS.fetch_add(1, Ordering::SeqCst);
    }

    fn count_console_watchdog_start() {
        CONSOLE_WATCHDOG_STARTS.fetch_add(1, Ordering::SeqCst);
    }

    /// One sync a second over `published_until_seconds`, the view's frame changing at every
    /// sync while published; returns the syncs (in seconds) at which the view is settled.
    fn settled_syncs(published_until_seconds: u64, last_sync_seconds: u64) -> Vec<u64> {
        let start = Instant::now();
        let mut activity = ViewActivity::new(0, start);
        let mut sequence = 0;
        let mut settled = Vec::new();
        for second in 1..=last_sync_seconds {
            let now = start + Duration::from_secs(second);
            if second <= published_until_seconds {
                sequence += 1;
            }
            activity.observe(sequence, now);
            if activity.is_settled(now) {
                settled.push(second);
            }
        }
        settled
    }

    #[test]
    fn a_view_published_throughout_gets_its_panel_after_the_settle_time() {
        assert_eq!(settled_syncs(10, 5), vec![3, 4, 5]);
    }

    #[test]
    fn a_view_published_only_while_a_recording_loads_never_gets_a_panel() {
        assert_eq!(settled_syncs(1, 10), Vec::<u64>::new());
    }

    #[test]
    fn a_view_that_stops_being_published_is_no_longer_settled() {
        assert_eq!(settled_syncs(4, 8), vec![3, 4, 5]);
    }

    #[test]
    fn the_device_is_picked_by_the_session_adapter() {
        let luid = [0xb1, 0x5f, 0x0d, 0x04, 0, 0, 0, 0];
        assert_eq!(
            device_selector(Some(GraphicsAdapter::Luid(luid))),
            PhysicalDeviceSelector::Luid(luid)
        );
        let uuid = [7; 16];
        assert_eq!(
            device_selector(Some(GraphicsAdapter::Uuid(uuid))),
            PhysicalDeviceSelector::Uuid(uuid)
        );
        assert_eq!(device_selector(None), PhysicalDeviceSelector::Discrete);
    }
}
