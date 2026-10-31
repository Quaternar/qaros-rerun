//! Creating the wgpu device ourselves: instance through wgpu, physical device through the
//! picker, `VkDevice` through wgpu-hal with extra extensions, then wrapped back into wgpu.

use std::ffi::CStr;
use std::fmt;

use ash::vk;

use crate::VulkanApi;
use crate::handles::VulkanDeviceHandles;
use crate::picker::{PhysicalDeviceIdentity, PhysicalDevicePicker, describe_physical_devices};

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
}

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
        } = config;
        instance_descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(instance_descriptor);

        let picked = pick_physical_device(&instance, picker)?;
        let adapter = find_adapter(&instance, picked).await?;
        let physical_device = adapter_identity(&instance, &adapter)?;
        log::info!("vulkan-device: creating the device on {physical_device}");

        let descriptor = device_descriptor(&adapter);
        let open_device = open_device(&adapter, &physical_device, &descriptor, extra_extensions)?;
        #[allow(unsafe_code)]
        // SAFETY: `open_device` was opened on this adapter's physical device with the features
        // and limits of `descriptor`, which the adapter reported as supported.
        let (device, queue) =
            unsafe { adapter.create_device_from_hal(open_device, &descriptor) }
                .map_err(|error| VulkanDeviceError::DeviceCreation(error.to_string()))?;
        let handles = VulkanDeviceHandles::of(&device).ok_or_else(|| {
            VulkanDeviceError::DeviceCreation("the new device is not on Vulkan".to_owned())
        })?;

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            physical_device,
            handles,
        })
    }
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

    let add_extensions =
        move |arguments: wgpu::hal::vulkan::CreateDeviceCallbackArgs<'_, '_, '_>| {
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
    // the callback only adds extensions the physical device supports, as checked above.
    unsafe {
        hal_adapter.open_with_callback(
            descriptor.required_features,
            &descriptor.required_limits,
            &descriptor.memory_hints,
            Some(Box::new(add_extensions)),
        )
    }
    .map_err(|error| VulkanDeviceError::DeviceCreation(error.to_string()))
}
