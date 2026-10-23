//! The GPU variant of the seam: the sink hands out textures, the renderer draws both eyes into
//! them and gives the sink the command buffer to submit, so the sink can wrap that submission
//! in whatever synchronization its consumer needs.

use crate::{DepthRange, EYE_COUNT, FrameKey, FrameRequest, ShowInfo, SinkError};

/// Where one eye is rendered for one frame. Both are render attachments; the renderer must
/// write every pixel (clear on load), since the sink's images arrive with undefined content.
#[derive(Clone, Debug)]
pub struct GpuEyeTargets {
    /// At [`FrameRequest::resolution_in_pixel`].
    pub color: wgpu::Texture,
    /// `None` when the sink takes no depth. May be smaller (or larger) than color; the renderer
    /// resamples depth to the texture's own size.
    pub depth: Option<wgpu::Texture>,
}

#[derive(Clone, Debug)]
pub struct GpuFrameRequest {
    pub request: FrameRequest,
    /// Indexed like `request.views`.
    pub eyes: [GpuEyeTargets; EYE_COUNT],
}

/// A consumer of stereo frames rendered straight into its textures, for any number of users,
/// the QAROS Vulkan render senders being the one implementation. Same lifecycle as
/// [`crate::FrameSink`]: every frame from `begin_frames` ends in exactly one successful
/// `submit_and_show` or one `abandon_frame`, all on the thread that renders.
pub trait GpuFrameSink {
    /// Applies user joins and leaves, then returns the next frame of every rendered user that
    /// has none open (one frame per user is in flight). Empty while nobody is rendered.
    fn begin_frames(&mut self) -> Result<Vec<GpuFrameRequest>, SinkError>;

    /// As [`crate::FrameSink::update_users`].
    fn update_users(&mut self) {}

    /// Submits `commands`, the renderer's last use of the frame's targets, on the wgpu queue
    /// the sink was made for, then presents the frame with `show`. When it fails, the renderer
    /// calls `abandon_frame`, which does nothing for a frame the sink already let go of.
    fn submit_and_show(
        &mut self,
        key: FrameKey,
        commands: wgpu::CommandBuffer,
        show: ShowInfo,
    ) -> Result<(), SinkError>;

    /// Releases a frame that will not be shown. The renderer has not submitted any work on its
    /// targets.
    fn abandon_frame(&mut self, key: FrameKey);

    /// As [`crate::FrameSink::is_streaming`]: false while nobody is rendered.
    fn is_streaming(&self) -> bool;

    /// As [`crate::FrameSink::is_finished`].
    fn is_finished(&self) -> bool {
        !self.is_streaming()
    }

    /// The encoding the sink expects in the depth target; always a normalized one, since a
    /// depth attachment holds values in `[0, 1]`.
    fn depth_range(&self) -> DepthRange;

    /// The sink also submits to the queue outside wgpu (raw Vulkan batches, queue waits),
    /// which must not interleave with another thread's wgpu submits.
    fn submits_outside_wgpu(&self) -> bool {
        false
    }
}
