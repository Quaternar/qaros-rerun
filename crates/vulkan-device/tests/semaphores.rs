//! Semaphores staged on wgpu's submissions and raw batches chained to them.

#![allow(clippy::expect_used, clippy::unwrap_used, unsafe_code)]

mod common;

use std::time::Duration;

use ash::vk;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vulkan_device::{
    SemaphoreSignal, SemaphoreWait, VulkanDevice, stage_signal_semaphore, stage_wait_semaphore,
    submit_sync_batch,
};

const ONE_SECOND_IN_NANOSECONDS: u64 = 1_000_000_000;

fn timeline_semaphore(ash_device: &ash::Device) -> vk::Semaphore {
    let mut type_info = vk::SemaphoreTypeCreateInfo::default()
        .semaphore_type(vk::SemaphoreType::TIMELINE)
        .initial_value(0);
    let info = vk::SemaphoreCreateInfo::default().push_next(&mut type_info);
    unsafe { ash_device.create_semaphore(&info, None) }.unwrap()
}

fn binary_semaphore(ash_device: &ash::Device) -> vk::Semaphore {
    unsafe { ash_device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.unwrap()
}

fn timeline_value(ash_device: &ash::Device, semaphore: vk::Semaphore) -> u64 {
    unsafe { ash_device.get_semaphore_counter_value(semaphore) }.unwrap()
}

fn wait_for_timeline(ash_device: &ash::Device, semaphore: vk::Semaphore, value: u64) {
    let semaphores = [semaphore];
    let values = [value];
    let info = vk::SemaphoreWaitInfo::default()
        .semaphores(&semaphores)
        .values(&values);
    unsafe { ash_device.wait_semaphores(&info, ONE_SECOND_IN_NANOSECONDS) }.unwrap();
}

/// A small real submission: a buffer cleared on the GPU.
fn submit_work(device: &wgpu::Device, queue: &wgpu::Queue) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("semaphore test"),
        size: 256,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.clear_buffer(&buffer, 0, None);
    queue.submit([encoder.finish()]);
}

#[test]
fn wgpu_signals_a_staged_timeline_semaphore() {
    let Some(vulkan) = common::default_device() else {
        return;
    };
    let ash_device = common::ash_device(&vulkan.device);
    let done = timeline_semaphore(&ash_device);

    let staged = unsafe {
        stage_signal_semaphore(
            &vulkan.queue,
            SemaphoreSignal {
                semaphore: done,
                value: Some(7),
            },
        )
    };
    assert!(staged);
    submit_work(&vulkan.device, &vulkan.queue);
    wait_for_timeline(&ash_device, done, 7);
    assert_eq!(timeline_value(&ash_device, done), 7);

    vulkan
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    unsafe { ash_device.destroy_semaphore(done, None) };
}

#[test]
fn a_staged_wait_holds_wgpu_work_until_the_host_signals() {
    let Some(vulkan) = common::default_device() else {
        return;
    };
    let ash_device = common::ash_device(&vulkan.device);
    let gate = timeline_semaphore(&ash_device);
    let done = timeline_semaphore(&ash_device);

    unsafe {
        stage_wait_semaphore(
            &vulkan.queue,
            SemaphoreWait {
                semaphore: gate,
                value: Some(5),
                stage: vk::PipelineStageFlags::ALL_COMMANDS,
            },
        );
        stage_signal_semaphore(
            &vulkan.queue,
            SemaphoreSignal {
                semaphore: done,
                value: Some(1),
            },
        );
    }
    submit_work(&vulkan.device, &vulkan.queue);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        timeline_value(&ash_device, done),
        0,
        "ran before the gate opened"
    );

    let signal = vk::SemaphoreSignalInfo::default().semaphore(gate).value(5);
    unsafe { ash_device.signal_semaphore(&signal) }.unwrap();
    wait_for_timeline(&ash_device, done, 1);

    vulkan
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    unsafe {
        ash_device.destroy_semaphore(gate, None);
        ash_device.destroy_semaphore(done, None);
    }
}

#[test]
fn raw_batches_chain_before_and_after_a_wgpu_submission() {
    for vulkan in common::devices_for_both_raw_queues() {
        eprintln!("raw batches on {:?}", vulkan.raw_queue);
        raw_batches_chain_on(&vulkan);
    }
}

