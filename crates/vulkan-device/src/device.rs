//! Creating the wgpu device ourselves: instance through wgpu, physical device through the
//! picker, `VkDevice` through wgpu-hal with extra extensions, then wrapped back into wgpu.

use std::cell::Cell;
use std::ffi::CStr;
use std::fmt;

use ash::vk;

use crate::VulkanApi;
use crate::handles::VulkanDeviceHandles;
use crate::picker::{PhysicalDeviceIdentity, PhysicalDevicePicker, describe_physical_devices};
use crate::raw_queue::RawQueue;

/// The device extensions a QAROS sink on Windows needs beyond wgpu's own list: shared D3D
/// memory and semaphores, the keyed mutex on shared textures, and timeline semaphores (core in
/// Vulkan 1.2; named so drivers that still list the extension get it explicitly).
pub const WINDOWS_INTEROP_EXTENSIONS: &[&CStr] = &[
    ash::khr::external_memory_win32::NAME,
    ash::khr::external_semaphore_win32::NAME,
    ash::khr::win32_keyed_mutex::NAME,
    ash::khr::timeline_semaphore::NAME,
];

/// The device extensions QAROS's Vulkan render sender requires of the application's device on
/// Windows, as QuaternAR's Vulkan cube enables them
/// (`qar-streaming-c/examples/vulkan_cube/renderer.cpp`): the sender's images and the frame semaphore are shared with
/// the encoder through Win32 handles.
pub const QAROS_WINDOWS_EXTENSIONS: &[&CStr] = &[
    ash::khr::external_memory_win32::NAME,
    ash::khr::external_semaphore_win32::NAME,
];

pub struct VulkanDeviceConfig {
    /// Its backends are narrowed to Vulkan; flags and the rest are kept.
    pub instance_descriptor: wgpu::InstanceDescriptor,
    pub picker: PhysicalDevicePicker,
    /// Added to the extensions wgpu enables itself; every one must be supported.
    pub extra_extensions: Vec<&'static CStr>,
    /// Features, limits and memory hints for the chosen adapter (Rerun's own request).
    pub device_descriptor: Box<dyn FnOnce(&wgpu::Adapter) -> wgpu::DeviceDescriptor<'static>>,
    /// Ask for a second queue in wgpu's queue family for raw batches, used when the family has
    /// one; `false` keeps raw batches on wgpu's queue.
    pub separate_raw_queue: bool,
}

/// wgpu takes queue 0 of its family; raw batches get queue 1.
const RAW_QUEUE_INDEX: u32 = 1;

/// Priorities of wgpu's queue and the raw batches' queue. `vkCreateDevice` reads them after the
/// device creation callback has returned, so they cannot live in the callback.
static TWO_QUEUE_PRIORITIES: [f32; 2] = [1.0, 1.0];

/// A wgpu device on a `VkDevice` this process created, ready for
/// `egui_wgpu::WgpuSetup::Existing`.
pub struct VulkanDevice {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub physical_device: PhysicalDeviceIdentity,
    /// Valid for as long as `device` lives.
    pub handles: VulkanDeviceHandles,
    /// Where raw batches go: queue 1 of wgpu's family when the device has it, else wgpu's queue.
    pub raw_queue: RawQueue,
}

#[derive(Debug)]
pub enum VulkanDeviceError {
    /// wgpu created no Vulkan instance: no loader on the library path, or no ICD.
    NoVulkanInstance,
    NoVulkanAdapter,
    Picker(String),
    /// The picker chose a physical device that wgpu does not expose as an adapter.
    PickedDeviceNotUsable(String),
    UnsupportedExtensions {
        device: String,
        missing: Vec<String>,
    },
    DeviceCreation(String),
}

impl fmt::Display for VulkanDeviceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoVulkanInstance => write!(
                formatter,
                "wgpu has no Vulkan instance; is the Vulkan loader on the library path? On macOS: \
                 DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib with Homebrew's vulkan-loader and molten-vk"
            ),
            Self::NoVulkanAdapter => write!(formatter, "the Vulkan instance has no usable adapter"),
            Self::Picker(reason) => write!(formatter, "physical device picker: {reason}"),
            Self::PickedDeviceNotUsable(device) => {
                write!(
                    formatter,
                    "wgpu does not expose the picked physical device {device}"
                )
            }
            Self::UnsupportedExtensions { device, missing } => write!(
                formatter,
                "{device} does not support the device extensions {}",
                missing.join(", ")
            ),
            Self::DeviceCreation(reason) => {
                write!(formatter, "creating the Vulkan device: {reason}")
            }
        }
    }
}

