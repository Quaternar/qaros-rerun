//! Rendering with wgpu into images another API or process owns between frames. wgpu can record
//! neither a queue family ownership transfer nor a final layout of our choosing, so a frame is
//! three batches on wgpu's queue:
//!
//! 1. a raw acquire batch: each image from the layout and queue family its owner reports to
//!    the attachment layout on wgpu's family;
//! 2. wgpu's submission, which renders;
//! 3. a raw release batch: each image to its final layout, released to the owner's family,
//!    then the owner's semaphores are signalled.
//!
//! A semaphore wait only orders the batch it belongs to, so the batches are chained through
//! binary semaphores of ours: the acquire batch signals one that wgpu's submission waits on,
//! and wgpu's submission signals one the release batch waits on.

use std::fmt;

use ash::vk;

use crate::VulkanApi;
use crate::semaphores::{
    SemaphoreSignal, SemaphoreWait, stage_signal_semaphore, stage_wait_semaphore,
};

/// How long a slot may still be busy from an earlier frame before the handover fails.
const SLOT_WAIT_TIMEOUT_NANOSECONDS: u64 = 2_000_000_000;

/// One image to take over before wgpu renders into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageAcquire {
    pub image: vk::Image,
    pub aspect: vk::ImageAspectFlags,
    /// The layout the owner reports; `UNDEFINED` discards the content.
    pub old_layout: vk::ImageLayout,
    /// [`attachment_layout`] for a render target.
    pub new_layout: vk::ImageLayout,
    /// The queue family that owns the image now. `vk::QUEUE_FAMILY_IGNORED` or wgpu's own
    /// family means no ownership transfer.
    pub source_queue_family: u32,
}

/// One image to hand over after wgpu's last use of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageRelease {
    pub image: vk::Image,
    pub aspect: vk::ImageAspectFlags,
    /// The layout wgpu's last use left the image in, [`attachment_layout`] after a render pass.
    pub old_layout: vk::ImageLayout,
    pub new_layout: vk::ImageLayout,
    /// `vk::QUEUE_FAMILY_EXTERNAL` hands the image to another API or process;
    /// `vk::QUEUE_FAMILY_IGNORED` keeps it on wgpu's queue family and only changes the layout.
    pub destination_queue_family: u32,
}

/// The layout wgpu-hal leaves a texture in after a render pass that wrote it as an attachment:
/// `COLOR_TARGET` and `DEPTH_STENCIL_WRITE` uses (`wgpu-hal-30.0.0/src/vulkan/conv.rs`
/// L240-260, `derive_image_layout`). wgpu does not transition a texture at the end of a
/// submission, so this is still its layout when the release batch runs.
pub fn attachment_layout(format: wgpu::TextureFormat) -> vk::ImageLayout {
    if format.is_depth_stencil_format() {
        vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL
    } else {
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
    }
}

/// The aspects a barrier on an image of this format names.
pub fn image_aspect(format: wgpu::TextureFormat) -> vk::ImageAspectFlags {
    match (format.has_depth_aspect(), format.has_stencil_aspect()) {
        (true, true) => vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
        (true, false) => vk::ImageAspectFlags::DEPTH,
        (false, true) => vk::ImageAspectFlags::STENCIL,
        (false, false) => vk::ImageAspectFlags::COLOR,
    }
}

fn attachment_write_access(aspect: vk::ImageAspectFlags) -> vk::AccessFlags {
    if aspect.contains(vk::ImageAspectFlags::COLOR) {
        vk::AccessFlags::COLOR_ATTACHMENT_WRITE
    } else {
        vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE
    }
}

fn whole_image(aspect: vk::ImageAspectFlags) -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: aspect,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

#[derive(Debug)]
pub enum HandoverError {
    /// The device or queue is not on wgpu's Vulkan backend.
    NotVulkan,
    Vulkan {
        operation: &'static str,
        result: vk::Result,
    },
    /// An earlier release batch failed after wgpu's submission had signalled its chain
    /// semaphore; that semaphore can no longer be signalled safely.
    Broken,
}

