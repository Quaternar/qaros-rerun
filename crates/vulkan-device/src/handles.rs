use ash::vk;

use crate::VulkanApi;

/// The raw Vulkan objects behind a wgpu device: what an API that adopts the app's device (as
/// QAROS does in QuaternAR's StereoKit fork) needs. Plain handles, valid while the
/// `wgpu::Device` they were read from lives; wgpu keeps ownership of all of them.
#[derive(Clone, Debug)]
pub struct VulkanDeviceHandles {
    pub instance: vk::Instance,
    /// The loader entry the instance was created through, for an API that resolves its own
    /// function pointers.
    pub get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pub instance_api_version: u32,
    pub physical_device: vk::PhysicalDevice,
    pub device: vk::Device,
    pub queue_family_index: u32,
    pub queue_index: u32,
    /// wgpu submits to this queue from the thread that calls `wgpu::Queue::submit`; anyone else
    /// submitting to it must be ordered with that thread (Vulkan requires queue access to be
    /// externally synchronized).
    pub queue: vk::Queue,
    pub enabled_device_extensions: Vec<String>,
}

impl VulkanDeviceHandles {
    /// `None` when `device` is not on wgpu's Vulkan backend.
    pub fn of(device: &wgpu::Device) -> Option<Self> {
        #[allow(unsafe_code)]
        // SAFETY: only handles and immutable properties are read; nothing is destroyed.
        let hal_device = unsafe { device.as_hal::<VulkanApi>() }?;
        let instance = hal_device.shared_instance();
        Some(Self {
            instance: instance.raw_instance().handle(),
            get_instance_proc_addr: instance.entry().static_fn().get_instance_proc_addr,
            instance_api_version: instance.instance_api_version(),
            physical_device: hal_device.raw_physical_device(),
            device: hal_device.raw_device().handle(),
            queue_family_index: hal_device.queue_family_index(),
            queue_index: hal_device.queue_index(),
            queue: hal_device.raw_queue(),
            enabled_device_extensions: hal_device
                .enabled_device_extensions()
                .iter()
                .map(|extension| extension.to_string_lossy().into_owned())
                .collect(),
        })
    }

    pub fn has_device_extension(&self, name: &std::ffi::CStr) -> bool {
        let name = name.to_string_lossy();
        self.enabled_device_extensions
            .iter()
            .any(|extension| *extension == name)
    }
}