impl std::error::Error for VulkanDeviceError {}

impl VulkanDevice {
    pub async fn create(config: VulkanDeviceConfig) -> Result<Self, VulkanDeviceError> {
        let VulkanDeviceConfig {
            mut instance_descriptor,
            picker,
            extra_extensions,
            device_descriptor,
            separate_raw_queue,
        } = config;
        instance_descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(instance_descriptor);

        let picked = pick_physical_device(&instance, picker)?;
        let adapter = find_adapter(&instance, picked).await?;
        let physical_device = adapter_identity(&instance, &adapter)?;
        log::info!("vulkan-device: creating the device on {physical_device}");

        let descriptor = device_descriptor(&adapter);
        let raw_queue_requested = Cell::new(false);
        let open_device = open_device(
            &adapter,
            &physical_device,
            &descriptor,
            extra_extensions,
            separate_raw_queue.then_some(&raw_queue_requested),
        )?;
        #[allow(unsafe_code)]
        // SAFETY: `open_device` was opened on this adapter's physical device with the features
        // and limits of `descriptor`, which the adapter reported as supported.
        let (device, queue) =
            unsafe { adapter.create_device_from_hal(open_device, &descriptor) }
                .map_err(|error| VulkanDeviceError::DeviceCreation(error.to_string()))?;
        let not_on_vulkan =
            || VulkanDeviceError::DeviceCreation("the new device is not on Vulkan".to_owned());
        let handles = VulkanDeviceHandles::of(&device).ok_or_else(not_on_vulkan)?;
        let raw_queue = if raw_queue_requested.get() {
            #[allow(unsafe_code)]
            // SAFETY: the callback raised wgpu's queue create info to two queues.
            unsafe { RawQueue::beside_wgpu(&device, RAW_QUEUE_INDEX) }.ok_or_else(|| {
                VulkanDeviceError::DeviceCreation(format!(
                    "wgpu took queue {} of family {}, the queue meant for raw batches",
                    handles.queue_index, handles.queue_family_index
                ))
            })?
        } else {
            RawQueue::of_wgpu(&device).ok_or_else(not_on_vulkan)?
        };
        log::info!("vulkan-device: raw batches go to {raw_queue:?}");

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            physical_device,
            handles,
            raw_queue,
        })
    }
}

/// What [`VulkanDevice::create`] would do for raw batches on the physical device a picker
/// chooses, read without creating a device.
#[derive(Clone, Debug)]
pub struct RawQueueForecast {
    pub physical_device: PhysicalDeviceIdentity,
    /// Queues in the family wgpu-hal 30 opens, family 0 (`adapter.rs` L3005).
    pub wgpu_family_queue_count: u32,
}

impl RawQueueForecast {
    /// Picks the physical device as [`VulkanDevice::create`] does, on an instance of its own.
    pub async fn of(
        mut instance_descriptor: wgpu::InstanceDescriptor,
        picker: PhysicalDevicePicker,
    ) -> Result<Self, VulkanDeviceError> {
        instance_descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(instance_descriptor);
        let chosen_device = pick_physical_device(&instance, picker)?;
        let adapter = find_adapter(&instance, chosen_device).await?;
        let physical_device = adapter_identity(&instance, &adapter)?;
        #[allow(unsafe_code)]
        // SAFETY: the hal adapter is only borrowed for the query, while `adapter` lives.
        let hal_adapter =
            unsafe { adapter.as_hal::<VulkanApi>() }.ok_or(VulkanDeviceError::NoVulkanAdapter)?;
        #[allow(unsafe_code)]
        // SAFETY: the physical device handle is the adapter's own, valid while `adapter` lives.
        let families = unsafe {
            hal_adapter
                .shared_instance()
                .raw_instance()
                .get_physical_device_queue_family_properties(hal_adapter.raw_physical_device())
        };
        Ok(Self {
            physical_device,
            wgpu_family_queue_count: families.first().map_or(0, |family| family.queue_count),
        })
    }

