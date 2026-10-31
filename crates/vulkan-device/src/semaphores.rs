//! Semaphores around wgpu's submissions, for work shared with another API on the same device.
//!
//! - Staged on wgpu's queue: [`stage_wait_semaphore`] and [`stage_signal_semaphore`] attach to
//!   the next `wgpu::Queue::submit`, whichever code calls it, so stage right before your own
//!   submit, on the thread that submits.
//! - A raw batch: [`submit_sync_batch`] is an empty `vkQueueSubmit` on wgpu's queue carrying
//!   waits, signals and optionally a Win32 keyed mutex acquire/release, things wgpu cannot put
//!   on its own submissions. A semaphore wait only orders the batch it belongs to, so a raw batch
//!   is chained to wgpu's work through a semaphore: the raw batch signals one that wgpu's
//!   submission waits on, or waits on one that wgpu's submission signals.

use ash::vk;

use crate::VulkanApi;

#[derive(Clone, Copy, Debug)]
pub struct SemaphoreWait {
    pub semaphore: vk::Semaphore,
    /// `Some` for a timeline semaphore (wait until at least this value), `None` for binary.
    pub value: Option<u64>,
    /// The stages of the waiting batch that must not start before the signal.
    pub stage: vk::PipelineStageFlags,
}

#[derive(Clone, Copy, Debug)]
pub struct SemaphoreSignal {
    pub semaphore: vk::Semaphore,
    /// `Some` for a timeline semaphore (the value to set), `None` for binary.
    pub value: Option<u64>,
}