impl fmt::Display for HandoverError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotVulkan => write!(formatter, "the wgpu device is not on Vulkan"),
            Self::Vulkan { operation, result } => write!(formatter, "{operation}: {result}"),
            Self::Broken => write!(
                formatter,
                "an earlier release batch failed; the handover queue cannot be used again"
            ),
        }
    }
}

impl std::error::Error for HandoverError {}

fn vulkan_error(operation: &'static str) -> impl Fn(vk::Result) -> HandoverError {
    move |result| HandoverError::Vulkan { operation, result }
}

/// The raw objects of one frame. The release batch's fence covers the acquire batch too: the
/// release batch waits on wgpu's submission, which waits on the acquire batch.
struct HandoverSlot {
    acquire_commands: vk::CommandBuffer,
    release_commands: vk::CommandBuffer,
    fence: vk::Fence,
    /// Signalled by the acquire batch, waited on by wgpu's submission.
    acquired: vk::Semaphore,
    /// Signalled by wgpu's submission, waited on by the release batch.
    rendered: vk::Semaphore,
    submitted: bool,
}

/// Acquire and release batches on wgpu's queue, a few slots deep so a slot is free again by
/// the time it comes round. Use it on the thread that submits to wgpu.
pub struct HandoverQueue {
    /// Keeps the `VkDevice` alive for as long as the raw objects below exist.
    _device: wgpu::Device,
    raw_device: ash::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    command_pool: vk::CommandPool,
    slots: Vec<HandoverSlot>,
    next_slot: usize,
    broken: bool,
}

impl HandoverQueue {
    pub fn new(device: &wgpu::Device, slot_count: usize) -> Result<Self, HandoverError> {
        #[allow(unsafe_code)]
        // SAFETY: only handles are read; the clone of the ash device stays valid while the
        // `wgpu::Device` clone kept in the queue lives.
        let (raw_device, queue, queue_family_index) = {
            let hal_device =
                unsafe { device.as_hal::<VulkanApi>() }.ok_or(HandoverError::NotVulkan)?;
            (
                hal_device.raw_device().clone(),
                hal_device.raw_queue(),
                hal_device.queue_family_index(),
            )
        };
        let mut handover_queue = Self {
            _device: device.clone(),
            raw_device,
            queue,
            queue_family_index,
            command_pool: vk::CommandPool::null(),
            slots: Vec::with_capacity(slot_count),
            next_slot: 0,
            broken: false,
        };
        handover_queue.create_objects(slot_count.max(1))?;
        Ok(handover_queue)
    }

    /// wgpu's queue family: the family images are acquired to and released from.
    pub fn queue_family_index(&self) -> u32 {
        self.queue_family_index
    }

