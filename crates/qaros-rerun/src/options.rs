use std::path::PathBuf;
use std::time::Duration;

use glam::Vec3;
use vulkan_device::PhysicalDeviceSelector;
use xr_math::{AppPlacement, DepthEncoding, DepthRange, Pose, ViewCoordinates};

pub const USAGE: &str = "\
qaros-rerun: the Rerun viewer with a headset renderer

USAGE: qaros-rerun [OPTIONS]

Headset stream:
    --sink image|qaros|off     frame sink: PNGs with scripted head motion, a QAROS render
                               stream, or none (default image; `images` is accepted too)
    --frames N                 image: each simulated user leaves after N shown frames (default
                               120); qaros: stop after N shown frames across all users
                               (default unlimited)
    --quit-when-done           close the viewer once the sink stops streaming
    --opaque-background        keep Rerun's background instead of clearing empty pixels to transparent
    --max-users N              render for at most N users at a time; later users wait for a
                               free slot (default 2)
    --frame-stamp              draw user, frame id and eye (L/R) into every eye image, to check
                               frame and pose pairing in the visualizer (default off)
    --decoupled-render         experiment: draw the eyes on a render thread of their own from the
                               last scene the UI finished, at the sink's rate; the sink and the
                               GUI panels live on that thread (default off); with the QAROS
                               Vulkan sink only headless

Image sink:
    --out DIR                  output directory, one user-<n>/ per user (default out/headset)
    --eye-resolution WxH       per-eye resolution (default 960x1000)
    --write-every N            write every N-th shown frame of each user (default 20)
    --raw-depth                also write each written frame's depth as raw little-endian f32
                               (frame_N_depth.f32: left eye, then right eye, row by row)
    --motion circle|orbit|still   scripted head motion (default circle): circle walks around
                               the world center looking level, orbit walks the same circle
                               looking at the center
    --radius METERS            radius of the circle walk (default 1.6)
    --eye-height METERS        eye height of the simulated heads (default 1.6)
    --period SECONDS           time for one circle at 60 frames per second (default 4)
    --fov DEGREES              a centered, camera-like field of view this many degrees wide,
                               with square pixels (default: a headset-like, asymmetric field)
    --users N                  simulated users, spread around the circle (default 1)
    --join-interval N          user k joins N viewer frames after user k-1 (default 30)
    --image-targets cpu|gpu    cpu (default) reads color and depth back from Rerun; gpu renders
                               into textures of the sink, as the QAROS Vulkan sink does, and
                               reads those back; needs --vulkan-device
    --depth-scale-down N       with --image-targets gpu: depth targets N times smaller than color
                               in each dimension (default 1), as a sender with lower-resolution
                               depth would hand out; the written depth repeats each depth pixel

QAROS sink (the library is QAROS_DLL, else the installed QAROS by the default lookup):
    --pairing-code CODE        the Hub's current pairing code, for the first start; later starts
                               rejoin with the stored onboarding id; given while a stored id
                               cannot be rejoined, the viewer pairs with it instead
    --onboarding-id-file PATH  where the onboarding id is kept (default qaros-rerun.onboarding-id.txt)
    --qaros-library PATH       qar-streaming-c library, overriding QAROS_DLL
    --pose-space app|room      space of the stream's eye poses (default app, the volume's app
                               content space in app units, as QAROS documents it): app maps
                               the poses into the room with --origin, --yaw and --scale and
                               hands near and far back in app units (room meters / --scale);
                               room uses the poses as they come, near and far in room meters
    --volume-size W,L,H        size of the app volume in meters (default 2,2,2)
    --volume-name NAME         the app volume's name (default rerun); viewers running at once
                               need different names, or QAROS gives them one volume and stream
    --display-name NAME        the app volume's label for people (default Rerun); the viewer
                               shows in the peer list as 'NAME viewer'
    --color-format bgra|rgba   color format requested from the sender (default bgra)
    --begin-frame poll         each user's frame is begun by polling
                               qar_render_sender_try_begin_frame once per viewer frame, the
                               only mode on both paths
    --panels-at-stop hide|keep what the GUI panels become at Stop (default hide): hide sets them
                               hidden, keep leaves them showing; either way they stay for the
                               next run to reattach, never closed
    --qaros-graphics cpu|vulkan   frames through the CPU render senders, or rendered straight
                               into the Vulkan render senders' images on a Vulkan device the
                               viewer creates after onboarding, on the adapter the session
                               reports, with QAROS's device extensions; --vulkan-device names
                               the device instead, created first and chained into onboarding
                               (default vulkan on Windows, cpu elsewhere)
    --launched-by-qaros        the QAROS API app contract: stop when stdin closes; on Windows
                               join as a local app (as with --local-app), elsewhere onboard
                               with the first line of stdin (the JSON launch invite, or a code
                               typed by hand); QAROS_STORAGE_DIR moves the runtime's state; runs
                               headless at 1280x720 unless --window or --headless is given
    --local-app                join the Hub installed on this PC as a local app, with no code
                               or invite; the Hub asks to allow the app on its first start.
                               With --launched-by-qaros stdin is not read for an invite, only
                               watched for Stop, which takes effect once the approval wait
                               ends. Takes no --pairing-code or --onboarding-id-file
    --gpu-priority high|normal   GPU scheduling priority of the process on Windows (default
                               high; normal leaves it as Windows set it)
    QAROS_RERUN_BEGIN_KICK=0   on the Vulkan path, begin a user's next frame in the next viewer
                               frame instead of right after its show; the early begin gives
                               every user a frame per viewer frame, rendered with a pose one
                               viewer frame old

Placement of Rerun's world in the room (OpenXR: Y up, -Z forward, meters; with --sink qaros
also the app volume's center, app pose and app scale):
    --origin X,Y,Z             room position of the world center (default 0,0.5,-1.6)
    --center X,Y,Z             Rerun-world point placed at --origin, the walk circles it (default 0,0,0)
    --scale S                  room meters per Rerun unit (default 1)
    --yaw DEGREES              turn the world about the room's up axis (default 0)
    --up CODE                  override the view coordinates, e.g. RFU (default: from the view)

Depth reported to the sink (the QAROS sink converts near and far to the poses' units, see
--pose-space):
    --near METERS              near plane in room meters (default 0.05)
    --far METERS               finite far plane in room meters (default 20)
    --depth forward|reverse|linear   depth encoding (default forward)

2D views as QAROS GUI panels (with --sink qaros; QAROS 0.9.0-dev.3177 or later):
    --panels                   open each non-3D view and a timeline as GUI panels within reach
                               of the headset user, each a video stream of this viewer; the
                               timeline takes taps and drags on its track and Play button
    --panel-interval-ms MS     capture interval (default 250)

