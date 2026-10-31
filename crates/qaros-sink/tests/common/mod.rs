//! Raw Vulkan helpers for the Vulkan sink tests: the viewer's device, sender-like images, and
//! a consumer that takes released images over the way QAROS's encoder would.

#![allow(dead_code)]

use frame_sink::GpuFrameRequest;
use vulkan_device::{PhysicalDeviceSelector, VulkanApi, VulkanDevice, VulkanDeviceConfig, vk};

const ONE_SECOND_IN_NANOSECONDS: u64 = 1_000_000_000;

/// The viewer's device on wgpu's first Vulkan adapter (with QAROS's Windows extensions there), or `None` (with a note) where no Vulkan
/// loader is on the library path. On a Mac: `DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib`
/// with the toolchain's own `cargo`, since Homebrew's drops `DYLD_*` variables.
pub fn create_device() -> Option<VulkanDevice> {
    let config = VulkanDeviceConfig {
        instance_descriptor: wgpu::InstanceDescriptor::new_without_display_handle(),
        picker: PhysicalDeviceSelector::Default.into_picker(),
        // The sink refuses a device without what QAROS needs; on Windows that is the Win32 set.
        extra_extensions: if cfg!(target_os = "windows") {
            vulkan_device::QAROS_WINDOWS_EXTENSIONS.to_vec()
        } else {
            Vec::new()
        },
        device_descriptor: Box::new(|adapter| wgpu::DeviceDescriptor {
            label: Some("vulkan sink test"),
            required_limits: adapter.limits(),
            ..Default::default()
        }),
    };
    match pollster::block_on(VulkanDevice::create(config)) {
        Ok(device) => Some(device),
        Err(vulkan_device::VulkanDeviceError::NoVulkanInstance) => {
            eprintln!("skipped: no Vulkan loader on the library path");
            None
        }
        Err(error) => panic!("creating the Vulkan device: {error}"),
    }
}

/// An image a consumer copies out, with its aspect and size.
#[derive(Clone, Copy, Debug)]
pub struct ConsumedImage {
    pub image: vk::Image,
    pub aspect: vk::ImageAspectFlags,
    pub resolution_in_pixel: [u32; 2],
}

/// The raw side of the viewer's device: ash device, wgpu's queue and memory types.
pub struct Raw {
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family_index: u32,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
}

impl Raw {
    pub fn of(device: &wgpu::Device) -> Self {
        let hal_device = unsafe { device.as_hal::<VulkanApi>() }.unwrap();
        let memory_properties = unsafe {
            hal_device
                .shared_instance()
                .raw_instance()
                .get_physical_device_memory_properties(hal_device.raw_physical_device())
        };
        Self {
            device: hal_device.raw_device().clone(),
            queue: hal_device.raw_queue(),
            queue_family_index: hal_device.queue_family_index(),
            memory_properties,
        }
    }

    fn memory_type(&self, type_bits: u32, properties: vk::MemoryPropertyFlags) -> u32 {
        (0..self.memory_properties.memory_type_count)
            .find(|&index| {
                type_bits & (1 << index) != 0
                    && self.memory_properties.memory_types[index as usize]
                        .property_flags
                        .contains(properties)
            })
            .unwrap()
    }

    /// A 2D image as a render sender would own it, with dedicated device-local memory.
    pub fn image(
        &self,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
        resolution_in_pixel: [u32; 2],
    ) -> (vk::Image, vk::DeviceMemory) {
        let [width, height] = resolution_in_pixel;
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage | vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { self.device.create_image(&info, None) }.unwrap();
        let requirements = unsafe { self.device.get_image_memory_requirements(image) };
        let allocation = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(self.memory_type(
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ));
        let memory = unsafe { self.device.allocate_memory(&allocation, None) }.unwrap();
        unsafe { self.device.bind_image_memory(image, memory, 0) }.unwrap();
        (image, memory)
    }