    #[allow(unsafe_code)]
    fn create_objects(&mut self, slot_count: usize) -> Result<(), HandoverError> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(self.queue_family_index);
        // SAFETY: a valid device and create info; the pool is destroyed in `Drop`.
        self.command_pool = unsafe { self.raw_device.create_command_pool(&pool_info, None) }
            .map_err(vulkan_error("vkCreateCommandPool"))?;
        let allocate_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(u32::try_from(slot_count * 2).unwrap_or(u32::MAX));
        // SAFETY: the pool belongs to this device; the buffers go with the pool.
        let command_buffers = unsafe { self.raw_device.allocate_command_buffers(&allocate_info) }
            .map_err(vulkan_error("vkAllocateCommandBuffers"))?;
        for pair in command_buffers.chunks_exact(2) {
            let fence = self.create_fence()?;
            let acquired = self.create_semaphore().inspect_err(|_| {
                // SAFETY: the fence was never submitted.
                unsafe { self.raw_device.destroy_fence(fence, None) };
            })?;
            let rendered = self.create_semaphore().inspect_err(|_| {
                // SAFETY: neither object was ever submitted.
                unsafe {
                    self.raw_device.destroy_fence(fence, None);
                    self.raw_device.destroy_semaphore(acquired, None);
                }
            })?;
            self.slots.push(HandoverSlot {
                acquire_commands: pair[0],
                release_commands: pair[1],
                fence,
                acquired,
                rendered,
                submitted: false,
            });
        }
        Ok(())
    }

    #[allow(unsafe_code)]
    fn create_fence(&self) -> Result<vk::Fence, HandoverError> {
        // SAFETY: plain creation on a valid device.
        unsafe {
            self.raw_device
                .create_fence(&vk::FenceCreateInfo::default(), None)
        }
        .map_err(vulkan_error("vkCreateFence"))
    }

    #[allow(unsafe_code)]
    fn create_semaphore(&self) -> Result<vk::Semaphore, HandoverError> {
        // SAFETY: plain creation on a valid device.
        unsafe {
            self.raw_device
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
        }
        .map_err(vulkan_error("vkCreateSemaphore"))
    }

    /// Submits the acquire batch for `acquires` (skipped when empty), `command_buffers`
    /// through wgpu, then the release batch that moves each image of `releases` to its new
    /// layout and queue family and signals `signals` (and nothing else) once those barriers
    /// have run.
    ///
    /// When recording or the acquire batch fails, nothing is submitted, not even
    /// `command_buffers`. When the release batch fails after wgpu's submission, the queue is
    /// broken for good.
    ///
    /// # Safety
    ///
    /// - `queue` is the queue of the device this handover queue was made from, and no other
    ///   thread submits to it during the call.
    /// - Every image belongs to that device and has `aspect` as its aspects; an acquired image
    ///   is in `old_layout`, owned by `source_queue_family`; a released image is used by
    ///   `command_buffers` last in `old_layout`.
    /// - Every semaphore of `signals` belongs to that device and can be signalled (binary ones
    ///   are unsignalled with no pending signal).
    #[allow(unsafe_code)]
    pub unsafe fn submit_handover(
        &mut self,
        queue: &wgpu::Queue,
        acquires: &[ImageAcquire],
        command_buffers: Vec<wgpu::CommandBuffer>,
        releases: &[ImageRelease],
        signals: &[SemaphoreSignal],
    ) -> Result<wgpu::SubmissionIndex, HandoverError> {
        if self.broken {
            return Err(HandoverError::Broken);
        }
        let slot_index = self.next_slot;
        self.next_slot = (self.next_slot + 1) % self.slots.len();
        self.prepare_slot(slot_index)?;
        let slot = &self.slots[slot_index];
        let (acquire_commands, release_commands) = (slot.acquire_commands, slot.release_commands);
        let (acquire_done, render_done) = (slot.acquired, slot.rendered);

        let acquire_barriers: Vec<_> = acquires
            .iter()
            .map(|acquire| self.acquire_barrier(acquire))
            .collect();
        let release_barriers: Vec<_> = releases
            .iter()
            .map(|release| self.release_barrier(release))
            .collect();
        if !acquire_barriers.is_empty() {
            // The acquire's first scope is empty: the owner's last use is ordered by the
            // owner (QAROS hands a frame out only once it is done with it).
            self.record_barriers(
                acquire_commands,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::ALL_COMMANDS,
                &acquire_barriers,
            )?;
        }
        // Waiting on the chain semaphore at ALL_COMMANDS puts that wait in the release
        // barrier's first scope; the writes are made available by the semaphore.
        self.record_barriers(
            release_commands,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            &release_barriers,
        )?;

        if !acquire_barriers.is_empty() {
            self.submit_raw(
                acquire_commands,
                None,
                &[acquire_done],
                &[],
                vk::Fence::null(),
            )
            .map_err(vulkan_error("vkQueueSubmit of the acquire batch"))?;
            // SAFETY: `acquire_done` was just signalled by a batch on this queue's device, and
            // nothing else waits on it.
            unsafe {
                stage_wait_semaphore(
                    queue,
                    SemaphoreWait {
                        semaphore: acquire_done,
                        value: None,
                        stage: vk::PipelineStageFlags::ALL_COMMANDS,
                    },
                );
            }
        }
        // SAFETY: `render_done` is unsignalled (the slot's previous release batch waited on it
        // and has completed), and it belongs to this queue's device.
        let staged = unsafe {
            stage_signal_semaphore(
                queue,
                SemaphoreSignal {
                    semaphore: render_done,
                    value: None,
                },
            )
        };
        if !staged {
            return Err(HandoverError::NotVulkan);
        }
        let submission = queue.submit(command_buffers);

        let fence = self.slots[slot_index].fence;
        let release_submitted = self
            .submit_raw(release_commands, Some(render_done), &[], signals, fence)
            .map_err(vulkan_error("vkQueueSubmit of the release batch"));
        match release_submitted {
            Ok(()) => {
                self.slots[slot_index].submitted = true;
                Ok(submission)
            }
            Err(error) => {
                self.broken = true;
                Err(error)
            }
        }
    }

    #[allow(unsafe_code)]
    fn prepare_slot(&mut self, slot_index: usize) -> Result<(), HandoverError> {
        let slot = &mut self.slots[slot_index];
        if slot.submitted {
            // SAFETY: the fence belongs to this device and was submitted with the slot's batch.
            unsafe {
                self.raw_device
                    .wait_for_fences(&[slot.fence], true, SLOT_WAIT_TIMEOUT_NANOSECONDS)
            }
            .map_err(vulkan_error("vkWaitForFences on a handover slot"))?;
            // SAFETY: the fence is signalled and no longer used by a pending submission.
            unsafe { self.raw_device.reset_fences(&[slot.fence]) }
                .map_err(vulkan_error("vkResetFences"))?;
            slot.submitted = false;
        }
        Ok(())
    }

    #[allow(unsafe_code)]
    fn record_barriers(
        &self,
        command_buffer: vk::CommandBuffer,
        source_stage: vk::PipelineStageFlags,
        destination_stage: vk::PipelineStageFlags,
        barriers: &[vk::ImageMemoryBarrier<'_>],
    ) -> Result<(), HandoverError> {
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the command buffer's last submission has completed (`prepare_slot`), and
        // its pool allows resetting single buffers.
        unsafe {
            self.raw_device
                .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
                .map_err(vulkan_error("vkResetCommandBuffer"))?;
            self.raw_device
                .begin_command_buffer(command_buffer, &begin_info)
                .map_err(vulkan_error("vkBeginCommandBuffer"))?;
            if !barriers.is_empty() {
                self.raw_device.cmd_pipeline_barrier(
                    command_buffer,
                    source_stage,
                    destination_stage,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    barriers,
                );
            }
            self.raw_device
                .end_command_buffer(command_buffer)
                .map_err(vulkan_error("vkEndCommandBuffer"))?;
        }
        Ok(())
    }

    /// Source and destination family of a transfer between `other_family` and wgpu's, or
    /// `IGNORED` twice when no transfer is needed.
    fn transfer_families(&self, other_family: u32) -> (u32, u32) {
        if other_family == vk::QUEUE_FAMILY_IGNORED || other_family == self.queue_family_index {
            (vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED)
        } else {
            (other_family, self.queue_family_index)
        }
    }

    fn acquire_barrier(&self, acquire: &ImageAcquire) -> vk::ImageMemoryBarrier<'static> {
        let (source_family, destination_family) =
            self.transfer_families(acquire.source_queue_family);
        vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(attachment_write_access(acquire.aspect))
            .old_layout(acquire.old_layout)
            .new_layout(acquire.new_layout)
            .src_queue_family_index(source_family)
            .dst_queue_family_index(destination_family)
            .image(acquire.image)
            .subresource_range(whole_image(acquire.aspect))
    }

    fn release_barrier(&self, release: &ImageRelease) -> vk::ImageMemoryBarrier<'static> {
        let (destination_family, source_family) =
            self.transfer_families(release.destination_queue_family);
        vk::ImageMemoryBarrier::default()
            .src_access_mask(attachment_write_access(release.aspect))
            .dst_access_mask(vk::AccessFlags::empty())
            .old_layout(release.old_layout)
            .new_layout(release.new_layout)
            .src_queue_family_index(source_family)
            .dst_queue_family_index(destination_family)
            .image(release.image)
            .subresource_range(whole_image(release.aspect))
    }

    #[allow(unsafe_code)]
    fn submit_raw(
        &self,
        command_buffer: vk::CommandBuffer,
        wait: Option<vk::Semaphore>,
        binary_signals: &[vk::Semaphore],
        signals: &[SemaphoreSignal],
        fence: vk::Fence,
    ) -> Result<(), vk::Result> {
        let wait_semaphores: Vec<vk::Semaphore> = wait.into_iter().collect();
        let wait_stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; wait_semaphores.len()];
        let wait_values = vec![0; wait_semaphores.len()];
        let command_buffers = [command_buffer];
        let mut signal_semaphores: Vec<vk::Semaphore> = binary_signals.to_vec();
        let mut signal_values = vec![0; binary_signals.len()];
        for signal in signals {
            signal_semaphores.push(signal.semaphore);
            signal_values.push(signal.value.unwrap_or(0));
        }
        let mut timeline_info = vk::TimelineSemaphoreSubmitInfo::default()
            .wait_semaphore_values(&wait_values)
            .signal_semaphore_values(&signal_values);
        let mut submit_info = vk::SubmitInfo::default()
            .wait_semaphores(&wait_semaphores)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&command_buffers)
            .signal_semaphores(&signal_semaphores);
        if signals.iter().any(|signal| signal.value.is_some()) {
            submit_info = submit_info.push_next(&mut timeline_info);
        }
        // SAFETY: the caller of `submit_handover` guarantees exclusive queue access and valid
        // signal semaphores; the arrays and chained structs outlive the call.
        unsafe {
            self.raw_device
                .queue_submit(self.queue, &[submit_info], fence)
        }
    }
}