Viewer:
    --port PORT                gRPC port the logging SDKs connect to (default 9876)
    --headless WxH             run without a window, the UI rendered off screen at WxH points
    --window                   open the desktop window also with --launched-by-qaros
    -h, --help                 this text

Graphics device (Vulkan on macOS needs Homebrew's vulkan-loader and molten-vk, and
DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib):
    --graphics-backend vulkan|metal|dx12|gl   wgpu backend, set on the instance (default: Rerun's)
    --vulkan-device SELECTOR   create the Vulkan device here instead of in egui: the physical
                               device by default, name:TEXT, uuid:HEX, luid:HEX or index:N
    --vulkan-extension NAME    with --vulkan-device, also enable this device extension; repeatable;
                               windows-interop names the QAROS Windows set
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkKind {
    Images,
    Qaros,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionKind {
    Circle,
    Orbit,
    Still,
}

/// Where the image sink's frames are rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageTargets {
    /// Rerun's screenshot and depth readbacks, as for the QAROS CPU render sender.
    Cpu,
    /// Textures of the sink, as for the QAROS Vulkan render sender, then read back.
    Gpu,
}

/// Space of the QAROS stream's eye poses; see `qaros_sink::PoseSpace`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoseSpaceKind {
    Room,
    App,
}

/// What the GUI panels become at Stop; see `qaros_sink::PanelsAtStop`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelsAtStopKind {
    Hide,
    Keep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsBackend {
    Vulkan,
    Metal,
    Dx12,
    Gl,
}

impl GraphicsBackend {
    pub fn backends(self) -> wgpu::Backends {
        match self {
            Self::Vulkan => wgpu::Backends::VULKAN,
            Self::Metal => wgpu::Backends::METAL,
            Self::Dx12 => wgpu::Backends::DX12,
            Self::Gl => wgpu::Backends::GL,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorFormatKind {
    Bgra,
    Rgba,
}

/// How frames reach the QAROS render senders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QarosGraphics {
    /// Read back to the CPU and copied into the CPU render senders' textures.
    Cpu,
    /// Rendered into the Vulkan render senders' images on the viewer's own Vulkan device.
    Vulkan,
}

impl QarosGraphics {
    /// Vulkan on Windows, where QAROS runs and frames then never pass through the CPU; the
    /// CPU path elsewhere, where the Vulkan path needs MoltenVK.
    pub const PLATFORM_DEFAULT: Self = if cfg!(windows) {
        Self::Vulkan
    } else {
        Self::Cpu
    };
}

#[derive(Clone, Debug)]
pub struct QarosOptions {
    pub pairing_code: Option<String>,
    /// `--onboarding-id-file`; absent means [`DEFAULT_ONBOARDING_ID_FILE`].
    pub onboarding_id_file_flag: Option<PathBuf>,
    pub library_path: Option<PathBuf>,
    pub pose_space: PoseSpaceKind,
    pub volume_size: Vec3,
    /// The app volume's name: QAROS hands an app that asks for an existing name that volume
    /// and its render stream, so two viewers at once need two names.
    pub volume_name: String,
    /// The app volume's label for people; the viewer's peer is "<display name> viewer".
    pub display_name: String,
    pub color_format: ColorFormatKind,
    pub panels_at_stop: PanelsAtStopKind,
    /// `--qaros-graphics`; absent means [`QarosGraphics::PLATFORM_DEFAULT`].
    pub graphics_flag: Option<QarosGraphics>,
    pub launched_by_qaros: bool,
    /// `--local-app`: onboard as a local app of the Hub on this PC instead of with an invite
    /// or code.
    pub local_app: bool,
    /// `--gpu-priority high` (the default) sets the process's GPU scheduling priority to high
    /// on Windows; `normal` leaves it as Windows set it.
    pub raise_gpu_priority: bool,
}

#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub const DEFAULT_ONBOARDING_ID_FILE: &str = "qaros-rerun.onboarding-id.txt";

#[cfg_attr(not(feature = "qaros"), allow(dead_code))]
pub const UNANSWERED_PEERS_FILE_NAME: &str = "qaros-rerun.unanswered-peers.txt";

/// On Windows QAROS writes no launch invite to a launched app's stdin: the app joins as a
/// local app and the Hub recognises the process it started.
const LAUNCHED_APP_JOINS_AS_LOCAL_APP: bool = cfg!(windows);

impl QarosOptions {
    /// `--local-app`, or `--launched-by-qaros` where QAROS launches apps as local apps.
    #[cfg_attr(not(feature = "qaros"), allow(dead_code))]
    pub fn joins_as_local_app(&self) -> bool {
        self.local_app || (self.launched_by_qaros && LAUNCHED_APP_JOINS_AS_LOCAL_APP)
    }

    #[cfg_attr(not(feature = "qaros"), allow(dead_code))]
    pub fn onboarding_id_file(&self) -> PathBuf {
        self.onboarding_id_file_flag
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_ONBOARDING_ID_FILE))
    }

    /// Next to the onboarding id file, the other state the viewer keeps across runs.
    #[cfg_attr(not(feature = "qaros"), allow(dead_code))]
    pub fn unanswered_peers_file(&self) -> PathBuf {
        self.onboarding_id_file()
            .with_file_name(UNANSWERED_PEERS_FILE_NAME)
    }

    pub fn graphics(&self) -> QarosGraphics {
        self.graphics_flag
            .unwrap_or(QarosGraphics::PLATFORM_DEFAULT)
    }