/// The Win32 keyed mutexes of imported D3D memory to take and give back in a raw batch
/// (`VkWin32KeyedMutexAcquireReleaseInfoKHR`, `VK_KHR_win32_keyed_mutex`).
#[derive(Clone, Debug, Default)]
pub struct KeyedMutexSync {
    pub acquire: Vec<KeyedMutexAcquire>,
    pub release: Vec<KeyedMutexRelease>,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyedMutexAcquire {
    pub memory: vk::DeviceMemory,
    pub key: u64,
    pub timeout_milliseconds: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyedMutexRelease {
    pub memory: vk::DeviceMemory,
    pub key: u64,
}

/// Makes wgpu's next submission on `queue` wait for `wait`. Returns `false` when the queue is
/// not on the Vulkan backend.
///
/// # Safety
///
/// `wait.semaphore` is a semaphore of this queue's device, of the kind `wait.value` says, and
/// stays alive until that submission has completed.
#[allow(unsafe_code)]
pub unsafe fn stage_wait_semaphore(queue: &wgpu::Queue, wait: SemaphoreWait) -> bool {
    // SAFETY: only wgpu-hal's pending wait list is touched; the caller vouches for the handle.
    let Some(hal_queue) = (unsafe { queue.as_hal::<VulkanApi>() }) else {
        return false;
    };
    hal_queue.add_wait_semaphore(wait.semaphore, wait.value, wait.stage);
    true
}

/// Makes wgpu's next submission on `queue` signal `signal` when it completes. Returns `false`
/// when the queue is not on the Vulkan backend.
///
/// # Safety
///
/// As for [`stage_wait_semaphore`].
#[allow(unsafe_code)]
pub unsafe fn stage_signal_semaphore(queue: &wgpu::Queue, signal: SemaphoreSignal) -> bool {
    // SAFETY: as in `stage_wait_semaphore`.
    let Some(hal_queue) = (unsafe { queue.as_hal::<VulkanApi>() }) else {
        return false;
    };
    hal_queue.add_signal_semaphore(signal.semaphore, signal.value);
    true
}

/// Submits a batch without command buffers to wgpu's queue: `waits`, then the keyed mutex
/// acquires, then the releases and `signals`. `fence` may be null.
///
/// # Safety
///
/// - Every handle belongs to `device`'s `VkDevice`; the semaphores are of the kinds their
///   `value` says, `VK_KHR_win32_keyed_mutex` is enabled when `keyed_mutex` is given, and the
///   memories were imported from keyed-mutex D3D resources.
/// - No other thread submits to wgpu's queue meanwhile (Vulkan requires queue access to be
///   externally synchronized; wgpu submits from the thread calling `wgpu::Queue::submit`).
#[allow(unsafe_code)]
pub unsafe fn submit_sync_batch(
    device: &wgpu::Device,
    waits: &[SemaphoreWait],
    signals: &[SemaphoreSignal],
    keyed_mutex: Option<&KeyedMutexSync>,
    fence: vk::Fence,
) -> Result<(), vk::Result> {
    let wait_semaphores: Vec<vk::Semaphore> = waits.iter().map(|wait| wait.semaphore).collect();
    let wait_stages: Vec<vk::PipelineStageFlags> = waits.iter().map(|wait| wait.stage).collect();
    let wait_values: Vec<u64> = waits.iter().map(|wait| wait.value.unwrap_or(0)).collect();
    let signal_semaphores: Vec<vk::Semaphore> =
        signals.iter().map(|signal| signal.semaphore).collect();
    let signal_values: Vec<u64> = signals
        .iter()
        .map(|signal| signal.value.unwrap_or(0))
        .collect();
    let any_timeline = waits.iter().any(|wait| wait.value.is_some())
        || signals.iter().any(|signal| signal.value.is_some());

    let mut timeline_info = vk::TimelineSemaphoreSubmitInfo::default()
        .wait_semaphore_values(&wait_values)
        .signal_semaphore_values(&signal_values);
    let keyed_mutex = keyed_mutex.cloned().unwrap_or_default();
    let acquire_memories: Vec<vk::DeviceMemory> = keyed_mutex
        .acquire
        .iter()
        .map(|acquire| acquire.memory)
        .collect();
    let acquire_keys: Vec<u64> = keyed_mutex
        .acquire
        .iter()
        .map(|acquire| acquire.key)
        .collect();
    let acquire_timeouts: Vec<u32> = keyed_mutex
        .acquire
        .iter()
        .map(|acquire| acquire.timeout_milliseconds)
        .collect();
    let release_memories: Vec<vk::DeviceMemory> = keyed_mutex
        .release
        .iter()
        .map(|release| release.memory)
        .collect();
    let release_keys: Vec<u64> = keyed_mutex
        .release
        .iter()
        .map(|release| release.key)
        .collect();
    let mut keyed_mutex_info = vk::Win32KeyedMutexAcquireReleaseInfoKHR::default()
        .acquire_syncs(&acquire_memories)
        .acquire_keys(&acquire_keys)
        .acquire_timeouts(&acquire_timeouts)
        .release_syncs(&release_memories)
        .release_keys(&release_keys);

    let mut submit_info = vk::SubmitInfo::default()
        .wait_semaphores(&wait_semaphores)
        .wait_dst_stage_mask(&wait_stages)
        .signal_semaphores(&signal_semaphores);
    if any_timeline {
        submit_info = submit_info.push_next(&mut timeline_info);
    }
    if !acquire_memories.is_empty() || !release_memories.is_empty() {
        submit_info = submit_info.push_next(&mut keyed_mutex_info);
    }

    // SAFETY: the caller guarantees the handles and exclusive queue access; the arrays and
    // chained structs outlive the call.
    let Some(hal_device) = (unsafe { device.as_hal::<VulkanApi>() }) else {
        return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
    };
    unsafe {
        hal_device
            .raw_device()
            .queue_submit(hal_device.raw_queue(), &[submit_info], fence)
    }
}

/// Waits until wgpu's queue has finished everything submitted to it, raw batches included
/// (`vkQueueWaitIdle`); `wgpu::Device::poll` waits only for wgpu's own submissions.
///
/// # Safety
///
/// No other thread submits to wgpu's queue meanwhile (queue access is externally
/// synchronized in Vulkan).
#[allow(unsafe_code)]
pub unsafe fn wait_queue_idle(device: &wgpu::Device) -> Result<(), vk::Result> {
    // SAFETY: as the caller guarantees.
    let Some(hal_device) = (unsafe { device.as_hal::<VulkanApi>() }) else {
        return Err(vk::Result::ERROR_FEATURE_NOT_PRESENT);
    };
    unsafe {
        hal_device
            .raw_device()
            .queue_wait_idle(hal_device.raw_queue())
    }
}
