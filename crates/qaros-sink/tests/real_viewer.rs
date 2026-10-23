//! The viewer's QAROS sink end to end against the real runtime, with no human and no Hub:
//! this test process loads the QAROS package and QuaternAR's test extension, starts an
//! extension hub and one extension viewer peer per target, mints a launch invite, and starts
//! the release viewer as a QAROS API app would be started (`--launched-by-qaros`, the invite
//! on stdin, headless). The viewer finds the targets in the peer list and creates a sender per
//! target; each peer's render receiver requests frames, and every frame of every target must
//! arrive in order, with the scene in it.
//!
//! Needs `QAROS_PACKAGE_BIN` (skipped otherwise), the feature `e2e`, and the release viewer
//! (`cargo build --release -p qaros-rerun`), found as `target/release/qaros-rerun` next to
//! `log-test-scene` unless `QAROS_E2E_VIEWER` names it:
//! `cargo test --release -p qaros-sink --features e2e --test real_viewer -- --nocapture`.
//! `QAROS_E2E_GRAPHICS=cpu|vulkan` (default cpu), `QAROS_E2E_EYE=<width>x<height>` (default
//! 960x1000), `QAROS_E2E_FRAMES` (frames per target, default 120), `QAROS_E2E_TARGETS`
//! (default 1), `QAROS_E2E_VULKAN_DEVICE` (a `--vulkan-device` selector; unset, the viewer
//! creates its device on the adapter the session reports).
//! Every target requests its frame before the test waits for any, as independent headsets
//! would; `QAROS_E2E_REQUEST_AHEAD=0` makes each target's request wait for the previous
//! target's frame instead. `QAROS_E2E_IN_FLIGHT=<k>` keeps k requests outstanding per target,
//! so a request is always pending, the way a headset requests every display frame: the rate
//! is then the viewer's throughput. QAROS keeps one pending request per stream (a new one
//! replaces it), so k requests in flight yield k - 1 frames fewer than requests; the test
//! sends k - 1 extra requests at the end, and pairs requests with arrivals only for k = 1.
//! The receiver lends one frame at a time, lent again as soon as a frame has been read.
//! `QAROS_E2E_RECEIVER=vulkan` lends receiver frames as Vulkan images on the extension's
//! device, as a headset would receive them (no copy back to the CPU); the scene check then
//! reads every 30th frame back. Default `cpu`: the receiver's own CPU buffers, every frame
//! checked.
//! `QAROS_E2E_WINDOWED=1` starts the viewer in a window instead of headless.
//! `QAROS_E2E_TIMING_LOG` names a file for each request and arrival with the wall clock
//! in microseconds, to line up with the viewer's `QAROS_RERUN_PERF_LOG`.

#![cfg(feature = "e2e")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    unsafe_code
)]

use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use qaros::qaros_sys::{QarPixelFormat, QarVideoFrameLayout, QarVideoFrameViewType, defaults};
use qaros::{Library, LibraryConfig, LogSeverity};
use qaros_ext::{Extension, InviteMethod, Peer, RenderReceiver, VulkanDevice, VulkanTestFrame};

const VIEWER_PORT: u16 = 19876;
const FIRST_FRAME_BUDGET: Duration = Duration::from_mins(2);
const FRAME_BUDGET: Duration = Duration::from_secs(10);
/// How long a target after the first may take to show up in the peer list.
const PEER_LIST_BUDGET: Duration = Duration::from_secs(30);