    pub fn raw_batches_get_a_queue_of_their_own(&self) -> bool {
        self.wgpu_family_queue_count > RAW_QUEUE_INDEX
    }
}

/// Raises wgpu's queue create info to two queues when its family has them, and records that in
/// `requested`. wgpu-hal passes the create infos to `vkCreateDevice` as left here and takes
/// queue 0 of its family whatever their count.
fn request_raw_queue(
    queue_create_infos: &mut [vk::DeviceQueueCreateInfo<'_>],
    family_queue_counts: &[u32],
    requested: &Cell<bool>,
) {
    let Some(wgpu_queue_info) = queue_create_infos.first_mut() else {
        return;
    };
    let family_queue_count = usize::try_from(wgpu_queue_info.queue_family_index)
        .ok()
        .and_then(|family| family_queue_counts.get(family))
        .copied()
        .unwrap_or(0);
    if wgpu_queue_info.queue_count != 1 || family_queue_count <= RAW_QUEUE_INDEX {
        log::info!(
            "vulkan-device: queue family {} has {family_queue_count} queue(s); raw batches share \
             wgpu's queue",
            wgpu_queue_info.queue_family_index
        );
        return;
    }
    *wgpu_queue_info = wgpu_queue_info.queue_priorities(&TWO_QUEUE_PRIORITIES);
    requested.set(true);
}

fn pick_physical_device(
    instance: &wgpu::Instance,
    picker: PhysicalDevicePicker,
) -> Result<Option<vk::PhysicalDevice>, VulkanDeviceError> {
    #[allow(unsafe_code)]
    // SAFETY: the hal instance is only borrowed for the picker call, while `instance` lives.
    let hal_instance =
        unsafe { instance.as_hal::<VulkanApi>() }.ok_or(VulkanDeviceError::NoVulkanInstance)?;
    picker(hal_instance.shared_instance().raw_instance()).map_err(VulkanDeviceError::Picker)
}

async fn find_adapter(
    instance: &wgpu::Instance,
    picked: Option<vk::PhysicalDevice>,
) -> Result<wgpu::Adapter, VulkanDeviceError> {
    let adapters = instance.enumerate_adapters(wgpu::Backends::VULKAN).await;
    let Some(picked) = picked else {
        let adapter = adapters
            .into_iter()
            .next()
            .ok_or(VulkanDeviceError::NoVulkanAdapter)?;
        log::info!("vulkan-device: the picker kept the default, wgpu's first Vulkan adapter");
        return Ok(adapter);
    };
    adapters
        .into_iter()
        .find(|adapter| raw_physical_device(adapter) == Some(picked))
        .ok_or_else(|| VulkanDeviceError::PickedDeviceNotUsable(format!("{picked:?}")))
}

fn raw_physical_device(adapter: &wgpu::Adapter) -> Option<vk::PhysicalDevice> {
    #[allow(unsafe_code)]
    // SAFETY: only the physical device handle is read.
    unsafe { adapter.as_hal::<VulkanApi>() }.map(|hal_adapter| hal_adapter.raw_physical_device())
}

fn adapter_identity(
    instance: &wgpu::Instance,
    adapter: &wgpu::Adapter,
) -> Result<PhysicalDeviceIdentity, VulkanDeviceError> {
    let handle = raw_physical_device(adapter).ok_or(VulkanDeviceError::NoVulkanAdapter)?;
    #[allow(unsafe_code)]
    // SAFETY: as in `pick_physical_device`.
    let hal_instance =
        unsafe { instance.as_hal::<VulkanApi>() }.ok_or(VulkanDeviceError::NoVulkanInstance)?;
    describe_physical_devices(hal_instance.shared_instance().raw_instance())
        .map_err(|error| VulkanDeviceError::Picker(error.to_string()))?
        .into_iter()
        .find(|identity| identity.handle == handle)
        .ok_or(VulkanDeviceError::NoVulkanAdapter)
}

fn open_device(
    adapter: &wgpu::Adapter,
    physical_device: &PhysicalDeviceIdentity,
    descriptor: &wgpu::DeviceDescriptor<'static>,
    extra_extensions: Vec<&'static CStr>,
    raw_queue_requested: Option<&Cell<bool>>,
) -> Result<wgpu::hal::OpenDevice<VulkanApi>, VulkanDeviceError> {
    #[allow(unsafe_code)]
    // SAFETY: the hal adapter is only borrowed for this function, while `adapter` lives.
    let hal_adapter =
        unsafe { adapter.as_hal::<VulkanApi>() }.ok_or(VulkanDeviceError::NoVulkanAdapter)?;
    let capabilities = hal_adapter.physical_device_capabilities();
    let missing: Vec<String> = extra_extensions
        .iter()
        .filter(|&&extension| !capabilities.supports_extension(extension))
        .map(|extension| extension.to_string_lossy().into_owned())
        .collect();
    if !missing.is_empty() {
        return Err(VulkanDeviceError::UnsupportedExtensions {
            device: physical_device.name.clone(),
            missing,
        });
    }

    let mut extra_extensions = extra_extensions;
    // wgpu-hal 30 enables it on its own too; named here so the budget query does not rest on that.
    if capabilities.supports_extension(ash::ext::memory_budget::NAME)
        && !extra_extensions.contains(&ash::ext::memory_budget::NAME)
    {
        extra_extensions.push(ash::ext::memory_budget::NAME);
    }

    #[allow(unsafe_code)]
    // SAFETY: the physical device handle is the adapter's own, valid while `adapter` lives.
    let family_queue_counts: Vec<u32> = unsafe {
        hal_adapter
            .shared_instance()
            .raw_instance()
            .get_physical_device_queue_family_properties(hal_adapter.raw_physical_device())
    }
    .iter()
    .map(|family| family.queue_count)
    .collect();

    let adjust_device =
        move |arguments: wgpu::hal::vulkan::CreateDeviceCallbackArgs<'_, '_, '_>| {
            if let Some(requested) = raw_queue_requested {
                request_raw_queue(
                    arguments.queue_create_infos,
                    &family_queue_counts,
                    requested,
                );
            }
            for extension in extra_extensions {
                if !arguments.extensions.contains(&extension) {
                    arguments.extensions.push(extension);
                }
            }
            log::info!(
                "vulkan-device: enabling device extensions {}",
                arguments
                    .extensions
                    .iter()
                    .map(|extension| extension.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };
    #[allow(unsafe_code)]
    // SAFETY: the features and limits are the adapter's own (Rerun derives them from it), and
    // the callback only adds extensions the physical device supports, as checked above, and
    // queues its queue family has.
    unsafe {
        hal_adapter.open_with_callback(
            descriptor.required_features,
            &descriptor.required_limits,
            &descriptor.memory_hints,
            Some(Box::new(adjust_device)),
        )
    }
    .map_err(|error| VulkanDeviceError::DeviceCreation(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    static ONE_QUEUE_PRIORITY: [f32; 1] = [1.0];

    fn wgpu_queue_info(family: u32) -> vk::DeviceQueueCreateInfo<'static> {
        vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&ONE_QUEUE_PRIORITY)
    }

    #[allow(unsafe_code)]
    fn priorities(info: &vk::DeviceQueueCreateInfo<'_>) -> Vec<f32> {
        let count = usize::try_from(info.queue_count).unwrap_or(0);
        // SAFETY: the pointer and count were set together by `queue_priorities` from a static.
        unsafe { std::slice::from_raw_parts(info.p_queue_priorities, count) }.to_vec()
    }

    #[test]
    fn a_family_with_two_queues_gives_wgpu_queue_0_and_raw_batches_queue_1() {
        let mut infos = vec![wgpu_queue_info(1)];
        let requested = Cell::new(false);
        request_raw_queue(&mut infos, &[1, 16], &requested);
        assert!(requested.get());
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].queue_family_index, 1);
        assert_eq!(infos[0].queue_count, 2);
        assert_eq!(priorities(&infos[0]), vec![1.0, 1.0]);
    }

    #[test]
    fn a_family_with_one_queue_keeps_wgpus_create_info() {
        let mut infos = vec![wgpu_queue_info(0)];
        let requested = Cell::new(false);
        request_raw_queue(&mut infos, &[1, 16], &requested);
        assert!(!requested.get());
        assert_eq!(infos[0].queue_count, 1);
        assert_eq!(priorities(&infos[0]), vec![1.0]);

        let mut unknown_family = vec![wgpu_queue_info(5)];
        request_raw_queue(&mut unknown_family, &[16], &requested);
        assert!(!requested.get());
        assert_eq!(unknown_family[0].queue_count, 1);
    }
}
