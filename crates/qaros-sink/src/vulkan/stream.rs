//! What the Vulkan sink needs from a QAROS Vulkan render sender, kept as a small trait so the
//! frame handling (wrapping, release, write-back, discard) does not depend on the binding's
//! shape and can be tested with images of our own.

use frame_sink::{EYE_COUNT, ShowInfo, SinkError, UserId};
use vulkan_device::vk;
use xr_math::{Fov, Pose};

/// One image of a sender frame, as `QarVideoTextureVulkan` describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SenderImage {
    pub image: vk::Image,
    pub format: wgpu::TextureFormat,
    pub resolution_in_pixel: [u32; 2],
    /// The features of the image's format with its tiling; rendering needs
    /// `COLOR_ATTACHMENT` or `DEPTH_STENCIL_ATTACHMENT`.
    pub format_features: vk::FormatFeatureFlags,
    /// The layout and queue family the image is in when the frame begins.
    pub layout: vk::ImageLayout,
    pub queue_family_index: u32,
}

/// One eye of a sender frame: where to render it from and into which images.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SenderEye {
    /// `room_from_eye`, already mapped into room space by the stream.
    pub pose: Pose,
    pub fov: Fov,
    /// Index into [`SenderFrame::images`].
    pub color_image: usize,
    pub depth_image: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SenderFrame {
    pub eyes: [SenderEye; EYE_COUNT],
    pub images: Vec<SenderImage>,
    /// `synchronization.semaphore`: signalled exactly once for every begun frame, shown or
    /// discarded, so the sender can consume it.
    pub semaphore: vk::Semaphore,
}

/// An image's layout and queue family after the frame's last batch, written back into the
/// sender's frame before it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageState {
    pub layout: vk::ImageLayout,
    pub queue_family_index: u32,
}

/// A source of Vulkan frames for any number of users: follows who joins and leaves, keeps one
/// render sender per rendered user, and runs each sender's frame cycle. Called on the thread
/// that renders.
pub trait VulkanFrameStream {
    /// Applies user joins and leaves: creates the sender of each admitted user and drops the
    /// sender of each released one. A frame still open on a dropped sender has its semaphore
    /// signalled first.
    fn update_users(&mut self);

    /// Just before frames are begun: releases the users whose targets asked for no frame in
    /// the stall timeout.
    fn release_stalled_users(&mut self) {}

    /// Beginning the user's frame failed, in the stream or in the sink (its images cannot be
    /// rendered into); the stream gives the user up once its begins keep failing.
    fn begin_failed(&mut self, user: UserId, error: &SinkError) {
        log::warn!("qaros: cannot begin a frame for {user}: {error}");
    }

    /// The users with a sender, in a stable order.
    fn users(&self) -> Vec<UserId>;

    /// Begins the user's next frame when its sender has one ready; `Ok(None)` while the user
    /// has no sender or the sender has no frame yet.
    fn try_begin_frame(&mut self, user: UserId) -> Result<Option<SenderFrame>, SinkError>;

    /// Writes `states` (one per image of the begun frame, in order) back into the user's
    /// frame, sets its fence to null, and shows it. The frame is over either way; its semaphore
    /// is signalled already, so a stream may give up the user's sender when this fails.
    fn show_frame(
        &mut self,
        user: UserId,
        states: &[ImageState],
        show: ShowInfo,
    ) -> Result<(), SinkError>;

    /// Ends the user's begun frame without showing it. Its semaphore has been signalled
    /// already.
    fn discard_frame(&mut self, user: UserId);
}