#[test]
fn the_viewer_streams_the_test_scene_to_an_extension_receiver() {
    let Some(bin) = std::env::var_os("QAROS_PACKAGE_BIN").map(PathBuf::from) else {
        eprintln!("skipped: set QAROS_PACKAGE_BIN to the QAROS package's bin folder");
        return;
    };
    let graphics = std::env::var("QAROS_E2E_GRAPHICS").unwrap_or_else(|_| "cpu".to_owned());
    let (eye_width, eye_height) = eye_size();
    let frames_per_target: u64 = std::env::var("QAROS_E2E_FRAMES")
        .ok()
        .map_or(120, |text| text.parse().expect("QAROS_E2E_FRAMES"));
    let target_count: u64 = std::env::var("QAROS_E2E_TARGETS")
        .ok()
        .map_or(1, |text| text.parse().expect("QAROS_E2E_TARGETS"));
    let frames_in_flight: u64 = std::env::var("QAROS_E2E_IN_FLIGHT")
        .ok()
        .map_or(1, |text| text.parse().expect("QAROS_E2E_IN_FLIGHT"))
        .max(1);
    let request_ahead = frames_in_flight > 1
        || std::env::var("QAROS_E2E_REQUEST_AHEAD").map_or(true, |value| value != "0");
    let vulkan_receiver = std::env::var("QAROS_E2E_RECEIVER").is_ok_and(|kind| kind == "vulkan");
    let mut timing_log = std::env::var_os("QAROS_E2E_TIMING_LOG")
        .map(|path| BufWriter::new(File::create(path).expect("create the timing log")));
    let scratch = std::env::temp_dir().join("qaros-rerun-e2e").join(format!(
        "{graphics}-{eye_width}x{eye_height}-{target_count}t-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(scratch.join("logs")).expect("scratch directory");
    eprintln!("scratch: {}", scratch.display());

    let library_path = bin.join(qaros::qaros_sys::LIBRARY_FILE_NAME);
    let library_config = LibraryConfig {
        console_logging: false,
        log_directory: Some(scratch.join("logs")),
        log_severity: LogSeverity::Info,
    };
    // SAFETY: the package's DLL, whose header qaros-sys was generated from.
    let library = unsafe { Library::load(&library_path, &library_config) }.expect("load the base");
    // SAFETY: the base from the same folder is loaded and initialized.
    let extension = unsafe { Extension::load(&bin) }.expect("load the extension");
    // A PAKE invite expires within seconds, before the viewer reads its stdin
    // (about 15 s after start); the password invite stays valid.
    let hub = extension.create_hub(true).expect("extension hub");
    let target_peers: Vec<Peer> = (1..=target_count)
        .map(|number| {
            hub.create_peer(Some(&format!("e2e-headset-{number}")))
                .expect("extension viewer peer")
        })
        .collect();
    let layout = extension
        .layout_side_by_side(eye_width, eye_height)
        .expect("receiver layout");
    let invite = hub
        .mint_invite(InviteMethod::Password)
        .expect("mint the launch invite");
    eprintln!(
        "launch invite: {} bytes, starts with '{{': {}",
        invite.len(),
        invite.starts_with('{')
    );

    let started = Instant::now();
    let (viewer_child, viewer_stdin) = start_viewer(
        &library_path,
        &scratch,
        &graphics,
        // Requests running ahead can make the viewer show frames past the last one awaited
        // (a repeated request); the headroom keeps it streaming until stdin closes.
        (frames_per_target
            + if request_ahead {
                frames_in_flight + 2
            } else {
                0
            })
            * target_count,
        target_count,
        &invite,
    );
    let mut viewer = ChildGuard(viewer_child);
    let scene = start_test_scene(&scratch).map(ChildGuard);
    // A receiver's stream request would make the viewer add its peer too; created only once the
    // viewer has found the targets in the peer list, the receivers are not how it found them.
    // A target not yet listed in the application's session after `PEER_LIST_BUDGET` gets its
    // receiver started anyway, and its stream request is how the viewer finds it.
    for number in 1..=target_count {
        let budget = if number == 1 {
            FIRST_FRAME_BUDGET
        } else {
            PEER_LIST_BUDGET
        };
        let needle = format!("(e2e-headset-{number}) found by the peer list");
        let found = wait_for_viewer_log(&mut viewer.0, &scratch, &needle, budget);
        assert!(found || number > 1, "the viewer never logged {needle:?}");
        if !found {
            eprintln!("e2e-headset-{number} is not in the peer list after {budget:?}");
        }
    }
    let vulkan_device = vulkan_receiver.then(|| {
        extension
            .create_vulkan_device()
            .expect("create the extension's Vulkan device")
    });
    let mut targets: Vec<Target> = target_peers
        .iter()
        .enumerate()
        .map(|(index, peer)| Target {
            name: format!("target {}", index + 1),
            receiver: connect_receiver(
                peer,
                &layout,
                vulkan_device.as_ref(),
                &mut viewer.0,
                &scratch,
            ),
            vulkan_frame: vulkan_device
                .as_ref()
                .map(|device| device.create_frame(&layout).expect("receiver frame images")),
            requests: Vec::new(),
            arrivals: Vec::new(),
            sequences: Vec::new(),
            content_frames: 0,
            checked_frames: 0,
        })
        .collect();

    if request_ahead {
        for target in &mut targets {
            lend_buffers(target, 0, &mut viewer.0, &scratch);
            for frame_number in 0..frames_in_flight.min(frames_per_target) {
                send_request(target, frame_number);
                target.requests.push(now());
            }
        }
    }
    for frame_number in 0..frames_per_target {
        for target in &mut targets {
            if !request_ahead {
                lend_buffers(target, frame_number, &mut viewer.0, &scratch);
                send_request(target, frame_number);
                target.requests.push(now());
            }
            let info = receive_frame(target, frame_number, &mut viewer.0, &scratch);
            let frame_index = usize::try_from(frame_number).expect("frame index");
            if let Some(timing_log) = &mut timing_log {
                let requested = target.requests[frame_index];
                let arrived = target.arrivals[frame_index];
                writeln!(
                    timing_log,
                    "{},request,{},{frame_number}\n{},arrival,{},{frame_number}",
                    requested.1, target.name, arrived.1, target.name
                )
                .expect("write the timing log");
            }
            if target.arrivals.len() == 1 {
                eprintln!(
                    "{}: first frame {:?} after the viewer started",
                    target.name,
                    started.elapsed()
                );
            }
            assert!(info.valid_frame, "{} frame {frame_number}", target.name);
            target.sequences.push(info.sequence_number);
            let is_checked = vulkan_device.is_none()
                || frame_number % 30 == 0
                || frame_number + 1 == frames_per_target;
            let statistics = is_checked
                .then(|| frame_statistics(&target.receiver, vulkan_device.as_ref(), &layout));
            if request_ahead && frame_number + 1 < frames_per_target {
                lend_buffers(target, frame_number + 1, &mut viewer.0, &scratch);
                let next_request = frame_number + frames_in_flight;
                if next_request < frames_per_target + frames_in_flight - 1 {
                    send_request(target, next_request);
                    target.requests.push(now());
                }
            }
            if let Some(statistics) = &statistics {
                target.checked_frames += 1;
                if statistics.has_scene() {
                    target.content_frames += 1;
                }
            }
            if frame_number == 0 || frame_number + 1 == frames_per_target || frame_number % 30 == 0
            {
                eprintln!(
                    "{} frame {frame_number}: sequence {} {statistics:?}",
                    target.name, info.sequence_number
                );
            }
        }
    }
    for target in &targets {
        report_rate(&graphics, eye_width, eye_height, target);
        eprintln!(
            "{}: frames with scene content: {} of {} checked ({frames_per_target} received)",
            target.name, target.content_frames, target.checked_frames
        );
    }

    if let Some(timing_log) = &mut timing_log {
        timing_log.flush().expect("flush the timing log");
    }
    drop(viewer_stdin);
    let status = wait_for_exit(&mut viewer.0, Duration::from_secs(30));
    eprintln!(
        "viewer exit: {status:?}; {:?} after it started",
        started.elapsed()
    );
    print_viewer_summary(&scratch);
    drop(scene);
    for target in &targets {
        assert!(
            target.content_frames > 0,
            "the test scene shows up in the frames of {}",
            target.name
        );
        assert!(
            target
                .sequences
                .is_sorted_by(|earlier, later| earlier < later),
            "{} received its frames in order: {:?}",
            target.name,
            target.sequences
        );
    }
    let log = viewer_log(&scratch);
    let found_in_peer_list = log
        .lines()
        .filter(|line| line.contains("found by the peer list"))
        .count();
    eprintln!("targets found in the peer list: {found_in_peer_list} of {target_count}");
    assert_eq!(
        status,
        Some(0),
        "the viewer exits cleanly when stdin closes"
    );

    drop(targets);
    drop(vulkan_device);
    drop(target_peers);
    drop(hub);
    drop(extension);
    drop(library);
}

/// One target peer as the test drives it: its receiver and what arrived there.
struct Target {
    name: String,
    receiver: RenderReceiver,
    /// The images lent to a Vulkan receiver; after the receiver, which must go first.
    vulkan_frame: Option<VulkanTestFrame>,
    /// Monotonic time and wall clock in microseconds since the Unix epoch.
    requests: Vec<(Instant, u128)>,
    arrivals: Vec<(Instant, u128)>,
    sequences: Vec<u64>,
    content_frames: u64,
    checked_frames: u64,
}

fn now() -> (Instant, u128) {
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("wall clock after 1970")
        .as_micros();
    (Instant::now(), wall)
}

fn frame_budget(frame_number: u64) -> Duration {
    if frame_number == 0 {
        FIRST_FRAME_BUDGET
    } else {
        FRAME_BUDGET
    }
}

/// Lends the receiver's buffers for frame `frame_number`.
fn lend_buffers(target: &mut Target, frame_number: u64, viewer: &mut Child, scratch: &Path) {
    let deadline = Instant::now() + frame_budget(frame_number);
    loop {
        let accepted = match &target.vulkan_frame {
            Some(frame) => {
                let lent = frame.lend().expect("describe the Vulkan frame");
                // SAFETY: the frame's images live in the target, beside the receiver and
                // dropped after it.
                unsafe { target.receiver.provide_vulkan_frame(&lent) }.expect("lend the frame")
            }
            None => target.receiver.provide_cpu_frame().expect("lend buffers"),
        };
        if accepted {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{} frame {frame_number}: buffers never accepted",
            target.name
        );
        assert_viewer_alive(viewer, scratch);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn send_request(target: &Target, frame_number: u64) {
    let mut pose = defaults::qar_pose_default();
    pose.position.y = 1.6;
    if target
        .receiver
        .wait_connected(Duration::ZERO)
        .expect("connection state")
    {
        target
            .receiver
            .request_render(pose, Some(frame_number))
            .expect("request a frame");
    }
}

/// Waits for frame `frame_number`, already requested; repeats the request every 500 ms.
fn receive_frame(
    target: &mut Target,
    frame_number: u64,
    viewer: &mut Child,
    scratch: &Path,
) -> qaros_ext::ExtVideoFrameInfo {
    let budget = frame_budget(frame_number);
    let deadline = Instant::now() + budget;
    loop {
        if let Some(info) = target
            .receiver
            .receive_frame(Duration::from_millis(500))
            .expect("receive")
        {
            target.arrivals.push(now());
            return info;
        }
        // Before the viewer's sender exists the request goes nowhere; repeat it.
        send_request(target, frame_number);
        assert!(
            Instant::now() < deadline,
            "{} frame {frame_number} did not arrive within {budget:?}",
            target.name
        );
        assert_viewer_alive(viewer, scratch);
    }
}

/// Kills the child when dropped, so a failing test leaves no process behind.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A receiver whose stream request goes unanswered gives up after about 10 s
/// ("no peer acknowledged stream request"), and the viewer joins only after
/// its own start-up, so receivers are created until one connects.
fn connect_receiver(
    peer: &Peer,
    layout: &QarVideoFrameLayout,
    vulkan_device: Option<&VulkanDevice>,
    viewer: &mut Child,
    scratch: &Path,
) -> RenderReceiver {
    let deadline = Instant::now() + FIRST_FRAME_BUDGET;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let receiver = peer
            .create_render_receiver(layout, vulkan_device)
            .expect("render receiver");
        match receiver.wait_connected(Duration::from_secs(12)) {
            Ok(true) => {
                eprintln!("receiver connected on attempt {attempt}");
                return receiver;
            }
            Ok(false) => eprintln!("receiver attempt {attempt}: not connected yet"),
            Err(error) => eprintln!("receiver attempt {attempt}: {error}"),
        }
        drop(receiver);
        assert!(
            Instant::now() < deadline,
            "no receiver connected within {FIRST_FRAME_BUDGET:?}"
        );
        assert_viewer_alive(viewer, scratch);
    }
}

fn eye_size() -> (u32, u32) {
    let text = std::env::var("QAROS_E2E_EYE").unwrap_or_else(|_| "960x1000".to_owned());
    let (width, height) = text
        .split_once('x')
        .expect("QAROS_E2E_EYE is <width>x<height>");
    (
        width.parse().expect("eye width"),
        height.parse().expect("eye height"),
    )
}

fn start_viewer(
    library_path: &Path,
    scratch: &Path,
    graphics: &str,
    frame_total: u64,
    target_count: u64,
    invite: &str,
) -> (Child, ChildStdin) {
    let mut command = Command::new(viewer_binary());
    command
        .current_dir(scratch)
        .env("QAROS_STORAGE_DIR", scratch.join("storage"))
        .args(["--sink", "qaros", "--launched-by-qaros"])
        .args(["--quit-when-done", "--frames", &frame_total.to_string()])
        .args(["--max-users", &target_count.to_string()])
        .args(["--port", &VIEWER_PORT.to_string()])
        .arg("--qaros-library")
        .arg(library_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(
            std::fs::File::create(scratch.join("viewer.stdout.log")).expect("stdout log"),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(scratch.join("viewer.stderr.log")).expect("stderr log"),
        ));
    if std::env::var("QAROS_E2E_WINDOWED").is_ok_and(|value| value == "1") {
        eprintln!("viewer in a window");
        command.arg("--window");
    } else {
        command.args(["--headless", "1280x720"]);
    }
    command.args(["--qaros-graphics", graphics]);
    if graphics == "vulkan"
        && let Ok(selector) = std::env::var("QAROS_E2E_VULKAN_DEVICE")
    {
        command.args(["--vulkan-device", &selector]);
    }
    let mut viewer = command.spawn().expect("start the viewer");
    let mut stdin = viewer.stdin.take().expect("viewer stdin");
    stdin
        .write_all(format!("{invite}\n").as_bytes())
        .expect("write the launch invite");
    stdin.flush().expect("flush stdin");
    (viewer, stdin)
}

/// The bundled test scene, logged into the viewer's port. `None` when the binary is missing.
fn start_test_scene(scratch: &Path) -> Option<Child> {
    let url = format!("rerun+http://127.0.0.1:{VIEWER_PORT}/proxy");
    Command::new(viewer_binary().with_file_name(executable_name("log-test-scene")))
        .args(["100000", &url])
        .current_dir(scratch)
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            std::fs::File::create(scratch.join("scene.stderr.log")).expect("scene log"),
        ))
        .spawn()
        .map_err(|error| eprintln!("test scene not started: {error}"))
        .ok()
}

/// `QAROS_E2E_VIEWER`, else the workspace's release build.
fn viewer_binary() -> PathBuf {
    std::env::var_os("QAROS_E2E_VIEWER").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/release")
                .join(executable_name("qaros-rerun"))
        },
        PathBuf::from,
    )
}

