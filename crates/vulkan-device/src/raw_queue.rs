use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use ash::vk;

use crate::VulkanApi;

/// The `VkQueue` raw batches go to: queue 1 of wgpu's queue family on a device made by
/// [`crate::VulkanDevice::create`] whose family has two queues, else wgpu's own queue.
///
/// Raw calls on it are serialized with each other by a mutex of its own, whatever the thread.
/// On wgpu's own queue ([`Self::shares_wgpu_queue`]) they must also be ordered with wgpu's
/// submits by the caller, since wgpu does not see them.
#[derive(Clone)]
pub struct RawQueue {
    shared: Arc<RawQueueShared>,
}

struct RawQueueShared {
    /// Keeps the `VkDevice` alive for as long as the queue is used.
    _device: wgpu::Device,
    raw_device: ash::Device,
    queue: vk::Queue,
    family_index: u32,
    queue_index: u32,
    shares_wgpu_queue: bool,
    access: Mutex<()>,
}

impl fmt::Debug for RawQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawQueue")
            .field("family_index", &self.shared.family_index)
            .field("queue_index", &self.shared.queue_index)
            .field("shares_wgpu_queue", &self.shared.shares_wgpu_queue)
            .finish_non_exhaustive()
    }
}

impl RawQueue {
    /// wgpu's own queue, for a device that has no queue for raw batches beside it (one wgpu
    /// created, for example). `None` when `device` is not on wgpu's Vulkan backend. The mutex
    /// belongs to this value and its clones, so share one `RawQueue` per device by cloning it.
    pub fn of_wgpu(device: &wgpu::Device) -> Option<Self> {
        #[allow(unsafe_code)]
        // SAFETY: only handles are read; the ash device clone stays valid while the
        // `wgpu::Device` clone kept beside it lives.
        let hal_device = unsafe { device.as_hal::<VulkanApi>() }?;
        Some(Self::new(RawQueueShared {
            _device: device.clone(),
            raw_device: hal_device.raw_device().clone(),
            queue: hal_device.raw_queue(),
            family_index: hal_device.queue_family_index(),
            queue_index: hal_device.queue_index(),
            shares_wgpu_queue: true,
            access: Mutex::new(()),
        }))
    }

    /// Queue `queue_index` of wgpu's family, which the device was created with and wgpu does
    /// not use. `None` when `device` is not on Vulkan or `queue_index` is wgpu's.
    ///
    /// # Safety
    ///
    /// The device was created with at least `queue_index + 1` queues in wgpu's family.
    #[allow(unsafe_code)]
    pub(crate) unsafe fn beside_wgpu(device: &wgpu::Device, queue_index: u32) -> Option<Self> {
        // SAFETY: as in `of_wgpu`.
        let hal_device = unsafe { device.as_hal::<VulkanApi>() }?;
        if hal_device.queue_index() == queue_index {
            return None;
        }
        let family_index = hal_device.queue_family_index();
        let raw_device = hal_device.raw_device().clone();
        // SAFETY: the caller guarantees the device has this queue.
        let queue = unsafe { raw_device.get_device_queue(family_index, queue_index) };
        Some(Self::new(RawQueueShared {
            _device: device.clone(),
            raw_device,
            queue,
            family_index,
            queue_index,
            shares_wgpu_queue: false,
            access: Mutex::new(()),
        }))
    }

    fn new(shared: RawQueueShared) -> Self {
        Self {
            shared: Arc::new(shared),
        }
    }

    /// True on the one-queue fallback: raw batches share wgpu's `VkQueue`.
    pub fn shares_wgpu_queue(&self) -> bool {
        self.shared.shares_wgpu_queue
    }

    /// Always wgpu's queue family, so images need no ownership transfer between the queues.
    pub fn family_index(&self) -> u32 {
        self.shared.family_index
    }

    pub fn queue_index(&self) -> u32 {
        self.shared.queue_index
    }

    pub fn handle(&self) -> vk::Queue {
        self.shared.queue
    }

    pub(crate) fn raw_device(&self) -> &ash::Device {
        &self.shared.raw_device
    }

    /// `vkQueueSubmit` under the queue's mutex.
    ///
    /// # Safety
    ///
    /// As for `vkQueueSubmit`, apart from synchronizing with other raw calls on this queue;
    /// on wgpu's queue, no wgpu submit runs meanwhile.
    #[allow(unsafe_code)]
    pub(crate) unsafe fn submit(
        &self,
        submits: &[vk::SubmitInfo<'_>],
        fence: vk::Fence,
    ) -> Result<(), vk::Result> {
        let _access = self
            .shared
            .access
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // SAFETY: as the caller guarantees; the mutex orders this call with other raw calls.
        unsafe {
            self.shared
                .raw_device
                .queue_submit(self.shared.queue, submits, fence)
        }
    }

    /// `vkQueueWaitIdle` under the queue's mutex.
    ///
    /// # Safety
    ///
    /// On wgpu's queue, no wgpu submit runs meanwhile.
    #[allow(unsafe_code)]
    pub(crate) unsafe fn wait_idle(&self) -> Result<(), vk::Result> {
        let _access = self
            .shared
            .access
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // SAFETY: as in `submit`.
        unsafe { self.shared.raw_device.queue_wait_idle(self.shared.queue) }
    }
}