    fn apply_flag(&mut self, flag: &str, value: String) -> Result<(), ParseOutcome> {
        match flag {
            "--pairing-code" => self.pairing_code = Some(value),
            "--onboarding-id-file" => self.onboarding_id_file_flag = Some(PathBuf::from(value)),
            "--qaros-library" => self.library_path = Some(PathBuf::from(value)),
            "--pose-space" => {
                self.pose_space = match value.as_str() {
                    "room" => PoseSpaceKind::Room,
                    "app" => PoseSpaceKind::App,
                    other => return Err(invalid(flag, other)),
                }
            }
            "--volume-size" => {
                let size = parse_vector(flag, &value)?;
                if size.min_element() <= 0.0 {
                    return Err(ParseOutcome::Error(format!("{flag} must be positive")));
                }
                self.volume_size = size;
            }
            "--volume-name" => {
                if value.trim().is_empty() {
                    return Err(ParseOutcome::Error(format!("{flag} must not be empty")));
                }
                self.volume_name = value;
            }
            "--display-name" => {
                if value.trim().is_empty() {
                    return Err(ParseOutcome::Error(format!("{flag} must not be empty")));
                }
                self.display_name = value;
            }
            "--color-format" => {
                self.color_format = match value.as_str() {
                    "bgra" => ColorFormatKind::Bgra,
                    "rgba" => ColorFormatKind::Rgba,
                    other => return Err(invalid(flag, other)),
                }
            }
            "--begin-frame" => match value.as_str() {
                "poll" => {}
                unsupported @ ("async" | "blocking") => {
                    return Err(ParseOutcome::Error(format!(
                        "{flag} {unsupported} is not supported: frames are begun by polling \
                         (poll), which never waits inside the runtime and leaves no begin pending \
                         when the session is destroyed"
                    )));
                }
                other => return Err(invalid(flag, other)),
            },
            "--panels-at-stop" => {
                self.panels_at_stop = match value.as_str() {
                    "hide" => PanelsAtStopKind::Hide,
                    "keep" => PanelsAtStopKind::Keep,
                    other => return Err(invalid(flag, other)),
                }
            }
            "--gpu-priority" => {
                self.raise_gpu_priority = match value.as_str() {
                    "high" => true,
                    "normal" => false,
                    other => return Err(invalid(flag, other)),
                }
            }
            "--qaros-graphics" => {
                self.graphics_flag = Some(match value.as_str() {
                    "cpu" => QarosGraphics::Cpu,
                    "vulkan" => QarosGraphics::Vulkan,
                    other => return Err(invalid(flag, other)),
                });
            }
            other => return Err(ParseOutcome::Error(format!("unknown option {other}"))),
        }
        Ok(())
    }
}

