//! The seam between the headset renderer and whatever consumes its frames.
//!
//! The renderer asks the sink for frames, one per rendered user (two eye poses and fields of
//! view each), renders every eye, then hands back color and depth per eye and shows each frame
//! with the near and far planes it rendered with. The QAROS render senders (one per user, in
//! `qaros-sink`) are one implementation; [`ImageSink`] writes PNGs per simulated user so the
//! renderer can be checked on a Mac.
//!
//! With the `gpu` feature, `GpuFrameSink` is the same seam for sinks whose textures the
//! renderer draws into directly (the QAROS Vulkan render senders), and `GpuReadbackSink` puts
//! any `FrameSink` behind it.

pub mod frame_stamp;
#[cfg(feature = "gpu")]
mod gpu;
#[cfg(feature = "gpu")]
mod gpu_readback;
mod image_sink;
pub mod perf;
mod pose_source;
mod users;

pub use frame_stamp::{FrameStamp, stamp_eye};
#[cfg(feature = "gpu")]
pub use gpu::{GpuEyeTargets, GpuFrameRequest, GpuFrameSink};
#[cfg(feature = "gpu")]
pub use gpu_readback::{GpuReadbackSink, READBACK_COLOR_FORMAT, READBACK_DEPTH_FORMAT};
pub use image_sink::{ImageSink, ImageSinkOptions, SimulatedUser, depth_visualization};
pub use pose_source::{HeadMotion, PoseSource, ScriptedHeadMotion, camera_like_fov};
pub use users::{MaxUsers, RenderPolicy, RosterChange, UserEvent, UserId, UserInfo, UserRoster};
pub use xr_math::{DepthEncoding, DepthRange, Fov, NearFar, Pose, VolumeTransform};

/// Counts one user's frames from `begin_frames` until each is shown or abandoned. Ids are
/// per user and start at 0 when the user is admitted.
pub type FrameId = u64;

pub const EYE_COUNT: usize = 2;

/// One frame of one user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameKey {
    pub user: UserId,
    pub frame_id: FrameId,
}

impl std::fmt::Display for FrameKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} frame {}", self.user, self.frame_id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewRequest {
    /// `room_from_eye`, like a QAROS view pose.
    pub pose: Pose,
    pub fov: Fov,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameRequest {
    pub key: FrameKey,
    /// The poses this frame must be rendered with, as issued for exactly this frame.
    pub views: [ViewRequest; EYE_COUNT],
    /// Per-eye texture size, as the stream layout dictates.
    pub resolution_in_pixel: [u32; 2],
    /// The app volume transform the sink mapped this frame's poses with, which the renderer
    /// places Rerun's world in for this frame; `None` keeps the renderer's own placement.
    pub volume: Option<VolumeTransform>,
}

/// One rendered eye: tightly packed rows, top row first.
#[derive(Clone, Debug, PartialEq)]
pub struct EyeImage {
    pub resolution_in_pixel: [u32; 2],
    /// RGBA, 8 bits per channel, sRGB-encoded color with premultiplied alpha (ADR-0050).
    pub color_rgba8: Vec<u8>,
    /// One float per pixel, already in the sink's depth encoding.
    pub depth: Vec<f32>,
}

impl EyeImage {
    pub fn pixel_count(&self) -> usize {
        self.resolution_in_pixel[0] as usize * self.resolution_in_pixel[1] as usize
    }

    pub fn validate(&self) -> Result<(), SinkError> {
        let pixel_count = self.pixel_count();
        if self.color_rgba8.len() != pixel_count * 4 {
            return Err(SinkError::InvalidImage(format!(
                "color has {} bytes, expected {} for {:?}",
                self.color_rgba8.len(),
                pixel_count * 4,
                self.resolution_in_pixel
            )));
        }
        if self.depth.len() != pixel_count {
            return Err(SinkError::InvalidImage(format!(
                "depth has {} values, expected {pixel_count} for {:?}",
                self.depth.len(),
                self.resolution_in_pixel
            )));
        }
        Ok(())
    }
}

/// What `QarRenderFrameShow` carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShowInfo {
    pub near_far: NearFar,
    pub depth_scale: f32,
}

impl ShowInfo {
    pub fn from_depth_range(depth_range: DepthRange) -> Self {
        Self {
            near_far: depth_range.reported_near_far(),
            depth_scale: depth_range.depth_scale(),
        }
    }
}

#[derive(Debug)]
pub enum SinkError {
    UnknownFrame(FrameKey),
    UnknownView(usize),
    InvalidImage(String),
    Io(std::io::Error),
    Backend(String),
}

impl std::fmt::Display for SinkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFrame(key) => write!(formatter, "{key} is not in flight"),
            Self::UnknownView(view_index) => write!(formatter, "no view with index {view_index}"),
            Self::InvalidImage(reason) => write!(formatter, "invalid eye image: {reason}"),
            Self::Io(error) => write!(formatter, "i/o: {error}"),
            Self::Backend(reason) => write!(formatter, "sink backend: {reason}"),
        }
    }
}

impl std::error::Error for SinkError {}

impl From<std::io::Error> for SinkError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// A consumer of stereo frames for any number of users, shaped after the QAROS render sender
/// loop per user (`begin_frame`, fill the CPU frame, `show_frame`; `rendering-streams.mdx`
/// L118-149).
///
/// Every call happens on the thread that renders, the viewer's UI thread. Readback is
/// asynchronous, so a frame stays in flight for one or more viewer frames between
/// `begin_frames` and `show_frame`.
///
/// Every frame returned by `begin_frames` ends in exactly one successful `show_frame` or one
/// `abandon_frame`: when `submit_view` or `show_frame` fails, the renderer calls
/// `abandon_frame` for that frame. Frames of a user the sink has released fail with
/// [`SinkError::UnknownFrame`], and abandoning them is a no-op.
pub trait FrameSink {
    /// Applies user joins and leaves, then returns the next frame of every rendered user that
    /// has none open (one frame per user is in flight). Empty while nobody is rendered.
    fn begin_frames(&mut self) -> Result<Vec<FrameRequest>, SinkError>;

    /// Applies user joins and leaves without beginning frames, for a viewer frame in which
    /// `begin_frames` is not called (no 3D view streams), so users are admitted and released
    /// every viewer frame.
    fn update_users(&mut self) {}

    fn submit_view(
        &mut self,
        key: FrameKey,
        view_index: usize,
        image: EyeImage,
    ) -> Result<(), SinkError>;

    /// Presents the frame. `show` must carry the near and far planes the views were rendered
    /// with; the QAROS mixer reprojects with exactly these values.
    fn show_frame(&mut self, key: FrameKey, show: ShowInfo) -> Result<(), SinkError>;

    /// Releases a frame that will not be shown, for example after a failed readback.
    fn abandon_frame(&mut self, key: FrameKey);

    /// True while frames should be produced; the viewer repaints continuously while this holds.
    /// False while nobody is rendered, so an idle viewer rests.
    fn is_streaming(&self) -> bool;

    /// True once the sink will produce no more frames (a frame limit reached), as opposed to
    /// idle while nobody is rendered.
    fn is_finished(&self) -> bool {
        !self.is_streaming()
    }

    /// The encoding the sink expects in [`EyeImage::depth`].
    fn depth_range(&self) -> DepthRange;
}
