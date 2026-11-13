//! A `VkImage` created with ash on the device, wrapped as a wgpu texture, rendered into and
//! read back.

#![allow(clippy::expect_used, clippy::unwrap_used, unsafe_code)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ash::vk;
use vulkan_device::{VulkanApi, texture_from_vk_image};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.2,
    g: 0.4,
    b: 0.6,
    a: 1.0,
};

/// An image and its memory as another API on the same device would own them.
#[derive(Clone, Copy)]
struct ForeignImage {
    image: vk::Image,
    memory: vk::DeviceMemory,
}

fn create_foreign_image(device: &wgpu::Device, ash_device: &ash::Device) -> ForeignImage {
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
        .usage(
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { ash_device.create_image(&image_info, None) }.unwrap();
    let requirements = unsafe { ash_device.get_image_memory_requirements(image) };

    let hal_device = unsafe { device.as_hal::<VulkanApi>() }.unwrap();
    let memory_properties = unsafe {
        hal_device
            .shared_instance()
            .raw_instance()
            .get_physical_device_memory_properties(hal_device.raw_physical_device())
    };
    let memory_type_index = (0..memory_properties.memory_type_count)
        .find(|&index| {
            requirements.memory_type_bits & (1 << index) != 0
                && memory_properties.memory_types[index as usize]
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        })
        .unwrap();
    let allocation = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type_index);
    let memory = unsafe { ash_device.allocate_memory(&allocation, None) }.unwrap();
    unsafe { ash_device.bind_image_memory(image, memory, 0) }.unwrap();
    ForeignImage { image, memory }
}

#[test]
fn a_foreign_vk_image_renders_and_reads_back_through_wgpu() {
    let Some(vulkan) = common::default_device() else {
        return;
    };
    let device = &vulkan.device;
    let ash_device = common::ash_device(device);
    let foreign = create_foreign_image(device, &ash_device);

    let released = Arc::new(AtomicBool::new(false));
    let on_drop = {
        let released = Arc::clone(&released);
        let ash_device = ash_device.clone();
        let ForeignImage { image, memory } = foreign;
        Box::new(move || {
            unsafe {
                ash_device.destroy_image(image, None);
                ash_device.free_memory(memory, None);
            }
            released.store(true, Ordering::SeqCst);
        })
    };
    let descriptor = wgpu::TextureDescriptor {
        label: Some("foreign image"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    };
    let texture =
        unsafe { texture_from_vk_image(device, foreign.image, &descriptor, on_drop) }.unwrap();
    assert_eq!(
        unsafe { texture.as_hal::<VulkanApi>().unwrap().raw_handle() },
        foreign.image
    );

    let bytes_per_row = WIDTH * 4;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(bytes_per_row * HEIGHT),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("clear foreign image"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(CLEAR),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(HEIGHT),
            },
        },
        descriptor.size,
    );
    vulkan.queue.submit([encoder.finish()]);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, |result| result.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();

    let pixels = readback.slice(..).get_mapped_range().unwrap().to_vec();
    let expected = [51_u8, 102, 153, 255];
    assert_eq!(pixels.len(), (WIDTH * HEIGHT * 4) as usize);
    for pixel in pixels.chunks_exact(4) {
        assert_eq!(pixel, expected);
    }

    assert!(!released.load(Ordering::SeqCst));
    drop(view);
    drop(texture);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    assert!(
        released.load(Ordering::SeqCst),
        "the drop callback did not run"
    );
}