    /// Waits on `semaphore` (or on nothing), then, for each image, acquires it from the
    /// external queue family in `TRANSFER_SRC_OPTIMAL`, copies it into host memory and releases
    /// it back to the external family. Returns the bytes of each image, four per pixel.
    #[allow(clippy::too_many_lines)]
    pub fn consume(
        &self,
        semaphore: Option<vk::Semaphore>,
        images: &[ConsumedImage],
    ) -> Vec<Vec<u8>> {
        let device = &self.device;
        let sizes: Vec<u64> = images
            .iter()
            .map(|consumed| {
                let [width, height] = consumed.resolution_in_pixel;
                u64::from(width * height * 4)
            })
            .collect();
        let total_size: u64 = sizes.iter().sum();
        let buffer_info = vk::BufferCreateInfo::default()
            .size(total_size)
            .usage(vk::BufferUsageFlags::TRANSFER_DST);
        let buffer = unsafe { device.create_buffer(&buffer_info, None) }.unwrap();
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let memory = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(self.memory_type(
                        requirements.memory_type_bits,
                        vk::MemoryPropertyFlags::HOST_VISIBLE
                            | vk::MemoryPropertyFlags::HOST_COHERENT,
                    )),
                None,
            )
        }
        .unwrap();
        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.unwrap();
        let pool = unsafe {
            device.create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(self.queue_family_index),
                None,
            )
        }
        .unwrap();
        let command_buffer = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .command_buffer_count(1),
            )
        }
        .unwrap()[0];
        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }.unwrap();

        let acquires: Vec<_> = images
            .iter()
            .map(|consumed| {
                vk::ImageMemoryBarrier::default()
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
                    .dst_queue_family_index(self.queue_family_index)
                    .image(consumed.image)
                    .subresource_range(subresource_range(consumed.aspect))
            })
            .collect();
        let give_backs: Vec<_> = acquires
            .iter()
            .map(|acquire| {
                acquire
                    .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                    .dst_access_mask(vk::AccessFlags::empty())
                    .src_queue_family_index(self.queue_family_index)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
            })
            .collect();
        unsafe {
            device
                .begin_command_buffer(command_buffer, &vk::CommandBufferBeginInfo::default())
                .unwrap();
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &acquires,
            );
            let mut offset = 0;
            for (consumed, size) in images.iter().zip(&sizes) {
                let [width, height] = consumed.resolution_in_pixel;
                let region = vk::BufferImageCopy::default()
                    .buffer_offset(offset)
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: consumed.aspect,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    });
                device.cmd_copy_image_to_buffer(
                    command_buffer,
                    consumed.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer,
                    &[region],
                );
                offset += size;
            }
            device.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &give_backs,
            );
            device.end_command_buffer(command_buffer).unwrap();
            let waits: Vec<vk::Semaphore> = semaphore.into_iter().collect();
            let stages = vec![vk::PipelineStageFlags::TOP_OF_PIPE; waits.len()];
            let command_buffers = [command_buffer];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&command_buffers);
            device.queue_submit(self.queue, &[submit], fence).unwrap();
            device
                .wait_for_fences(&[fence], true, ONE_SECOND_IN_NANOSECONDS)
                .unwrap();
            let mapped = device
                .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                .unwrap();
            let bytes = std::slice::from_raw_parts(
                mapped.cast::<u8>(),
                usize::try_from(total_size).unwrap(),
            );
            let mut copies = Vec::with_capacity(images.len());
            let mut start = 0;
            for size in &sizes {
                let end = start + usize::try_from(*size).unwrap();
                copies.push(bytes[start..end].to_vec());
                start = end;
            }
            device.unmap_memory(memory);
            device.destroy_fence(fence, None);
            device.destroy_command_pool(pool, None);
            device.destroy_buffer(buffer, None);
            device.free_memory(memory, None);
            copies
        }
    }

    /// Waits until `semaphore` has been signalled, with an empty batch.
    pub fn wait_signalled(&self, semaphore: vk::Semaphore) {
        let fence = unsafe {
            self.device
                .create_fence(&vk::FenceCreateInfo::default(), None)
        }
        .unwrap();
        let waits = [semaphore];
        let stages = [vk::PipelineStageFlags::TOP_OF_PIPE];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages);
        unsafe {
            self.device
                .queue_submit(self.queue, &[submit], fence)
                .unwrap();
            self.device
                .wait_for_fences(&[fence], true, ONE_SECOND_IN_NANOSECONDS)
                .unwrap();
            self.device.destroy_fence(fence, None);
        }
    }
}

fn subresource_range(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// What the renderer does in these tests: clears each eye's color target, and its depth
/// target where there is one, in passes of their own since the two may differ in size.
pub fn render_eyes(
    device: &wgpu::Device,
    request: &GpuFrameRequest,
    colors: [wgpu::Color; 2],
    depth: f32,
) -> wgpu::CommandBuffer {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    for (eye, color) in request.eyes.iter().zip(colors) {
        let color_view = eye
            .color
            .create_view(&wgpu::TextureViewDescriptor::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("eye color"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        if let Some(depth_target) = &eye.depth {
            let depth_view = depth_target.create_view(&wgpu::TextureViewDescriptor::default());
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("eye depth"),
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(depth),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
        }
    }
    encoder.finish()
}

/// `[r, g, b, a]` bytes as the clear color that produces them in an UNORM target.
pub fn color_of(bytes: [u8; 4]) -> wgpu::Color {
    let [red, green, blue, alpha] = bytes.map(|channel| f64::from(channel) / 255.0);
    wgpu::Color {
        r: red,
        g: green,
        b: blue,
        a: alpha,
    }
}