fn raw_batches_chain_on(vulkan: &VulkanDevice) {
    let ash_device = common::ash_device(&vulkan.device);
    let gate = timeline_semaphore(&ash_device);
    let before = binary_semaphore(&ash_device);
    let after = binary_semaphore(&ash_device);
    let done = timeline_semaphore(&ash_device);

    unsafe {
        // Stands in for the keyed-mutex acquire: held until the host opens `gate`.
        submit_sync_batch(
            &vulkan.raw_queue,
            &[SemaphoreWait {
                semaphore: gate,
                value: Some(1),
                stage: vk::PipelineStageFlags::ALL_COMMANDS,
            }],
            &[SemaphoreSignal {
                semaphore: before,
                value: None,
            }],
            None,
            vk::Fence::null(),
        )
        .unwrap();
        stage_wait_semaphore(
            &vulkan.queue,
            SemaphoreWait {
                semaphore: before,
                value: None,
                stage: vk::PipelineStageFlags::ALL_COMMANDS,
            },
        );
        stage_signal_semaphore(
            &vulkan.queue,
            SemaphoreSignal {
                semaphore: after,
                value: None,
            },
        );
    }
    submit_work(&vulkan.device, &vulkan.queue);
    unsafe {
        // Stands in for the keyed-mutex release.
        submit_sync_batch(
            &vulkan.raw_queue,
            &[SemaphoreWait {
                semaphore: after,
                value: None,
                stage: vk::PipelineStageFlags::ALL_COMMANDS,
            }],
            &[SemaphoreSignal {
                semaphore: done,
                value: Some(1),
            }],
            None,
            vk::Fence::null(),
        )
        .unwrap();
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        timeline_value(&ash_device, done),
        0,
        "the chain ran before the gate opened"
    );

    let signal = vk::SemaphoreSignalInfo::default().semaphore(gate).value(1);
    unsafe { ash_device.signal_semaphore(&signal) }.unwrap();
    wait_for_timeline(&ash_device, done, 1);

    vulkan
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    unsafe {
        ash_device.device_wait_idle().unwrap();
        for semaphore in [gate, before, after, done] {
            ash_device.destroy_semaphore(semaphore, None);
        }
    }
}

/// Another thread's wgpu submission may take the wait staged for ours: an empty submission
/// right after staging stands in for it. The work still waits for the raw batch before it. The
/// signal is staged after the work and flushed by an empty submission, as `HandoverQueue` does;
/// that the signal covers the work is checked by content in the handover test.
#[test]
fn a_chain_holds_when_another_submission_takes_the_staged_semaphores() {
    for vulkan in common::devices_for_both_raw_queues() {
        eprintln!("raw batches on {:?}", vulkan.raw_queue);
        chain_holds_on(&vulkan);
    }
}

fn chain_holds_on(vulkan: &VulkanDevice) {
    let ash_device = common::ash_device(&vulkan.device);
    let gate = timeline_semaphore(&ash_device);
    let before = binary_semaphore(&ash_device);
    let after = binary_semaphore(&ash_device);
    let done = timeline_semaphore(&ash_device);
    let work_done = Arc::new(AtomicBool::new(false));

    unsafe {
        submit_sync_batch(
            &vulkan.raw_queue,
            &[SemaphoreWait {
                semaphore: gate,
                value: Some(1),
                stage: vk::PipelineStageFlags::ALL_COMMANDS,
            }],
            &[SemaphoreSignal {
                semaphore: before,
                value: None,
            }],
            None,
            vk::Fence::null(),
        )
        .unwrap();
        stage_wait_semaphore(
            &vulkan.queue,
            SemaphoreWait {
                semaphore: before,
                value: None,
                stage: vk::PipelineStageFlags::ALL_COMMANDS,
            },
        );
    }
    vulkan.queue.submit([]);
    submit_work(&vulkan.device, &vulkan.queue);
    let work_done_flag = Arc::clone(&work_done);
    vulkan
        .queue
        .on_submitted_work_done(move || work_done_flag.store(true, Ordering::SeqCst));
    unsafe {
        stage_signal_semaphore(
            &vulkan.queue,
            SemaphoreSignal {
                semaphore: after,
                value: None,
            },
        );
    }
    vulkan.queue.submit([]);
    unsafe {
        submit_sync_batch(
            &vulkan.raw_queue,
            &[SemaphoreWait {
                semaphore: after,
                value: None,
                stage: vk::PipelineStageFlags::ALL_COMMANDS,
            }],
            &[SemaphoreSignal {
                semaphore: done,
                value: Some(1),
            }],
            None,
            vk::Fence::null(),
        )
        .unwrap();
    }
    std::thread::sleep(Duration::from_millis(100));
    vulkan.device.poll(wgpu::PollType::Poll).unwrap();
    assert!(
        !work_done.load(Ordering::SeqCst),
        "the work ran before the raw batch it was staged to wait for"
    );
    assert_eq!(
        timeline_value(&ash_device, done),
        0,
        "the chain ran before the gate opened"
    );

    let signal = vk::SemaphoreSignalInfo::default().semaphore(gate).value(1);
    unsafe { ash_device.signal_semaphore(&signal) }.unwrap();
    wait_for_timeline(&ash_device, done, 1);
    vulkan
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert!(work_done.load(Ordering::SeqCst));

    unsafe {
        ash_device.device_wait_idle().unwrap();
        for semaphore in [gate, before, after, done] {
            ash_device.destroy_semaphore(semaphore, None);
        }
    }
}
