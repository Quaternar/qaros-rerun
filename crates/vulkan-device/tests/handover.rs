//! A frame on an image another API owns: `HandoverQueue` acquires the image from the layout
//! and queue family its owner reports, wgpu renders into it, and the release batch moves it to
//! `TRANSFER_SRC_OPTIMAL`, releases it to `VK_QUEUE_FAMILY_EXTERNAL` and signals the owner's
//! semaphore. A raw consumer batch acquires the image after that semaphore, copies it out and
//! releases it back to `VK_QUEUE_FAMILY_EXTERNAL`, as an encoder would. Three frames run on the
//! same image, wrapped again each frame, the way a render sender hands its images out: the
//! first from `UNDEFINED`, the others from the external family.

#![allow(clippy::expect_used, clippy::unwrap_used, unsafe_code)]

mod common;

use ash::vk;
use vulkan_device::{
    HandoverQueue, ImageAcquire, ImageRelease, SemaphoreSignal, VulkanApi, attachment_layout,
    image_aspect, texture_from_vk_image,
};

const WIDTH: u32 = 32;
const HEIGHT: u32 = 16;
const ONE_SECOND_IN_NANOSECONDS: u64 = 1_000_000_000;

struct MemoryTypes(vk::PhysicalDeviceMemoryProperties);

impl MemoryTypes {
    fn of(device: &wgpu::Device) -> Self {
        let hal_device = unsafe { device.as_hal::<VulkanApi>() }.unwrap();
        Self(unsafe {
            hal_device
                .shared_instance()
                .raw_instance()
                .get_physical_device_memory_properties(hal_device.raw_physical_device())
        })
    }

    fn find(&self, type_bits: u32, properties: vk::MemoryPropertyFlags) -> u32 {
        (0..self.0.memory_type_count)
            .find(|&index| {
                type_bits & (1 << index) != 0
                    && self.0.memory_types[index as usize]
                        .property_flags
                        .contains(properties)
            })
            .unwrap()
    }
}

/// A sender-owned image: created outside wgpu, rendered by wgpu, copied out by the consumer.
fn create_sender_image(
    ash_device: &ash::Device,
    memory_types: &MemoryTypes,
) -> (vk::Image, vk::DeviceMemory) {
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width: WIDTH,
            height: HEIGHT,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { ash_device.create_image(&image_info, None) }.unwrap();
    let requirements = unsafe { ash_device.get_image_memory_requirements(image) };
    let allocation = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_types.find(
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        ));
    let memory = unsafe { ash_device.allocate_memory(&allocation, None) }.unwrap();
    unsafe { ash_device.bind_image_memory(image, memory, 0) }.unwrap();
    (image, memory)
}

/// The consumer's side of one frame: waits on the frame semaphore, acquires the image from
/// the external queue family and copies it into a host-visible buffer.
struct Consumer {
    ash_device: ash::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    buffer: vk::Buffer,
    buffer_memory: vk::DeviceMemory,
}

impl Consumer {
    fn new(device: &wgpu::Device, memory_types: &MemoryTypes) -> Self {
        let (ash_device, queue, queue_family_index) = {
            let hal_device = unsafe { device.as_hal::<VulkanApi>() }.unwrap();
            (
                hal_device.raw_device().clone(),
                hal_device.raw_queue(),
                hal_device.queue_family_index(),
            )
        };
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(queue_family_index);
        let command_pool = unsafe { ash_device.create_command_pool(&pool_info, None) }.unwrap();
        let command_buffer = unsafe {
            ash_device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .unwrap()[0];
        let fence =
            unsafe { ash_device.create_fence(&vk::FenceCreateInfo::default(), None) }.unwrap();
        let buffer_info = vk::BufferCreateInfo::default()
            .size(u64::from(WIDTH * HEIGHT * 4))
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let buffer = unsafe { ash_device.create_buffer(&buffer_info, None) }.unwrap();
        let requirements = unsafe { ash_device.get_buffer_memory_requirements(buffer) };
        let allocation = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_types.find(
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            ));
        let buffer_memory = unsafe { ash_device.allocate_memory(&allocation, None) }.unwrap();
        unsafe { ash_device.bind_buffer_memory(buffer, buffer_memory, 0) }.unwrap();
        Self {
            ash_device,
            queue,
            queue_family_index,
            command_pool,
            command_buffer,
            fence,
            buffer,
            buffer_memory,
        }
    }

