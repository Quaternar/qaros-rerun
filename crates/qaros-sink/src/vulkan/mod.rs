//! The Vulkan mode of the QAROS sink (`vulkan` feature): Rerun renders each eye straight into
//! the render sender's images on the viewer's own Vulkan device, which QAROS adopts.
//!
//! - [`VulkanSink`] is the frame handling, generic over a [`VulkanFrameStream`].
//! - [`QarosVulkanStream`] is QAROS's Vulkan render sender as that stream; tests also drive
//!   the sink with images and a semaphore of their own.

mod qaros_stream;
mod sink;
mod stream;

pub use qaros_stream::{QarosVulkanStream, QarosVulkanStreamOptions};
pub use sink::{VulkanSink, VulkanSinkOptions};
pub use stream::{ImageState, SenderEye, SenderFrame, SenderImage, VulkanFrameStream};
