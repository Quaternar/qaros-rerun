use std::ffi::CStr;

use vulkan_device::{
    PhysicalDevicePicker, PhysicalDeviceSelector, VulkanApi, VulkanDevice, VulkanDeviceConfig,
    VulkanDeviceError,
};

/// An extension wgpu does not enable itself but MoltenVK offers, to prove the callback path.
#[cfg(target_vendor = "apple")]
pub const EXTRA_EXTENSIONS: &[&CStr] = &[ash::ext::metal_objects::NAME];
#[cfg(not(target_vendor = "apple"))]
pub const EXTRA_EXTENSIONS: &[&CStr] = &[];

pub fn config(
    picker: PhysicalDevicePicker,
    extra_extensions: &[&'static CStr],
) -> VulkanDeviceConfig {
    VulkanDeviceConfig {
        instance_descriptor: wgpu::InstanceDescriptor::new_without_display_handle(),
        picker,
        extra_extensions: extra_extensions.to_vec(),
        device_descriptor: Box::new(|adapter| wgpu::DeviceDescriptor {
            label: Some("vulkan-device test"),
            required_limits: adapter.limits(),
            ..Default::default()
        }),
        separate_raw_queue: true,
    }
}

/// The device the tests run on, or `None` (with a note) where this machine has no Vulkan loader
/// on the library path; on a Mac with Homebrew's vulkan-loader and molten-vk, run the tests with
/// `DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib "$(rustup which cargo)" test`: Homebrew's
/// `cargo` is a shell script, and macOS strips `DYLD_*` variables from it.
pub fn create_or_skip(config: VulkanDeviceConfig) -> Option<VulkanDevice> {
    match pollster::block_on(VulkanDevice::create(config)) {
        Ok(device) => Some(device),
        Err(VulkanDeviceError::NoVulkanInstance) => {
            assert!(
                !ash_loader_loads(),
                "the Vulkan loader loads, yet wgpu created no Vulkan instance"
            );
            eprintln!(
                "skipped: no Vulkan loader on the library path ({})",
                VulkanDeviceError::NoVulkanInstance
            );
            None
        }
        Err(error) => panic!("creating the Vulkan device: {error}"),
    }
}

#[allow(dead_code)]
pub fn default_device() -> Option<VulkanDevice> {
    create_or_skip(config(test_selector().into_picker(), EXTRA_EXTENSIONS))
}

/// `VULKAN_DEVICE_TEST_SELECTOR` (a `PhysicalDeviceSelector`, `discrete` for example) picks the
/// device on a machine with several, such as a laptop whose first adapter is the integrated GPU.
pub fn test_selector() -> PhysicalDeviceSelector {
    match std::env::var("VULKAN_DEVICE_TEST_SELECTOR") {
        Ok(text) => PhysicalDeviceSelector::parse(&text).expect("VULKAN_DEVICE_TEST_SELECTOR"),
        Err(_) => PhysicalDeviceSelector::Default,
    }
}

/// The default device with raw batches on a queue of their own where its family has two, and
/// again with raw batches on wgpu's queue; empty where there is no Vulkan loader.
#[allow(dead_code)]
pub fn devices_for_both_raw_queues() -> Vec<VulkanDevice> {
    [true, false]
        .into_iter()
        .filter_map(|separate_raw_queue| {
            create_or_skip(VulkanDeviceConfig {
                separate_raw_queue,
                ..config(test_selector().into_picker(), EXTRA_EXTENSIONS)
            })
        })
        .collect()
}

/// How many queues wgpu's queue family has on this device.
#[allow(unsafe_code, dead_code)]
pub fn wgpu_family_queue_count(vulkan: &VulkanDevice) -> u32 {
    // SAFETY: only properties of the device's own physical device are read.
    let hal_device = unsafe { vulkan.device.as_hal::<VulkanApi>() }.expect("a Vulkan device");
    let families = unsafe {
        hal_device
            .shared_instance()
            .raw_instance()
            .get_physical_device_queue_family_properties(hal_device.raw_physical_device())
    };
    families[vulkan.handles.queue_family_index as usize].queue_count
}

#[allow(unsafe_code)]
fn ash_loader_loads() -> bool {
    // SAFETY: loading the loader library runs no Vulkan code of ours.
    unsafe { ash::Entry::load() }.is_ok()
}

#[allow(unsafe_code, dead_code)]
pub fn ash_device(device: &wgpu::Device) -> ash::Device {
    // SAFETY: the clone is a function table plus the handle; the tests keep `device` alive
    // for as long as they use it.
    unsafe { device.as_hal::<VulkanApi>() }
        .expect("a Vulkan device")
        .raw_device()
        .clone()
}
