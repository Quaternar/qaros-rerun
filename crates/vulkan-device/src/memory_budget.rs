//! Per-heap memory budget and usage from `VK_EXT_memory_budget`: what the OS video memory
//! manager grants this process (on Windows, WDDM's per-process budget) against what it uses.

use std::fmt;

use ash::vk;

use crate::VulkanApi;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryHeapBudget {
    pub size: vk::DeviceSize,
    pub flags: vk::MemoryHeapFlags,
    pub budget: vk::DeviceSize,
    pub usage: vk::DeviceSize,
}

impl MemoryHeapBudget {
    pub fn is_device_local(&self) -> bool {
        self.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL)
    }
}

/// One compact line for every heap, e.g.
/// `heap0 device-local 8.00 GiB budget 7.10 GiB usage 0.72 GiB | heap1 host 15.80 GiB ...`.
pub struct MemoryHeapBudgets<'heaps>(pub &'heaps [MemoryHeapBudget]);

impl fmt::Display for MemoryHeapBudgets<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, heap) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(" | ")?;
            }
            let kind = if heap.is_device_local() {
                "device-local"
            } else {
                "host"
            };
            write!(
                formatter,
                "heap{index} {kind} {} budget {} usage {}",
                Gibibytes(heap.size),
                Gibibytes(heap.budget),
                Gibibytes(heap.usage)
            )?;
        }
        Ok(())
    }
}

struct Gibibytes(vk::DeviceSize);

impl fmt::Display for Gibibytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const GIBIBYTE: f64 = 1024.0 * 1024.0 * 1024.0;
        #[allow(clippy::cast_precision_loss)]
        let gibibytes = self.0 as f64 / GIBIBYTE;
        write!(formatter, "{gibibytes:.2} GiB")
    }
}

/// The heaps of `device`'s physical device with their budget and usage now. An error when the
/// device is not on Vulkan or `VK_EXT_memory_budget` is not enabled on it.
pub fn memory_heap_budgets(device: &wgpu::Device) -> Result<Vec<MemoryHeapBudget>, String> {
    #[allow(unsafe_code)]
    // SAFETY: only the instance and physical device are read for a property query; nothing is
    // destroyed and the borrow ends with this function, while `device` lives.
    let hal_device = unsafe { device.as_hal::<VulkanApi>() }
        .ok_or_else(|| "the device is not on Vulkan".to_owned())?;
    if !hal_device
        .enabled_device_extensions()
        .contains(&ash::ext::memory_budget::NAME)
    {
        return Err("VK_EXT_memory_budget is not enabled on the device".to_owned());
    }
    let instance = hal_device.shared_instance();
    let physical_device = hal_device.raw_physical_device();

    let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
    let mut properties = vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
    if instance.instance_api_version() >= vk::API_VERSION_1_1 {
        #[allow(unsafe_code)]
        // SAFETY: `physical_device` belongs to this instance, the instance is Vulkan 1.1 or
        // later, and the chained budget structure's extension is enabled on the device.
        unsafe {
            instance
                .raw_instance()
                .get_physical_device_memory_properties2(physical_device, &mut properties);
        }
    } else if instance
        .extensions()
        .contains(&ash::khr::get_physical_device_properties2::NAME)
    {
        let properties2 = ash::khr::get_physical_device_properties2::Instance::new(
            instance.entry(),
            instance.raw_instance(),
        );
        #[allow(unsafe_code)]
        // SAFETY: as above, through the instance extension the 1.0 instance has enabled.
        unsafe {
            properties2.get_physical_device_memory_properties2(physical_device, &mut properties);
        }
    } else {
        return Err(
            "the instance has neither Vulkan 1.1 nor VK_KHR_get_physical_device_properties2"
                .to_owned(),
        );
    }

    let memory_properties = properties.memory_properties;
    let heap_count = usize::try_from(memory_properties.memory_heap_count)
        .map_err(|error| error.to_string())?
        .min(vk::MAX_MEMORY_HEAPS);
    Ok((0..heap_count)
        .map(|index| MemoryHeapBudget {
            size: memory_properties.memory_heaps[index].size,
            flags: memory_properties.memory_heaps[index].flags,
            budget: budget.heap_budget[index],
            usage: budget.heap_usage[index],
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIBIBYTE: vk::DeviceSize = 1024 * 1024 * 1024;
    const MEBIBYTE: vk::DeviceSize = 1024 * 1024;

    #[test]
    fn every_heap_on_one_line_with_kind_size_budget_and_usage() {
        let heaps = [
            MemoryHeapBudget {
                size: 8 * GIBIBYTE,
                flags: vk::MemoryHeapFlags::DEVICE_LOCAL,
                budget: 7 * GIBIBYTE + 102 * MEBIBYTE,
                usage: 720 * MEBIBYTE,
            },
            MemoryHeapBudget {
                size: 16 * GIBIBYTE,
                flags: vk::MemoryHeapFlags::empty(),
                budget: 12 * GIBIBYTE,
                usage: 0,
            },
        ];
        assert_eq!(
            MemoryHeapBudgets(&heaps).to_string(),
            "heap0 device-local 8.00 GiB budget 7.10 GiB usage 0.70 GiB | \
             heap1 host 16.00 GiB budget 12.00 GiB usage 0.00 GiB"
        );
    }

    #[test]
    fn no_heaps_is_an_empty_line() {
        assert_eq!(MemoryHeapBudgets(&[]).to_string(), "");
    }
}
