//! The app-created device: picker, extra extensions, raw handles.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use ash::vk;
use vulkan_device::{
    MemoryHeapBudget, MemoryHeapBudgets, PhysicalDeviceSelector, VulkanApi, VulkanDeviceError,
    VulkanDeviceHandles, describe_physical_devices, memory_heap_budgets,
};

#[test]
fn the_picker_sees_the_instance_and_its_choice_becomes_the_device() {
    let seen_by_picker = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&seen_by_picker);
    let picker = Box::new(move |instance: &ash::Instance| {
        let devices = describe_physical_devices(instance).map_err(|error| error.to_string())?;
        let chosen = devices.last().ok_or("no physical devices")?.clone();
        *seen.lock().unwrap() = Some((instance.handle(), chosen.clone()));
        Ok(Some(chosen.handle))
    });
    let Some(device) = common::create_or_skip(common::config(picker, common::EXTRA_EXTENSIONS))
    else {
        return;
    };

    let (instance_seen, chosen) = seen_by_picker.lock().unwrap().clone().unwrap();
    let handles = &device.handles;
    assert_eq!(handles.instance, instance_seen);
    assert_eq!(handles.physical_device, chosen.handle);
    assert_eq!(device.physical_device, chosen);
    assert_ne!(handles.device, vk::Device::null());
    assert_ne!(handles.queue, vk::Queue::null());
    assert_eq!(device.adapter.get_info().backend, wgpu::Backend::Vulkan);
    assert_eq!(device.adapter.get_info().name, chosen.name);
    for extension in common::EXTRA_EXTENSIONS {
        assert!(
            handles.has_device_extension(extension),
            "{extension:?} not enabled: {:?}",
            handles.enabled_device_extensions
        );
    }
    eprintln!("device: {}\nhandles: {handles:#?}", device.physical_device);
}

#[test]
fn handles_read_from_the_wgpu_device_match_the_ones_from_creation() {
    let Some(device) = common::default_device() else {
        return;
    };
    let handles = VulkanDeviceHandles::of(&device.device).unwrap();
    assert_eq!(handles.device, device.handles.device);
    assert_eq!(handles.queue, device.handles.queue);
    assert_eq!(
        handles.queue_family_index,
        device.handles.queue_family_index
    );
    assert_eq!(handles.queue_index, device.handles.queue_index);
    // wgpu-hal 30 always opens queue 0 of family 0.
    assert_eq!((handles.queue_family_index, handles.queue_index), (0, 0));
}

#[test]
fn a_selector_by_name_picks_that_device() {
    let Some(probe) = common::default_device() else {
        return;
    };
    let name = probe.physical_device.name.clone();
    drop(probe);
    let device = common::create_or_skip(common::config(
        PhysicalDeviceSelector::NameContains(name.to_uppercase()).into_picker(),
        &[],
    ))
    .unwrap();
    assert_eq!(device.physical_device.name, name);
}

#[test]
fn an_unsupported_extension_is_refused_before_device_creation() {
    let result = pollster::block_on(vulkan_device::VulkanDevice::create(common::config(
        PhysicalDeviceSelector::Default.into_picker(),
        &[c"VK_QAROS_rehearsal_extension_that_does_not_exist"],
    )));
    match result {
        Err(VulkanDeviceError::UnsupportedExtensions { missing, .. }) => {
            assert_eq!(
                missing,
                ["VK_QAROS_rehearsal_extension_that_does_not_exist"]
            );
        }
        Err(VulkanDeviceError::NoVulkanInstance) => eprintln!("skipped: no Vulkan loader"),
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("a device with a made-up extension was created"),
    }
}

#[test]
fn a_picker_error_and_an_unmatched_selector_stop_device_creation() {
    let result = pollster::block_on(vulkan_device::VulkanDevice::create(common::config(
        PhysicalDeviceSelector::Luid([0xab; 8]).into_picker(),
        &[],
    )));
    match result {
        Err(VulkanDeviceError::Picker(message)) => {
            assert!(message.contains("no physical device matches"), "{message}");
        }
        Err(VulkanDeviceError::NoVulkanInstance) => eprintln!("skipped: no Vulkan loader"),
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("a made-up LUID picked a device"),
    }
}

#[test]
fn the_memory_budget_is_enabled_and_read_whenever_the_physical_device_has_it() {
    let Some(device) = common::default_device() else {
        return;
    };
    #[allow(unsafe_code)]
    // SAFETY: only the adapter's capabilities are read.
    let is_supported = unsafe { device.adapter.as_hal::<VulkanApi>() }
        .unwrap()
        .physical_device_capabilities()
        .supports_extension(ash::ext::memory_budget::NAME);
    assert_eq!(
        device
            .handles
            .has_device_extension(ash::ext::memory_budget::NAME),
        is_supported
    );

    match memory_heap_budgets(&device.device) {
        Ok(heaps) => {
            assert!(is_supported);
            assert!(!heaps.is_empty());
            assert!(
                heaps.iter().any(MemoryHeapBudget::is_device_local),
                "{heaps:?}"
            );
            for heap in &heaps {
                assert!(heap.size > 0, "{heap:?}");
                assert!(
                    heap.budget > 0 && heap.budget <= heap.size,
                    "the budget is at most the heap size: {heap:?}"
                );
            }
            eprintln!("vulkan memory: {}", MemoryHeapBudgets(&heaps));
        }
        Err(reason) => {
            assert!(!is_supported, "{reason}");
            assert_eq!(reason, "VK_EXT_memory_budget is not enabled on the device");
            eprintln!("vulkan memory: budget unavailable: {reason}");
        }
    }
}