impl Drop for HandoverQueue {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        let pending_fences: Vec<vk::Fence> = self
            .slots
            .iter()
            .filter(|slot| slot.submitted)
            .map(|slot| slot.fence)
            .collect();
        if !pending_fences.is_empty() {
            // SAFETY: the fences belong to this device.
            let waited = unsafe {
                self.raw_device.wait_for_fences(
                    &pending_fences,
                    true,
                    SLOT_WAIT_TIMEOUT_NANOSECONDS,
                )
            };
            if let Err(result) = waited {
                log::error!(
                    "vulkan-device: handover batches did not finish ({result}); leaking their objects"
                );
                return;
            }
        }
        // SAFETY: no submission uses these objects any more; each is destroyed once.
        unsafe {
            for slot in &self.slots {
                self.raw_device.destroy_semaphore(slot.acquired, None);
                self.raw_device.destroy_semaphore(slot.rendered, None);
                self.raw_device.destroy_fence(slot.fence, None);
            }
            if self.command_pool != vk::CommandPool::null() {
                self.raw_device
                    .destroy_command_pool(self.command_pool, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_layouts_and_aspects_follow_the_format() {
        assert_eq!(
            attachment_layout(wgpu::TextureFormat::Rgba8Unorm),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
        );
        assert_eq!(
            attachment_layout(wgpu::TextureFormat::Depth32Float),
            vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL
        );
        assert_eq!(
            image_aspect(wgpu::TextureFormat::Bgra8Unorm),
            vk::ImageAspectFlags::COLOR
        );
        assert_eq!(
            image_aspect(wgpu::TextureFormat::Depth32Float),
            vk::ImageAspectFlags::DEPTH
        );
        assert_eq!(
            image_aspect(wgpu::TextureFormat::Depth24PlusStencil8),
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
        );
    }
}
