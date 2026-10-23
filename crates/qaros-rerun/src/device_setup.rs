//! The viewer's wgpu setup: Rerun's own, Rerun's with the backend forced, or a Vulkan device
//! the viewer creates itself (`--vulkan-device`, or the QAROS Vulkan render sender) and hands
//! to egui as an existing device.
//!
//! For the QAROS Vulkan render sender the device is created after onboarding, on the adapter
//! the session reports (header `b1338df`, "Basic flow", step 4), unless `--vulkan-device`
//! names one first; see `qaros_setup`.
//!
//! The backend is forced through the instance descriptor rather than Rerun's
//! `force_wgpu_backend`, which refuses `dx12` (`re_renderer/src/device_caps.rs`,
//! `validate_graphics_backend_applicability`).

use std::ffi::{CStr, CString};
use std::sync::Arc;

use vulkan_device::{
    PhysicalDeviceSelector, VulkanDevice, VulkanDeviceConfig, WINDOWS_INTEROP_EXTENSIONS,
};

use crate::memory_knobs::{MemoryKnobs, memory_hints_with_blocks_of};
use crate::options::{GraphicsBackend, Options};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphicsChoice {
    pub backend: Option<GraphicsBackend>,
    /// `--vulkan-device`: the viewer creates its Vulkan device on this physical device before
    /// anything else.
    pub vulkan_device: Option<PhysicalDeviceSelector>,
    pub vulkan_extensions: Vec<String>,
    /// The device is for QAROS's Vulkan render sender: its device extensions are added, and
    /// without `--vulkan-device` it is created once the session names its adapter.
    pub for_qaros: bool,
    /// `QAROS_RERUN_MEMORY_BLOCK_MIB`: gpu-allocator's device block size instead of wgpu's
    /// `MemoryHints::Performance`.
    pub memory_block_mebibytes: Option<u64>,
}

impl GraphicsChoice {
    pub fn from_options(options: &Options, memory_knobs: &MemoryKnobs) -> Self {
        Self {
            backend: options.graphics_backend,
            vulkan_device: options.vulkan_device.clone(),
            vulkan_extensions: options.vulkan_extensions.clone(),
            for_qaros: options.qaros_vulkan(),
            memory_block_mebibytes: memory_knobs.memory_block_mebibytes,
        }
    }

    /// True when Rerun's own setup applies unchanged.
    pub fn is_rerun_default(&self) -> bool {
        self.backend.is_none()
            && self.vulkan_device.is_none()
            && !self.for_qaros
            && self.memory_block_mebibytes.is_none()
    }

    /// The `--vulkan-device` device, created before the sink.
    pub async fn create_named_vulkan_device(&self) -> Result<Option<VulkanDevice>, String> {
        match &self.vulkan_device {
            Some(selector) => self.create_vulkan_device(selector).await.map(Some),
            None => Ok(None),
        }
    }

    /// The viewer's own Vulkan device on the physical device `selector` picks.
    pub async fn create_vulkan_device(
        &self,
        selector: &PhysicalDeviceSelector,
    ) -> Result<VulkanDevice, String> {
        let mut extra_extensions = extension_names(&self.vulkan_extensions)?;
        if self.for_qaros {
            for extension in qaros_device_extensions() {
                if !extra_extensions.contains(extension) {
                    extra_extensions.push(extension);
                }
            }
        }
        let config = VulkanDeviceConfig {
            instance_descriptor: re_renderer::device_caps::instance_descriptor(None),
            picker: selector.clone().into_picker(),
            extra_extensions,
            device_descriptor: Box::new(device_descriptor_with_blocks_of(
                self.memory_block_mebibytes,
            )),
        };
        let vulkan = VulkanDevice::create(config)
            .await
            .map_err(|error| error.to_string())?;
        re_log::info!(
            "vulkan-device: viewer device created here on {}; handles {:?}",
            vulkan.physical_device,
            vulkan.handles
        );
        Ok(vulkan)
    }

    /// egui's setup: the viewer's own device when there is one, else a new one.
    pub fn wgpu_setup(&self, vulkan: Option<&VulkanDevice>) -> egui_wgpu::WgpuSetup {
        if let Some(vulkan) = vulkan {
            return egui_wgpu::WgpuSetup::Existing(egui_wgpu::WgpuSetupExisting {
                instance: vulkan.instance.clone(),
                adapter: vulkan.adapter.clone(),
                device: vulkan.device.clone(),
                queue: vulkan.queue.clone(),
            });
        }
        let mut create_new = rerun_create_new();
        create_new.device_descriptor = Arc::new(device_descriptor_with_blocks_of(
            self.memory_block_mebibytes,
        ));
        if let Some(backend) = self.backend {
            create_new.instance_descriptor.backends = backend.backends();
        }
        egui_wgpu::WgpuSetup::CreateNew(create_new)
    }
}