impl Default for QarosOptions {
    fn default() -> Self {
        Self {
            pairing_code: None,
            onboarding_id_file_flag: None,
            library_path: None,
            pose_space: PoseSpaceKind::App,
            volume_size: Vec3::splat(2.0),
            volume_name: "rerun".to_owned(),
            display_name: "Rerun".to_owned(),
            color_format: ColorFormatKind::Bgra,
            panels_at_stop: PanelsAtStopKind::Hide,
            graphics_flag: None,
            launched_by_qaros: false,
            local_app: false,
            raise_gpu_priority: true,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent command-line switch"
)]
pub struct Options {
    pub sink: SinkKind,
    pub output_directory: PathBuf,
    pub eye_resolution: [u32; 2],
    /// `None`: the sink's default (120 frames per simulated user for the image sink,
    /// unlimited for QAROS).
    pub stop_after_frames: Option<u64>,
    pub write_every_nth_frame: u64,
    pub write_raw_depth: bool,
    pub depth_scale_down: u32,
    pub motion: MotionKind,
    pub circle_radius: f32,
    pub eye_height: f32,
    pub circle_period_seconds: f32,
    /// `Some`: a centered field of view this many degrees wide instead of the headset-like one.
    pub camera_fov_degrees: Option<f32>,
    pub image_targets: ImageTargets,
    pub simulated_users: u32,
    pub join_interval_in_viewer_frames: u64,
    pub max_users: usize,
    pub frame_stamp: bool,
    pub decoupled_render: bool,
    pub quit_when_done: bool,
    pub opaque_background: bool,
    pub world_origin: Vec3,
    pub world_center: Vec3,
    pub world_scale: f32,
    pub world_yaw_degrees: f32,
    pub world_coordinates_override: Option<ViewCoordinates>,
    pub depth_range: DepthRange,
    /// `--panels`: the 2D views and a timeline as QAROS GUI panels.
    pub panels: bool,
    pub panel_capture_interval: Duration,
    pub grpc_port: u16,
    pub headless_size: Option<[u32; 2]>,
    pub graphics_backend: Option<GraphicsBackend>,
    /// `Some`: the viewer creates its Vulkan device itself, on this physical device.
    pub vulkan_device: Option<PhysicalDeviceSelector>,
    pub vulkan_extensions: Vec<String>,
    pub qaros: QarosOptions,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            sink: SinkKind::Images,
            output_directory: PathBuf::from("out/headset"),
            eye_resolution: [960, 1000],
            stop_after_frames: None,
            write_every_nth_frame: 20,
            write_raw_depth: false,
            depth_scale_down: 1,
            motion: MotionKind::Circle,
            circle_radius: 1.6,
            eye_height: 1.6,
            circle_period_seconds: 4.0,
            camera_fov_degrees: None,
            image_targets: ImageTargets::Cpu,
            simulated_users: 1,
            join_interval_in_viewer_frames: 30,
            max_users: 2,
            frame_stamp: false,
            decoupled_render: false,
            quit_when_done: false,
            opaque_background: false,
            world_origin: Vec3::new(0.0, 0.5, -1.6),
            world_center: Vec3::ZERO,
            world_scale: 1.0,
            world_yaw_degrees: 0.0,
            world_coordinates_override: None,
            depth_range: DepthRange {
                near_far: xr_math::NearFar {
                    near_plane: 0.05,
                    far_plane: 20.0,
                },
                encoding: DepthEncoding::ForwardNormalized,
            },
            panels: false,
            // Each capture reads the whole window back from the GPU on the UI thread; on air
            // traffic 100 ms cost 15 % of the headset frames, 250 ms 6 %.
            panel_capture_interval: Duration::from_millis(250),
            grpc_port: 9876,
            headless_size: None,
            graphics_backend: None,
            vulkan_device: None,
            vulkan_extensions: Vec::new(),
            qaros: QarosOptions::default(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseOutcome {
    Help,
    Error(String),
}

impl Options {
    pub fn placement(&self) -> AppPlacement {
        AppPlacement {
            volume_pose: Pose::from_translation(self.world_origin),
            app_scale: self.world_scale,
            world_center: self.world_center,
            ..AppPlacement::default()
        }
        .with_up_axis_rotation(self.world_yaw_degrees.to_radians())
    }

    fn apply_graphics_flag(&mut self, flag: &str, value: String) -> Result<(), ParseOutcome> {
        match flag {
            "--graphics-backend" => {
                self.graphics_backend = Some(match value.as_str() {
                    "vulkan" => GraphicsBackend::Vulkan,
                    "metal" => GraphicsBackend::Metal,
                    "dx12" => GraphicsBackend::Dx12,
                    "gl" => GraphicsBackend::Gl,
                    other => return Err(invalid(flag, other)),
                });
            }
            "--vulkan-device" => {
                self.vulkan_device = Some(
                    PhysicalDeviceSelector::parse(&value)
                        .map_err(|error| ParseOutcome::Error(format!("{flag}: {error}")))?,
                );
            }
            "--vulkan-extension" => self.vulkan_extensions.push(value),
            other => return Err(ParseOutcome::Error(format!("unknown option {other}"))),
        }
        Ok(())
    }

    /// True when the viewer creates its Vulkan device itself: on `--vulkan-device`, or for the
    /// QAROS Vulkan render sender.
    pub fn creates_vulkan_device(&self) -> bool {
        self.vulkan_device.is_some() || self.qaros_vulkan()
    }

    pub fn qaros_vulkan(&self) -> bool {
        self.sink == SinkKind::Qaros && self.qaros.graphics() == QarosGraphics::Vulkan
    }

    fn check_graphics_flags(&self) -> Result<(), ParseOutcome> {
        if !self.vulkan_extensions.is_empty() && !self.creates_vulkan_device() {
            return Err(ParseOutcome::Error(
                "--vulkan-extension needs --vulkan-device or --qaros-graphics vulkan".to_owned(),
            ));
        }
        let other_backend = self
            .graphics_backend
            .is_some_and(|backend| backend != GraphicsBackend::Vulkan);
        if self.creates_vulkan_device() && other_backend {
            return Err(ParseOutcome::Error(
                "--vulkan-device and --qaros-graphics vulkan (the default on Windows) need the \
                 vulkan graphics backend; --qaros-graphics cpu renders on any backend"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn check_qaros_flags(&self) -> Result<(), ParseOutcome> {
        let is_qaros = self.sink == SinkKind::Qaros;
        if self.qaros.graphics_flag == Some(QarosGraphics::Vulkan) && !is_qaros {
            return Err(ParseOutcome::Error(
                "--qaros-graphics vulkan needs --sink qaros".to_owned(),
            ));
        }
        if self.qaros.launched_by_qaros && !is_qaros {
            return Err(ParseOutcome::Error(
                "--launched-by-qaros needs --sink qaros".to_owned(),
            ));
        }
        if self.panels && !is_qaros {
            return Err(ParseOutcome::Error(
                "--panels needs --sink qaros: the panels are QAROS GUI panels".to_owned(),
            ));
        }
        if self.qaros.local_app && !is_qaros {
            return Err(ParseOutcome::Error(
                "--local-app needs --sink qaros".to_owned(),
            ));
        }
        if self.qaros.local_app && self.qaros.pairing_code.is_some() {
            return Err(ParseOutcome::Error(
                "--local-app joins the Hub on this PC without a code; drop --pairing-code"
                    .to_owned(),
            ));
        }
        if self.qaros.local_app && self.qaros.onboarding_id_file_flag.is_some() {
            return Err(ParseOutcome::Error(
                "--local-app keeps no onboarding id; drop --onboarding-id-file".to_owned(),
            ));
        }
        if self.image_targets == ImageTargets::Gpu
            && (self.sink != SinkKind::Images || self.vulkan_device.is_none())
        {
            return Err(ParseOutcome::Error(
                "--image-targets gpu needs --sink image and --vulkan-device".to_owned(),
            ));
        }
        if self.depth_scale_down == 0 {
            return Err(ParseOutcome::Error(
                "--depth-scale-down must be at least 1".to_owned(),
            ));
        }
        if self.depth_scale_down != 1 && self.image_targets != ImageTargets::Gpu {
            return Err(ParseOutcome::Error(
                "--depth-scale-down needs --image-targets gpu; a QAROS sender sets the depth \
                 image's size itself"
                    .to_owned(),
            ));
        }
        let renders_into_depth_images =
            self.qaros_vulkan() || self.image_targets == ImageTargets::Gpu;
        if renders_into_depth_images && self.depth_range.encoding == DepthEncoding::LinearMeters {
            return Err(ParseOutcome::Error(
                "--qaros-graphics vulkan (the default on Windows) and --image-targets gpu write \
                 depth into a depth image, which holds normalized values only; use --depth \
                 forward or reverse, or --qaros-graphics cpu"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one flat match arm per command-line flag"
    )]
    pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self, ParseOutcome> {
        let mut options = Self::default();
        let mut near = options.depth_range.near();
        let mut far = options.depth_range.far();
        let mut encoding = options.depth_range.encoding;
        let mut window_requested = false;
        let mut arguments = arguments.into_iter();

        while let Some(flag) = arguments.next() {
            let mut value = || {
                arguments
                    .next()
                    .ok_or_else(|| ParseOutcome::Error(format!("{flag} needs a value")))
            };
            match flag.as_str() {
                "-h" | "--help" => return Err(ParseOutcome::Help),
                "--sink" => {
                    options.sink = match value()?.as_str() {
                        "image" | "images" => SinkKind::Images,
                        "qaros" => SinkKind::Qaros,
                        "off" => SinkKind::Off,
                        other => return Err(invalid(&flag, other)),
                    }
                }
                "--pairing-code"
                | "--onboarding-id-file"
                | "--qaros-library"
                | "--pose-space"
                | "--volume-size"
                | "--volume-name"
                | "--display-name"
                | "--color-format"
                | "--begin-frame"
                | "--panels-at-stop"
                | "--gpu-priority"
                | "--qaros-graphics" => options.qaros.apply_flag(&flag, value()?)?,
                "--launched-by-qaros" => options.qaros.launched_by_qaros = true,
                "--local-app" => options.qaros.local_app = true,
                "--users" => options.simulated_users = parse_positive(&flag, &value()?)?,
                "--join-interval" => {
                    options.join_interval_in_viewer_frames = parse_number(&flag, &value()?)?;
                }
                "--max-users" => options.max_users = parse_positive(&flag, &value()?)?,
                "--frame-stamp" => options.frame_stamp = true,
                "--decoupled-render" => options.decoupled_render = true,
                "--out" => options.output_directory = PathBuf::from(value()?),
                "--eye-resolution" => options.eye_resolution = parse_resolution(&flag, &value()?)?,
                "--frames" => options.stop_after_frames = Some(parse_number(&flag, &value()?)?),
                "--write-every" => options.write_every_nth_frame = parse_number(&flag, &value()?)?,
                "--motion" => {
                    options.motion = match value()?.as_str() {
                        "circle" => MotionKind::Circle,
                        "orbit" => MotionKind::Orbit,
                        "still" => MotionKind::Still,
                        other => return Err(invalid(&flag, other)),
                    }
                }
                "--radius" => options.circle_radius = parse_number(&flag, &value()?)?,
                "--eye-height" => options.eye_height = parse_number(&flag, &value()?)?,
                "--period" => options.circle_period_seconds = parse_positive(&flag, &value()?)?,
                "--fov" => {
                    let degrees: f32 = parse_number(&flag, &value()?)?;
                    if !(1.0..=170.0).contains(&degrees) {
                        return Err(ParseOutcome::Error(format!(
                            "{flag} must be between 1 and 170 degrees"
                        )));
                    }
                    options.camera_fov_degrees = Some(degrees);
                }
                "--image-targets" => options.image_targets = parse_image_targets(&flag, &value()?)?,
                "--quit-when-done" => options.quit_when_done = true,
                "--opaque-background" => options.opaque_background = true,
                "--raw-depth" => options.write_raw_depth = true,
                "--depth-scale-down" => options.depth_scale_down = parse_number(&flag, &value()?)?,
                "--origin" => options.world_origin = parse_vector(&flag, &value()?)?,
                "--center" => options.world_center = parse_vector(&flag, &value()?)?,
                "--scale" => options.world_scale = parse_number(&flag, &value()?)?,
                "--yaw" => options.world_yaw_degrees = parse_number(&flag, &value()?)?,
                "--up" => {
                    let code = value()?;
                    let coordinates = ViewCoordinates::parse(&code)
                        .map_err(|error| ParseOutcome::Error(error.to_string()))?;
                    coordinates
                        .room_from_coordinates()
                        .map_err(|error| ParseOutcome::Error(format!("--up {code}: {error}")))?;
                    options.world_coordinates_override = Some(coordinates);
                }
                "--near" => near = parse_number(&flag, &value()?)?,
                "--far" => far = parse_number(&flag, &value()?)?,
                "--depth" => {
                    encoding = match value()?.as_str() {
                        "forward" => DepthEncoding::ForwardNormalized,
                        "reverse" => DepthEncoding::ReverseNormalized,
                        "linear" => DepthEncoding::LinearMeters,
                        other => return Err(invalid(&flag, other)),
                    }
                }
                "--panels" => options.panels = true,
                "--panel-interval-ms" => {
                    options.panel_capture_interval =
                        Duration::from_millis(parse_number(&flag, &value()?)?);
                }
                "--port" => options.grpc_port = parse_number(&flag, &value()?)?,
                "--headless" => {
                    options.headless_size = Some(parse_resolution(&flag, &value()?)?);
                }
                "--window" => window_requested = true,
                "--graphics-backend" | "--vulkan-device" | "--vulkan-extension" => {
                    options.apply_graphics_flag(&flag, value()?)?;
                }
                other => return Err(ParseOutcome::Error(format!("unknown option {other}"))),
            }
        }

        options.check_graphics_flags()?;

        options.depth_range = DepthRange::new(near, far, encoding).ok_or_else(|| {
            ParseOutcome::Error(format!(
                "need 0 < near < far, both finite; got {near} and {far}"
            ))
        })?;
        if !(options.world_scale.is_finite() && options.world_scale > 0.0) {
            return Err(ParseOutcome::Error("--scale must be positive".to_owned()));
        }
        options.check_qaros_flags()?;
        options.choose_window_or_headless(window_requested)?;
        options.check_decoupled_render()?;
        Ok(options)
    }

    /// In a window, egui submits its frames to the queue without the submission lock, which
    /// the QAROS Vulkan sink's raw submits on the render thread need.
    fn check_decoupled_render(&self) -> Result<(), ParseOutcome> {
        if !self.decoupled_render {
            return Ok(());
        }
        if self.sink == SinkKind::Off {
            return Err(ParseOutcome::Error(
                "--decoupled-render needs a sink".to_owned(),
            ));
        }
        if self.qaros_vulkan() && self.headless_size.is_none() {
            return Err(ParseOutcome::Error(
                "--decoupled-render with the QAROS Vulkan sink needs --headless (or \
                 --launched-by-qaros without --window), or --qaros-graphics cpu"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    /// A viewer QAROS launches runs headless unless `--window` asks for the desktop window: a
    /// window is capped by vsync and starves when covered, and nobody looks at it while QAROS streams.
    fn choose_window_or_headless(&mut self, window_requested: bool) -> Result<(), ParseOutcome> {
        if window_requested && self.headless_size.is_some() {
            return Err(ParseOutcome::Error(
                "--window and --headless exclude each other".to_owned(),
            ));
        }
        if !window_requested && self.headless_size.is_none() && self.qaros.launched_by_qaros {
            self.headless_size = Some(QAROS_LAUNCH_HEADLESS_SIZE);
        }
        Ok(())
    }
}

/// The off-screen UI size of a viewer QAROS launches without `--window` or `--headless`.
pub const QAROS_LAUNCH_HEADLESS_SIZE: [u32; 2] = [1280, 720];

fn invalid(flag: &str, value: &str) -> ParseOutcome {
    ParseOutcome::Error(format!("{flag}: unexpected value {value:?}"))
}

fn parse_number<Number: std::str::FromStr>(flag: &str, text: &str) -> Result<Number, ParseOutcome> {
    text.trim().parse().map_err(|_| invalid(flag, text))
}

fn parse_image_targets(flag: &str, text: &str) -> Result<ImageTargets, ParseOutcome> {
    match text {
        "cpu" => Ok(ImageTargets::Cpu),
        "gpu" => Ok(ImageTargets::Gpu),
        other => Err(invalid(flag, other)),
    }
}

fn parse_positive<Number: std::str::FromStr + Default + PartialOrd>(
    flag: &str,
    text: &str,
) -> Result<Number, ParseOutcome> {
    let number: Number = parse_number(flag, text)?;
    if number > Number::default() {
        Ok(number)
    } else {
        Err(ParseOutcome::Error(format!("{flag} must be at least 1")))
    }
}

fn parse_resolution(flag: &str, text: &str) -> Result<[u32; 2], ParseOutcome> {
    let (width, height) = text.split_once('x').ok_or_else(|| invalid(flag, text))?;
    let resolution = [parse_number(flag, width)?, parse_number(flag, height)?];
    if resolution.contains(&0) {
        return Err(invalid(flag, text));
    }
    Ok(resolution)
}

fn parse_vector(flag: &str, text: &str) -> Result<Vec3, ParseOutcome> {
    let components: Vec<f32> = text
        .split(',')
        .map(|component| parse_number(flag, component))
        .collect::<Result<_, _>>()?;
    match components[..] {
        [x, y, z] if x.is_finite() && y.is_finite() && z.is_finite() => Ok(Vec3::new(x, y, z)),
        _ => Err(invalid(flag, text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Options, ParseOutcome> {
        Options::parse(arguments.iter().map(|&argument| argument.to_owned()))
    }

    #[test]
    fn defaults_without_arguments() {
        let options = parse(&[]).unwrap();
        assert_eq!(options.sink, SinkKind::Images);
        assert_eq!(options.eye_resolution, [960, 1000]);
        assert_eq!(
            options.depth_range.encoding,
            DepthEncoding::ForwardNormalized
        );
        assert!(!options.panels);
        assert_eq!(options.panel_capture_interval, Duration::from_millis(250));
        assert_eq!(options.max_users, 2);
        assert_eq!(options.simulated_users, 1);
        assert!(!options.frame_stamp);
        assert!(!options.decoupled_render);
    }

    #[test]
    fn parses_the_multi_user_options() {
        let options = parse(&[
            "--users",
            "3",
            "--join-interval",
            "45",
            "--max-users",
            "4",
            "--frame-stamp",
            "--begin-frame",
            "poll",
        ])
        .unwrap();
        assert_eq!(options.simulated_users, 3);
        assert_eq!(options.join_interval_in_viewer_frames, 45);
        assert_eq!(options.max_users, 4);
        assert!(options.frame_stamp);
        for rejected in [
            &["--max-users", "0"][..],
            &["--users", "0"],
            &["--begin-frame", "later"],
        ] {
            assert!(
                matches!(parse(rejected), Err(ParseOutcome::Error(_))),
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn async_and_blocking_begins_are_refused_with_the_reason() {
        for mode in ["async", "blocking"] {
            let Err(ParseOutcome::Error(message)) = parse(&["--begin-frame", mode]) else {
                panic!("--begin-frame {mode} was accepted");
            };
            assert_eq!(
                message,
                format!(
                    "--begin-frame {mode} is not supported: frames are begun by polling (poll), \
                     which never waits inside the runtime and leaves no begin pending when the \
                     session is destroyed"
                )
            );
        }
    }

    #[test]
    fn parses_every_option() {
        let options = parse(&[
            "--sink",
            "off",
            "--out",
            "/tmp/eyes",
            "--eye-resolution",
            "640x480",
            "--frames",
            "5",
            "--write-every",
            "1",
            "--raw-depth",
            "--motion",
            "still",
            "--radius",
            "2.5",
            "--eye-height",
            "1.4",
            "--period",
            "60",
            "--fov",
            "55",
            "--quit-when-done",
            "--opaque-background",
            "--origin",
            "1,2,-3",
            "--center",
            "10,-20,3",
            "--scale",
            "0.25",
            "--yaw",
            "90",
            "--up",
            "RUB",
            "--near",
            "0.1",
            "--far",
            "50",
            "--depth",
            "linear",
            "--panel-interval-ms",
            "400",
            "--port",
            "9999",
            "--headless",
            "1280x720",
        ])
        .unwrap();
        assert_eq!(options.sink, SinkKind::Off);
        assert_eq!(options.output_directory, PathBuf::from("/tmp/eyes"));
        assert_eq!(options.eye_resolution, [640, 480]);
        assert_eq!(options.stop_after_frames, Some(5));
        assert_eq!(options.write_every_nth_frame, 1);
        assert!(options.write_raw_depth);
        assert_eq!(options.motion, MotionKind::Still);
        assert!((options.circle_radius - 2.5).abs() < f32::EPSILON);
        assert!((options.eye_height - 1.4).abs() < f32::EPSILON);
        assert!((options.circle_period_seconds - 60.0).abs() < f32::EPSILON);
        assert_eq!(options.camera_fov_degrees, Some(55.0));
        assert!(options.quit_when_done);
        assert!(options.opaque_background);
        assert_eq!(options.world_origin, Vec3::new(1.0, 2.0, -3.0));
        assert_eq!(options.world_center, Vec3::new(10.0, -20.0, 3.0));
        assert!((options.world_scale - 0.25).abs() < f32::EPSILON);
        assert_eq!(
            options.world_coordinates_override,
            Some(ViewCoordinates::RIGHT_UP_BACK)
        );
        assert_eq!(options.depth_range.encoding, DepthEncoding::LinearMeters);
        assert!((options.depth_range.far() - 50.0).abs() < f32::EPSILON);
        assert_eq!(options.panel_capture_interval, Duration::from_millis(400));
        assert_eq!(options.grpc_port, 9999);
        assert_eq!(options.headless_size, Some([1280, 720]));
    }

    #[test]
    fn the_gpu_priority_is_high_unless_set_to_normal() {
        assert!(
            parse(&["--sink", "qaros"])
                .unwrap()
                .qaros
                .raise_gpu_priority
        );
        assert!(
            !parse(&["--sink", "qaros", "--gpu-priority", "normal"])
                .unwrap()
                .qaros
                .raise_gpu_priority
        );
        assert!(matches!(
            parse(&["--gpu-priority", "realtime"]),
            Err(ParseOutcome::Error(_))
        ));
    }

    #[test]
    fn the_display_name_labels_the_volume_apart_from_its_common_name() {
        let options = parse(&[
            "--sink",
            "qaros",
            "--volume-name",
            "rerun-air_traffic",
            "--display-name",
            "Rerun air traffic",
        ])
        .unwrap();
        assert_eq!(options.qaros.volume_name, "rerun-air_traffic");
        assert_eq!(options.qaros.display_name, "Rerun air traffic");
        assert_eq!(
            parse(&["--sink", "qaros"]).unwrap().qaros.display_name,
            "Rerun"
        );
        assert!(matches!(
            parse(&["--display-name", " "]),
            Err(ParseOutcome::Error(_))
        ));
    }

    #[test]
    fn the_unanswered_peers_are_kept_next_to_the_onboarding_id() {
        let with_id_file = parse(&["--sink", "qaros", "--onboarding-id-file", "ids/rerun.txt"]);
        assert_eq!(
            with_id_file.unwrap().qaros.unanswered_peers_file(),
            PathBuf::from("ids/qaros-rerun.unanswered-peers.txt")
        );
        assert_eq!(
            parse(&["--sink", "qaros"])
                .unwrap()
                .qaros
                .unanswered_peers_file(),
            PathBuf::from("qaros-rerun.unanswered-peers.txt")
        );
    }

    #[test]
    fn parses_the_qaros_sink_options() {
        let options = parse(&[
            "--sink",
            "qaros",
            "--pairing-code",
            "ABCD-EFGH",
            "--onboarding-id-file",
            "ids/rerun.txt",
            "--qaros-library",
            "C:/qaros/bin/qar-streaming-c.dll",
            "--pose-space",
            "room",
            "--volume-size",
            "3,2,1.5",
            "--volume-name",
            "rerun-air_traffic",
            "--color-format",
            "rgba",
            "--panels",
        ])
        .unwrap();
        assert_eq!(options.qaros.volume_name, "rerun-air_traffic");
        assert_eq!(
            parse(&["--sink", "qaros"]).unwrap().qaros.volume_name,
            "rerun"
        );
        assert!(matches!(
            parse(&["--volume-name", " "]),
            Err(ParseOutcome::Error(_))
        ));
        assert_eq!(options.sink, SinkKind::Qaros);
        assert_eq!(options.stop_after_frames, None);
        assert_eq!(options.qaros.pairing_code.as_deref(), Some("ABCD-EFGH"));
        assert_eq!(
            options.qaros.onboarding_id_file(),
            PathBuf::from("ids/rerun.txt")
        );
        assert_eq!(
            parse(&["--sink", "qaros"])
                .unwrap()
                .qaros
                .onboarding_id_file(),
            PathBuf::from("qaros-rerun.onboarding-id.txt")
        );
        assert_eq!(
            options.qaros.library_path,
            Some(PathBuf::from("C:/qaros/bin/qar-streaming-c.dll"))
        );
        assert_eq!(options.qaros.pose_space, PoseSpaceKind::Room);
        assert_eq!(
            parse(&["--sink", "qaros"]).unwrap().qaros.pose_space,
            PoseSpaceKind::App,
            "app content space by default"
        );
        assert_eq!(options.qaros.volume_size, Vec3::new(3.0, 2.0, 1.5));
        assert_eq!(options.qaros.color_format, ColorFormatKind::Rgba);
        assert!(options.panels);
        assert_eq!(parse(&["--sink", "image"]).unwrap().sink, SinkKind::Images);
        assert!(matches!(
            parse(&["--volume-size", "1,0,1"]),
            Err(ParseOutcome::Error(_))
        ));
        assert!(matches!(
            parse(&["--pose-space", "world"]),
            Err(ParseOutcome::Error(_))
        ));
        assert_eq!(options.qaros.graphics_flag, None);
        assert_eq!(options.qaros.graphics(), QarosGraphics::PLATFORM_DEFAULT);
        assert!(!options.qaros.launched_by_qaros);
        assert_eq!(options.creates_vulkan_device(), cfg!(windows));
    }

    #[test]
    fn panels_are_hidden_at_stop_unless_kept() {
        assert_eq!(
            parse(&[]).unwrap().qaros.panels_at_stop,
            PanelsAtStopKind::Hide
        );
        assert_eq!(
            parse(&["--panels-at-stop", "keep"])
                .unwrap()
                .qaros
                .panels_at_stop,
            PanelsAtStopKind::Keep
        );
        assert!(matches!(
            parse(&["--panels-at-stop", "close"]),
            Err(ParseOutcome::Error(_))
        ));
    }

    #[test]
    fn panels_are_a_switch_of_the_qaros_sink() {
        assert!(parse(&["--sink", "qaros", "--panels"]).unwrap().panels);
        assert!(matches!(
            parse(&["--sink", "image", "--panels"]),
            Err(ParseOutcome::Error(message)) if message.contains("--sink qaros")
        ));
        assert!(matches!(
            parse(&["--sink", "qaros", "--panels", "0.0.0.0:9877"]),
            Err(ParseOutcome::Error(message)) if message.contains("unknown option 0.0.0.0:9877")
        ));
        assert!(matches!(
            parse(&["--sink", "qaros", "--panel-route", "stream"]),
            Err(ParseOutcome::Error(_))
        ));
    }

    #[test]
    fn qaros_graphics_defaults_to_vulkan_on_windows_and_cpu_elsewhere() {
        let expected = if cfg!(windows) {
            QarosGraphics::Vulkan
        } else {
            QarosGraphics::Cpu
        };
        assert_eq!(QarosGraphics::PLATFORM_DEFAULT, expected);
        assert_eq!(
            parse(&["--sink", "qaros"]).unwrap().qaros_vulkan(),
            cfg!(windows)
        );
        let cpu = parse(&["--sink", "qaros", "--qaros-graphics", "cpu"]).unwrap();
        assert_eq!(cpu.qaros.graphics(), QarosGraphics::Cpu);
        assert!(!cpu.creates_vulkan_device());
        let image_sink = parse(&[]).unwrap();
        assert_eq!(image_sink.sink, SinkKind::Images);
        assert!(
            !image_sink.creates_vulkan_device(),
            "the default reaches only the QAROS sink"
        );
    }

    #[test]
    fn a_qaros_launched_viewer_opens_a_window_only_when_asked() {
        let launched = ["--sink", "qaros", "--launched-by-qaros"];
        let windowed = parse(&[&launched[..], &["--window"]].concat()).unwrap();
        assert_eq!(windowed.headless_size, None);
        let sized = parse(&[&launched[..], &["--headless", "800x600"]].concat()).unwrap();
        assert_eq!(sized.headless_size, Some([800, 600]));
        assert_eq!(
            parse(&["--sink", "qaros"]).unwrap().headless_size,
            None,
            "started by hand, the viewer keeps its window"
        );
        assert!(matches!(
            parse(&[&launched[..], &["--window", "--headless", "800x600"]].concat()),
            Err(ParseOutcome::Error(message)) if message.contains("--window")
        ));
    }

    #[test]
    fn a_qaros_launched_viewer_joins_as_a_local_app_only_on_windows() {
        let launched = parse(&["--sink", "qaros", "--launched-by-qaros"]).unwrap();
        assert!(!launched.qaros.local_app);
        assert_eq!(launched.qaros.joins_as_local_app(), cfg!(windows));
    }

    #[test]
    fn parses_local_app_onboarding() {
        let local_app = parse(&["--sink", "qaros", "--launched-by-qaros", "--local-app"]).unwrap();
        assert!(local_app.qaros.local_app);
        assert!(local_app.qaros.launched_by_qaros);
        assert!(local_app.qaros.joins_as_local_app());
        assert!(!parse(&["--sink", "qaros"]).unwrap().qaros.local_app);
        assert!(
            !parse(&["--sink", "qaros"])
                .unwrap()
                .qaros
                .joins_as_local_app()
        );
        assert!(matches!(
            parse(&["--local-app"]),
            Err(ParseOutcome::Error(message)) if message == "--local-app needs --sink qaros"
        ));
        assert!(matches!(
            parse(&["--sink", "qaros", "--local-app", "--pairing-code", "ABCD-EFGH"]),
            Err(ParseOutcome::Error(message))
                if message
                    == "--local-app joins the Hub on this PC without a code; drop --pairing-code"
        ));
        assert!(matches!(
            parse(&["--sink", "qaros", "--local-app", "--onboarding-id-file", "ids/rerun.txt"]),
            Err(ParseOutcome::Error(message))
                if message == "--local-app keeps no onboarding id; drop --onboarding-id-file"
        ));
    }

    #[test]
    fn decoupled_render_needs_a_sink_and_headless_for_the_qaros_vulkan_sink() {
        let images = parse(&["--decoupled-render"]).unwrap();
        assert!(images.decoupled_render);
        assert!(parse(&["--decoupled-render", "--sink", "off"]).is_err());

        let launched = parse(&[
            "--decoupled-render",
            "--sink",
            "qaros",
            "--qaros-graphics",
            "vulkan",
            "--launched-by-qaros",
        ])
        .unwrap();
        assert!(launched.decoupled_render);
        assert_eq!(launched.headless_size, Some([1280, 720]));

        let windowed_vulkan = [
            "--decoupled-render",
            "--sink",
            "qaros",
            "--qaros-graphics",
            "vulkan",
            "--launched-by-qaros",
            "--window",
        ];
        assert_eq!(
            parse(&windowed_vulkan).err(),
            Some(ParseOutcome::Error(
                "--decoupled-render with the QAROS Vulkan sink needs --headless (or \
                 --launched-by-qaros without --window), or --qaros-graphics cpu"
                    .to_owned()
            ))
        );
        let windowed_cpu = parse(&[
            "--decoupled-render",
            "--sink",
            "qaros",
            "--qaros-graphics",
            "cpu",
            "--launched-by-qaros",
            "--window",
        ])
        .unwrap();
        assert!(windowed_cpu.headless_size.is_none());
    }

    #[test]
    fn parses_the_qaros_vulkan_mode_and_launch_contract() {
        let options = parse(&[
            "--sink",
            "qaros",
            "--qaros-graphics",
            "vulkan",
            "--launched-by-qaros",
            "--vulkan-extension",
            "VK_KHR_timeline_semaphore",
        ])
        .unwrap();
        assert_eq!(options.qaros.graphics(), QarosGraphics::Vulkan);
        assert!(options.qaros.launched_by_qaros);
        assert!(options.qaros_vulkan());
        assert!(options.creates_vulkan_device());
        assert!(
            options.vulkan_device.is_none(),
            "the adapter comes from QAROS"
        );
        assert_eq!(options.vulkan_extensions, ["VK_KHR_timeline_semaphore"]);
        assert_eq!(
            options.headless_size,
            Some([1280, 720]),
            "QAROS launches the viewer headless unless asked otherwise"
        );

        let gpu_images = parse(&["--image-targets", "gpu", "--vulkan-device", "default"]).unwrap();
        assert_eq!(gpu_images.image_targets, ImageTargets::Gpu);
        assert_eq!(gpu_images.depth_scale_down, 1);
        assert_eq!(parse(&[]).unwrap().image_targets, ImageTargets::Cpu);
        let smaller_depth = parse(&[
            "--image-targets",
            "gpu",
            "--vulkan-device",
            "default",
            "--depth-scale-down",
            "2",
        ])
        .unwrap();
        assert_eq!(smaller_depth.depth_scale_down, 2);

        for arguments in [
            &["--qaros-graphics", "vulkan"][..],
            &["--launched-by-qaros"],
            &["--sink", "qaros", "--qaros-graphics", "d3d11"],
            &[
                "--sink",
                "qaros",
                "--qaros-graphics",
                "vulkan",
                "--depth",
                "linear",
            ],
            &[
                "--sink",
                "qaros",
                "--qaros-graphics",
                "vulkan",
                "--graphics-backend",
                "metal",
            ],
            &["--image-targets", "gpu"],
            &[
                "--image-targets",
                "gpu",
                "--vulkan-device",
                "default",
                "--sink",
                "off",
            ],
            &[
                "--image-targets",
                "gpu",
                "--vulkan-device",
                "default",
                "--depth",
                "linear",
            ],
            &["--depth-scale-down", "2"],
            &[
                "--sink",
                "qaros",
                "--qaros-graphics",
                "vulkan",
                "--depth-scale-down",
                "2",
            ],
            &[
                "--image-targets",
                "gpu",
                "--vulkan-device",
                "default",
                "--depth-scale-down",
                "0",
            ],
        ] {
            assert!(
                matches!(parse(arguments), Err(ParseOutcome::Error(_))),
                "{arguments:?}"
            );
        }
    }

    #[test]
    fn placement_applies_origin_scale_and_yaw() {
        let options = parse(&["--origin", "0,1,-2", "--scale", "0.5", "--yaw", "90"]).unwrap();
        let room_from_world = options.placement().room_from_world().unwrap();
        let room_point = room_from_world.transform_point3(Vec3::new(0.0, 0.0, -2.0));
        assert!(
            room_point.abs_diff_eq(Vec3::new(-1.0, 1.0, -2.0), 1e-5),
            "{room_point}"
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(parse(&["--help"]).unwrap_err(), ParseOutcome::Help);
        assert!(matches!(
            parse(&["--near", "5", "--far", "1"]),
            Err(ParseOutcome::Error(_))
        ));
        assert!(matches!(
            parse(&["--up", "RFD"]),
            Err(ParseOutcome::Error(_))
        ));
        assert!(matches!(
            parse(&["--eye-resolution", "0x10"]),
            Err(ParseOutcome::Error(_))
        ));
        assert!(matches!(
            parse(&["--origin", "1,2"]),
            Err(ParseOutcome::Error(_))
        ));
        assert!(matches!(
            parse(&["--scale", "-1"]),
            Err(ParseOutcome::Error(_))
        ));
        assert!(matches!(parse(&["--frames"]), Err(ParseOutcome::Error(_))));
        assert!(matches!(parse(&["--bogus"]), Err(ParseOutcome::Error(_))));
    }

    #[test]
    fn parses_the_graphics_device_options() {
        let options = parse(&[
            "--graphics-backend",
            "vulkan",
            "--vulkan-device",
            "luid:0102030405060708",
            "--vulkan-extension",
            "windows-interop",
            "--vulkan-extension",
            "VK_EXT_metal_objects",
        ])
        .unwrap();
        assert_eq!(options.graphics_backend, Some(GraphicsBackend::Vulkan));
        assert_eq!(
            options.vulkan_device,
            Some(PhysicalDeviceSelector::Luid([1, 2, 3, 4, 5, 6, 7, 8]))
        );
        assert_eq!(
            options.vulkan_extensions,
            ["windows-interop", "VK_EXT_metal_objects"]
        );

        let metal = parse(&["--graphics-backend", "metal"]).unwrap();
        assert_eq!(metal.graphics_backend, Some(GraphicsBackend::Metal));
        assert!(metal.vulkan_device.is_none());

        for arguments in [
            &["--graphics-backend", "webgpu"][..],
            &["--vulkan-device", "nvidia"],
            &["--vulkan-extension", "VK_KHR_win32_keyed_mutex"],
            &["--graphics-backend", "metal", "--vulkan-device", "default"],
        ] {
            assert!(
                matches!(parse(arguments), Err(ParseOutcome::Error(_))),
                "{arguments:?}"
            );
        }
    }
}