    fn copy_out(&self, image: vk::Image, frame_ready: vk::Semaphore) -> Vec<u8> {
        let device = &self.ash_device;
        let acquire = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL)
            .dst_queue_family_index(self.queue_family_index)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        let region = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: WIDTH,
                height: HEIGHT,
                depth: 1,
            });
        unsafe {
            device
                .reset_command_buffer(self.command_buffer, vk::CommandBufferResetFlags::empty())
                .unwrap();
            device
                .begin_command_buffer(self.command_buffer, &vk::CommandBufferBeginInfo::default())
                .unwrap();
            device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[acquire],
            );
            device.cmd_copy_image_to_buffer(
                self.command_buffer,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.buffer,
                &[region],
            );
            let give_back = acquire
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::empty())
                .src_queue_family_index(self.queue_family_index)
                .dst_queue_family_index(vk::QUEUE_FAMILY_EXTERNAL);
            device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[give_back],
            );
            device.end_command_buffer(self.command_buffer).unwrap();

            let wait_semaphores = [frame_ready];
            let wait_stages = [vk::PipelineStageFlags::TOP_OF_PIPE];
            let command_buffers = [self.command_buffer];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&wait_semaphores)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&command_buffers);
            device
                .queue_submit(self.queue, &[submit], self.fence)
                .unwrap();
            device
                .wait_for_fences(&[self.fence], true, ONE_SECOND_IN_NANOSECONDS)
                .unwrap();
            device.reset_fences(&[self.fence]).unwrap();

            let size = usize::try_from(WIDTH * HEIGHT * 4).unwrap();
            let mapped = device
                .map_memory(
                    self.buffer_memory,
                    0,
                    vk::WHOLE_SIZE,
                    vk::MemoryMapFlags::empty(),
                )
                .unwrap();
            let pixels = std::slice::from_raw_parts(mapped.cast::<u8>(), size).to_vec();
            device.unmap_memory(self.buffer_memory);
            pixels
        }
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        unsafe {
            self.ash_device.destroy_buffer(self.buffer, None);
            self.ash_device.free_memory(self.buffer_memory, None);
            self.ash_device.destroy_fence(self.fence, None);
            self.ash_device
                .destroy_command_pool(self.command_pool, None);
        }
    }
}

fn wrap(device: &wgpu::Device, image: vk::Image) -> wgpu::Texture {
    let descriptor = wgpu::TextureDescriptor {
        label: Some("sender image"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    };
    unsafe { texture_from_vk_image(device, image, &descriptor, Box::new(|| {})) }.unwrap()
}

fn clear_commands(
    device: &wgpu::Device,
    texture: &wgpu::Texture,
    color: wgpu::Color,
) -> wgpu::CommandBuffer {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("render into the sender image"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(color),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    encoder.finish()
}

#[test]
fn released_images_reach_the_consumer_after_the_frame_semaphore() {
    let Some(vulkan) = common::default_device() else {
        return;
    };
    let device = &vulkan.device;
    let ash_device = common::ash_device(device);
    let memory_types = MemoryTypes::of(device);
    let (image, image_memory) = create_sender_image(&ash_device, &memory_types);
    let consumer = Consumer::new(device, &memory_types);
    let frame_ready =
        unsafe { ash_device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.unwrap();
    let mut handover_queue = HandoverQueue::new(device, 2).unwrap();
    assert_eq!(
        handover_queue.queue_family_index(),
        vulkan.handles.queue_family_index
    );
    let mut owner_state = (vk::ImageLayout::UNDEFINED, vk::QUEUE_FAMILY_IGNORED);

    let frames = [
        (
            wgpu::Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            [255_u8, 0, 0, 255],
        ),
        (
            wgpu::Color {
                r: 0.0,
                g: 0.2,
                b: 1.0,
                a: 0.6,
            },
            [0, 51, 255, 153],
        ),
        (
            wgpu::Color {
                r: 0.0,
                g: 1.0,
                b: 0.0,
                a: 1.0,
            },
            [0, 255, 0, 255],
        ),
    ];
    for (clear_color, expected) in frames {
        let texture = wrap(device, image);
        let commands = clear_commands(device, &texture, clear_color);
        let format = texture.format();
        let (owner_layout, owner_family) = owner_state;
        unsafe {
            handover_queue.submit_handover(
                &vulkan.queue,
                &[ImageAcquire {
                    image,
                    aspect: image_aspect(format),
                    old_layout: owner_layout,
                    new_layout: attachment_layout(format),
                    source_queue_family: owner_family,
                }],
                vec![commands],
                &[ImageRelease {
                    image,
                    aspect: image_aspect(format),
                    old_layout: attachment_layout(format),
                    new_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    destination_queue_family: vk::QUEUE_FAMILY_EXTERNAL,
                }],
                &[SemaphoreSignal {
                    semaphore: frame_ready,
                    value: None,
                }],
            )
        }
        .unwrap();
        drop(texture);

        let pixels = consumer.copy_out(image, frame_ready);
        assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);
        for pixel in pixels.chunks_exact(4) {
            assert_eq!(pixel, expected);
        }
        owner_state = (
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::QUEUE_FAMILY_EXTERNAL,
        );
    }

    drop(handover_queue);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    unsafe {
        ash_device.device_wait_idle().unwrap();
        ash_device.destroy_semaphore(frame_ready, None);
        ash_device.destroy_image(image, None);
        ash_device.free_memory(image_memory, None);
    }
}
