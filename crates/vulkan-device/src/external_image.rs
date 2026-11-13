use ash::vk;

use crate::VulkanApi;

/// Wraps a `VkImage` created outside wgpu, on the same `VkDevice`, as a `wgpu::Texture`.
///
/// wgpu never owns the image or its memory: `on_drop` runs once wgpu has dropped the texture
/// and finished with it, and is where the owner may release them. wgpu assumes the image
/// starts in `VK_IMAGE_LAYOUT_UNDEFINED`, so its first use discards earlier content, and from
/// then on tracks its layout itself; whoever else touches the image between wgpu submissions
/// must return it to the layout wgpu left it in, or the image must be wrapped again.
///
/// Returns `None` when `device` is not on the Vulkan backend.
///
/// # Safety
///
/// - `image` was created on `device`'s `VkDevice` and matches `descriptor`: format, extent,
///   mip levels, sample count, dimension, and usage flags covering `descriptor.usage`.
/// - `descriptor.view_formats` is empty unless the image was created `MUTABLE_FORMAT`.
/// - `image` and its bound memory stay valid until `on_drop` is called.
#[allow(unsafe_code)]
pub unsafe fn texture_from_vk_image(
    device: &wgpu::Device,
    image: vk::Image,
    descriptor: &wgpu::TextureDescriptor<'_>,
    on_drop: wgpu::hal::DropCallback,
) -> Option<wgpu::Texture> {
    let hal_descriptor = wgpu::hal::TextureDescriptor {
        label: descriptor.label,
        size: descriptor.size,
        mip_level_count: descriptor.mip_level_count,
        sample_count: descriptor.sample_count,
        dimension: descriptor.dimension,
        format: descriptor.format,
        usage: texture_uses(descriptor.usage, descriptor.format),
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: descriptor.view_formats.to_vec(),
    };
    // SAFETY: the caller guarantees the image matches the descriptor and outlives `on_drop`;
    // with a drop callback and external memory, wgpu-hal destroys neither.
    let hal_texture = unsafe {
        device.as_hal::<VulkanApi>()?.texture_from_raw(
            image,
            &hal_descriptor,
            Some(on_drop),
            wgpu::hal::vulkan::TextureMemory::External,
        )
    };
    // SAFETY: the hal texture belongs to this device's backend and matches `descriptor`.
    Some(unsafe {
        device.create_texture_from_hal::<VulkanApi>(
            hal_texture,
            descriptor,
            wgpu::TextureUses::UNINITIALIZED,
        )
    })
}

fn texture_uses(usage: wgpu::TextureUsages, format: wgpu::TextureFormat) -> wgpu::TextureUses {
    let mut uses = wgpu::TextureUses::empty();
    uses.set(
        wgpu::TextureUses::COPY_SRC,
        usage.contains(wgpu::TextureUsages::COPY_SRC),
    );
    uses.set(
        wgpu::TextureUses::COPY_DST,
        usage.contains(wgpu::TextureUsages::COPY_DST),
    );
    uses.set(
        wgpu::TextureUses::RESOURCE,
        usage.contains(wgpu::TextureUsages::TEXTURE_BINDING),
    );
    uses.set(
        wgpu::TextureUses::STORAGE_READ_WRITE,
        usage.contains(wgpu::TextureUsages::STORAGE_BINDING),
    );
    if usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
        uses |= if format.is_depth_stencil_format() {
            wgpu::TextureUses::DEPTH_STENCIL_READ | wgpu::TextureUses::DEPTH_STENCIL_WRITE
        } else {
            wgpu::TextureUses::COLOR_TARGET
        };
    }
    uses
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_texture_usages_to_hal_uses() {
        let color = texture_uses(
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            wgpu::TextureFormat::Bgra8Unorm,
        );
        assert_eq!(
            color,
            wgpu::TextureUses::COLOR_TARGET | wgpu::TextureUses::COPY_SRC
        );
        let depth = texture_uses(
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            wgpu::TextureFormat::Depth32Float,
        );
        assert_eq!(
            depth,
            wgpu::TextureUses::DEPTH_STENCIL_READ
                | wgpu::TextureUses::DEPTH_STENCIL_WRITE
                | wgpu::TextureUses::RESOURCE
        );
    }
}