fn executable_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

fn assert_viewer_alive(viewer: &mut Child, scratch: &Path) {
    if let Some(status) = viewer.try_wait().expect("viewer state") {
        print_viewer_log_tail(scratch);
        panic!("the viewer exited early: {status}");
    }
}

fn wait_for_exit(viewer: &mut Child, budget: Duration) -> Option<i32> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = viewer.try_wait().expect("viewer state") {
            return status.code();
        }
        if Instant::now() >= deadline {
            eprintln!("the viewer did not exit within {budget:?}; killing it");
            let _ = viewer.kill();
            let _ = viewer.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn print_viewer_log_tail(scratch: &Path) {
    for name in ["viewer.stderr.log", "viewer.stdout.log"] {
        let text = std::fs::read_to_string(scratch.join(name)).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        eprintln!("--- {name}, last lines ---");
        for line in &lines[lines.len().saturating_sub(25)..] {
            eprintln!("{line}");
        }
    }
}

/// Waits until the viewer's log holds `needle`, within [`FIRST_FRAME_BUDGET`].
/// Waits until the viewer's log holds `needle`; false when `budget` runs out first.
fn wait_for_viewer_log(viewer: &mut Child, scratch: &Path, needle: &str, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while !viewer_log(scratch).contains(needle) {
        if Instant::now() >= deadline {
            return false;
        }
        assert_viewer_alive(viewer, scratch);
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!("viewer: {needle}");
    true
}

/// The viewer's stderr and stdout logs, in that order.
fn viewer_log(scratch: &Path) -> String {
    ["viewer.stderr.log", "viewer.stdout.log"]
        .map(|name| std::fs::read_to_string(scratch.join(name)).unwrap_or_default())
        .join("\n")
}

fn print_viewer_summary(scratch: &Path) {
    for line in viewer_log(scratch).lines().filter(|line| {
        line.contains("qaros:")
            || line.contains("shown per second")
            || line.contains("user-")
            || line.contains("vulkan-device")
            || line.contains("ERROR")
            || line.contains("WARN")
    }) {
        eprintln!("viewer: {line}");
    }
}

fn report_rate(graphics: &str, eye_width: u32, eye_height: u32, target: &Target) {
    // The first frames include the viewer's warm-up; the rate is taken over the second half.
    let steady = &target.arrivals[target.arrivals.len() / 2..];
    if steady.len() < 2 {
        return;
    }
    let span = steady[steady.len() - 1].0 - steady[0].0;
    let half = target.arrivals.len() / 2;
    let mut round_trips: Vec<Duration> = target.requests[half..]
        .iter()
        .zip(steady)
        .map(|(requested, arrived)| arrived.0 - requested.0)
        .collect();
    round_trips.sort_unstable();
    #[allow(clippy::cast_precision_loss)]
    let rate = (steady.len() - 1) as f64 / span.as_secs_f64();
    eprintln!(
        "RATE viewer, {graphics} path, {eye_width}x{eye_height} per eye, {}: {rate:.1} frames/s over the last {} frames; request to arrival median {:.1} ms",
        target.name,
        steady.len(),
        round_trips[round_trips.len() / 2].as_secs_f64() * 1000.0
    );
}

/// How much of each eye shows something other than the background.
#[derive(Debug)]
struct FrameStatistics {
    color_texels_unlike_background: u64,
    color_texels: u64,
    depth_texels_in_front: u64,
    depth_texels: u64,
}

impl FrameStatistics {
    fn has_scene(&self) -> bool {
        self.color_texels_unlike_background > 0 && self.depth_texels_in_front > 0
    }
}

/// Reads the received frame: the CPU buffers, or the Vulkan images read back.
fn frame_statistics(
    receiver: &RenderReceiver,
    vulkan_device: Option<&VulkanDevice>,
    layout: &QarVideoFrameLayout,
) -> FrameStatistics {
    let textures: Vec<(Cow<'_, [u8]>, usize)> = if let Some(device) = vulkan_device {
        let mut delivered = receiver.last_frame_vulkan().expect("last Vulkan frame");
        (0..delivered.textures_count)
            .map(|index| {
                let height = delivered.textures[index].size.height.max(1) as usize;
                // SAFETY: the record describes the lent images on this device, not lent again
                // before this read; the first read waits for its semaphore.
                let bytes = unsafe { device.read_texture(&raw mut delivered, index, index == 0) }
                    .expect("read the received texture");
                let pitch = bytes.len() / height;
                (Cow::Owned(bytes), pitch)
            })
            .collect()
    } else {
        let buffers = receiver.cpu_buffers().expect("CPU buffers");
        (0..layout.textures_count)
            .map(|index| {
                (
                    Cow::Borrowed(buffers.texture(index).unwrap_or_default()),
                    buffers.pitch(index).unwrap_or_default() as usize,
                )
            })
            .collect()
    };
    texel_statistics(&textures, layout)
}

fn texel_statistics(
    textures: &[(Cow<'_, [u8]>, usize)],
    layout: &QarVideoFrameLayout,
) -> FrameStatistics {
    let mut statistics = FrameStatistics {
        color_texels_unlike_background: 0,
        color_texels: 0,
        depth_texels_in_front: 0,
        depth_texels: 0,
    };
    for view in &layout.views[..layout.views_count] {
        let texture_index = view.texture_index as usize;
        let (bytes, pitch) = &textures[texture_index];
        let pitch = *pitch;
        let format = layout.textures[texture_index].format;
        let corner = view.start_y as usize * pitch + view.start_x as usize * 4;
        let background: [u8; 4] = bytes[corner..corner + 4].try_into().expect("a texel");
        for row in (view.start_y.min(view.end_y)..view.start_y.max(view.end_y)).step_by(8) {
            for column in (view.start_x.min(view.end_x)..view.start_x.max(view.end_x)).step_by(8) {
                let offset = row as usize * pitch + column as usize * 4;
                let texel: [u8; 4] = bytes[offset..offset + 4].try_into().expect("a texel");
                if view.data_type == QarVideoFrameViewType::QAR_VIDEO_FRAME_VIEW_TYPE_DEPTH
                    || format == QarPixelFormat::QAR_PIXEL_FORMAT_R32_FLOAT
                {
                    statistics.depth_texels += 1;
                    let depth = f32::from_le_bytes(texel);
                    if texel != background && depth.is_finite() {
                        statistics.depth_texels_in_front += 1;
                    }
                } else {
                    statistics.color_texels += 1;
                    if texel != background {
                        statistics.color_texels_unlike_background += 1;
                    }
                }
            }
        }
    }
    statistics
}
