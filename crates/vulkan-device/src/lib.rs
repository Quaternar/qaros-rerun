//! An app-created Vulkan device for the viewer, the part of "Rerun renders into QAROS via
//! Vulkan" that does not need the QAROS Vulkan API.
//!
//! - [`VulkanDevice::create`] creates the wgpu device itself instead of leaving it to egui: the
//!   physical device comes from a [`PhysicalDevicePicker`] given the `VkInstance` (as in
//!   QuaternAR's StereoKit fork, `qar_vulkan_set_device_picker`), and the `VkDevice` is opened through wgpu-hal
//!   with extra device extensions that wgpu never enables on its own. The result goes to
//!   eframe and egui as `egui_wgpu::WgpuSetup::Existing`.
//! - [`VulkanDeviceHandles`] are the raw handles an API that adopts the app's device needs: instance,
//!   physical device, device, queue family, queue index and queue.
//! - [`texture_from_vk_image`] wraps a `VkImage` created elsewhere on the same device as a
//!   `wgpu::Texture`.
//! - [`stage_wait_semaphore`], [`stage_signal_semaphore`] and [`submit_sync_batch`] put
//!   semaphores around wgpu's next submission, directly or through an empty raw batch.
//! - [`HandoverQueue`] wraps wgpu's submission in raw batches that take images over from their
//!   owner (layout and queue family as reported) and hand them back: final layout, released
//!   to the owner's queue family (`VK_QUEUE_FAMILY_EXTERNAL`), the owner's semaphores
//!   signalled.
//! - [`memory_heap_budgets`] reads every memory heap's size, budget and usage
//!   (`VK_EXT_memory_budget`, enabled on the device whenever the physical device has it).
//!
//! Every item works on any wgpu device on the Vulkan backend, the ones Rerun creates included;
//! the raw handles stay valid for as long as that `wgpu::Device` lives. The design and the open
//! assumptions are in `docs/vulkan-device.md`.

mod device;
mod external_image;
mod handles;
mod handover;
mod memory_budget;
mod picker;
mod semaphores;

pub use ash::vk;
pub use device::{
    QAROS_WINDOWS_EXTENSIONS, VulkanDevice, VulkanDeviceConfig, VulkanDeviceError,
    WINDOWS_INTEROP_EXTENSIONS,
};
pub use external_image::texture_from_vk_image;
pub use handles::VulkanDeviceHandles;
pub use handover::{
    HandoverError, HandoverQueue, ImageAcquire, ImageRelease, attachment_layout, image_aspect,
};
pub use memory_budget::{MemoryHeapBudget, MemoryHeapBudgets, memory_heap_budgets};
pub use picker::{
    PhysicalDeviceIdentity, PhysicalDevicePicker, PhysicalDeviceSelector, describe_physical_devices,
};
pub use semaphores::{
    KeyedMutexAcquire, KeyedMutexRelease, KeyedMutexSync, SemaphoreSignal, SemaphoreWait,
    stage_signal_semaphore, stage_wait_semaphore, submit_sync_batch, wait_queue_idle,
};

/// wgpu-hal's Vulkan API, the type `as_hal` is asked for.
pub type VulkanApi = wgpu::hal::api::Vulkan;