/// The device extensions QAROS's Vulkan render sender needs: the Win32 external memory and
/// semaphore pair on Windows, none elsewhere (the `qaros` crate's `vulkan::required_device_extensions`).
fn qaros_device_extensions() -> &'static [&'static CStr] {
    if cfg!(target_os = "windows") {
        vulkan_device::QAROS_WINDOWS_EXTENSIONS
    } else {
        &[]
    }
}

/// Rerun's own setup (`re_viewer::wgpu_options`, which is crate-private).
fn rerun_create_new() -> egui_wgpu::WgpuSetupCreateNew {
    let instance_descriptor = re_renderer::device_caps::instance_descriptor(None);
    let backends = instance_descriptor.backends;
    egui_wgpu::WgpuSetupCreateNew {
        instance_descriptor,
        native_adapter_selector: Some(Arc::new(move |adapters, surface| {
            re_renderer::device_caps::select_adapter(adapters, backends, surface)
        })),
        device_descriptor: Arc::new(rerun_device_descriptor),
        ..egui_wgpu::WgpuSetupCreateNew::without_display_handle()
    }
}

fn rerun_device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    re_renderer::device_caps::DeviceCaps::from_adapter_without_validation(adapter)
        .device_descriptor()
}

/// Rerun's device descriptor, with gpu-allocator's device blocks fixed when a size is given.
fn device_descriptor_with_blocks_of(
    memory_block_mebibytes: Option<u64>,
) -> impl Fn(&wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> + Send + Sync + 'static {
    move |adapter| {
        let mut descriptor = rerun_device_descriptor(adapter);
        if let Some(mebibytes) = memory_block_mebibytes {
            descriptor.memory_hints = memory_hints_with_blocks_of(mebibytes);
        }
        descriptor
    }
}

/// `windows-interop` expands to the QAROS Windows set; other names are taken as given and live
/// for the rest of the process, as wgpu-hal requires of device extension names.
fn extension_names(names: &[String]) -> Result<Vec<&'static CStr>, String> {
    let mut extensions = Vec::new();
    for name in names {
        if name == "windows-interop" {
            extensions.extend_from_slice(WINDOWS_INTEROP_EXTENSIONS);
            continue;
        }
        let owned = CString::new(name.as_str())
            .map_err(|_| format!("--vulkan-extension {name:?} contains a NUL byte"))?;
        extensions.push(&*Box::leak(owned.into_boxed_c_str()));
    }
    Ok(extensions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_names_expand_the_windows_set_and_keep_the_rest() {
        let names = extension_names(&[
            "VK_EXT_metal_objects".to_owned(),
            "windows-interop".to_owned(),
        ])
        .unwrap();
        let names: Vec<_> = names
            .iter()
            .map(|name| name.to_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "VK_EXT_metal_objects",
                "VK_KHR_external_memory_win32",
                "VK_KHR_external_semaphore_win32",
                "VK_KHR_win32_keyed_mutex",
                "VK_KHR_timeline_semaphore",
            ]
        );
        assert!(extension_names(&["VK_\0bad".to_owned()]).is_err());
    }

    #[test]
    fn rerun_default_only_without_backend_or_device() {
        let mut choice = GraphicsChoice {
            backend: None,
            vulkan_device: None,
            vulkan_extensions: Vec::new(),
            for_qaros: false,
            memory_block_mebibytes: None,
        };
        assert!(choice.is_rerun_default());
        choice.memory_block_mebibytes = Some(256);
        assert!(
            !choice.is_rerun_default(),
            "a fixed block size needs the viewer's own device descriptor"
        );
        choice.memory_block_mebibytes = None;
        choice.backend = Some(GraphicsBackend::Vulkan);
        assert!(!choice.is_rerun_default());
        choice.backend = None;
        choice.vulkan_device = Some(PhysicalDeviceSelector::Default);
        assert!(!choice.is_rerun_default());
        choice.vulkan_device = None;
        choice.for_qaros = true;
        assert!(
            !choice.is_rerun_default(),
            "the QAROS Vulkan sender gets a device created after onboarding"
        );
    }
}
