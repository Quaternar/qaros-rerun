//! `VulkanSink` driven by a local stream that behaves like the QAROS Vulkan render sender: it
//! owns one color and one depth image per eye and a binary semaphore on the viewer's device,
//! hands them out in the layout and queue family the last frame wrote back, and records what
//! `show_frame` writes back. A consumer batch waits on the semaphore, acquires the images from
//! `VK_QUEUE_FAMILY_EXTERNAL`, copies them out and gives them back, as the encoder would. Runs
//! on MoltenVK; skips without a Vulkan loader.

#![cfg(feature = "vulkan")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::float_cmp,
    unsafe_code
)]

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use common::{ConsumedImage, Raw, color_of, create_device, render_eyes};
use frame_sink::{
    DepthEncoding, DepthRange, Fov, FrameKey, GpuFrameSink, Pose, ShowInfo, SinkError, UserId,
};
use qaros_sink::vulkan::{
    ImageState, SenderEye, SenderFrame, SenderImage, VulkanFrameStream, VulkanSink,
    VulkanSinkOptions,
};
use vulkan_device::vk;

const RESOLUTION: [u32; 2] = [16, 8];
const COLOR_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
const DEPTH_FORMAT: vk::Format = vk::Format::D32_SFLOAT;

/// What the test observes of the stream.
#[derive(Default)]
struct StreamLog {
    shown: Vec<(Vec<ImageState>, ShowInfo)>,
    discarded: usize,
}

/// Images in the order left color, left depth, right color, right depth, like the cube's
/// separated layout.
struct LocalStream {
    images: Vec<SenderImage>,
    semaphore: vk::Semaphore,
    states: Vec<ImageState>,
    frame_open: bool,
    log: Rc<RefCell<StreamLog>>,
}

impl LocalStream {
    fn new(
        images: Vec<SenderImage>,
        semaphore: vk::Semaphore,
        log: &Rc<RefCell<StreamLog>>,
    ) -> Self {
        Self {
            states: images
                .iter()
                .map(|image| ImageState {
                    layout: image.layout,
                    queue_family_index: image.queue_family_index,
                })
                .collect(),
            images,
            semaphore,
            frame_open: false,
            log: Rc::clone(log),
        }
    }
}

const ONLY_USER: UserId = UserId(1);

impl VulkanFrameStream for LocalStream {
    fn update_users(&mut self) {}

    fn users(&self) -> Vec<UserId> {
        vec![ONLY_USER]
    }

    fn try_begin_frame(&mut self, user: UserId) -> Result<Option<SenderFrame>, SinkError> {
        assert_eq!(user, ONLY_USER);
        assert!(!self.frame_open, "a frame began while one was open");
        self.frame_open = true;
        let images = self
            .images
            .iter()
            .zip(&self.states)
            .map(|(image, state)| SenderImage {
                layout: state.layout,
                queue_family_index: state.queue_family_index,
                ..*image
            })
            .collect();
        let eye = |index: usize, x: f32| SenderEye {
            pose: Pose::from_xyzw_and_position([0.0, 0.0, 0.0, 1.0], [x, 1.6, 0.0]),
            fov: Fov {
                angle_left: -0.8,
                angle_right: 0.8,
                angle_up: 0.7,
                angle_down: -0.7,
            },
            color_image: index * 2,
            depth_image: Some(index * 2 + 1),
        };
        Ok(Some(SenderFrame {
            eyes: [eye(0, -0.032), eye(1, 0.032)],
            images,
            semaphore: self.semaphore,
            volume: None,
        }))
    }

    fn show_frame(
        &mut self,
        user: UserId,
        states: &[ImageState],
        show: ShowInfo,
    ) -> Result<(), SinkError> {
        assert_eq!(user, ONLY_USER);
        assert!(self.frame_open);
        self.frame_open = false;
        self.states = states.to_vec();
        self.log.borrow_mut().shown.push((states.to_vec(), show));
        Ok(())
    }

    fn discard_frame(&mut self, user: UserId) {
        assert_eq!(user, ONLY_USER);
        assert!(self.frame_open);
        self.frame_open = false;
        self.log.borrow_mut().discarded += 1;
    }
}

/// The local stream with color images only: eye `n` renders into image `n`.
struct ColorOnly(LocalStream);

impl VulkanFrameStream for ColorOnly {
    fn update_users(&mut self) {}

    fn users(&self) -> Vec<UserId> {
        self.0.users()
    }

    fn try_begin_frame(&mut self, user: UserId) -> Result<Option<SenderFrame>, SinkError> {
        let mut frame = self.0.try_begin_frame(user)?.expect("a frame");
        for (index, eye) in frame.eyes.iter_mut().enumerate() {
            eye.color_image = index;
            eye.depth_image = None;
        }
        Ok(Some(frame))
    }

    fn show_frame(
        &mut self,
        user: UserId,
        states: &[ImageState],
        show: ShowInfo,
    ) -> Result<(), SinkError> {
        self.0.show_frame(user, states, show)
    }

    fn discard_frame(&mut self, user: UserId) {
        self.0.discard_frame(user);
    }
}

/// One local stream per user, each with its own images and semaphore; users can be removed
/// between viewer frames, the way a peer leaves.
struct PerUserStreams {
    streams: BTreeMap<UserId, LocalStream>,
    leaving: Vec<UserId>,
}

impl VulkanFrameStream for PerUserStreams {
    fn update_users(&mut self) {
        for user in self.leaving.drain(..) {
            let stream = self.streams.remove(&user).expect("a known user");
            assert!(!stream.frame_open, "{user} left with a frame open");
        }
    }

    fn users(&self) -> Vec<UserId> {
        self.streams.keys().copied().collect()
    }

    fn try_begin_frame(&mut self, user: UserId) -> Result<Option<SenderFrame>, SinkError> {
        self.streams
            .get_mut(&user)
            .expect("a known user")
            .try_begin_frame(ONLY_USER)
    }

    fn show_frame(
        &mut self,
        user: UserId,
        states: &[ImageState],
        show: ShowInfo,
    ) -> Result<(), SinkError> {
        self.streams
            .get_mut(&user)
            .expect("a known user")
            .show_frame(ONLY_USER, states, show)
    }

    fn discard_frame(&mut self, user: UserId) {
        self.streams
            .get_mut(&user)
            .expect("a known user")
            .discard_frame(ONLY_USER);
    }
}

fn depth_range() -> DepthRange {
    DepthRange::new(0.1, 10.0, DepthEncoding::ForwardNormalized).unwrap()
}

fn only_request<Stream: VulkanFrameStream>(
    sink: &mut VulkanSink<Stream>,
) -> frame_sink::GpuFrameRequest {
    let mut requests = sink.begin_frames().unwrap();
    assert_eq!(requests.len(), 1, "one user, one frame");
    requests.remove(0)
}

fn sender_image(
    raw: &Raw,
    format: vk::Format,
    owned: &mut Vec<(vk::Image, vk::DeviceMemory)>,
) -> SenderImage {
    sized_sender_image(raw, format, RESOLUTION, owned)
}

fn sized_sender_image(
    raw: &Raw,
    format: vk::Format,
    resolution_in_pixel: [u32; 2],
    owned: &mut Vec<(vk::Image, vk::DeviceMemory)>,
) -> SenderImage {
    let (wgpu_format, usage, features) = if format == DEPTH_FORMAT {
        (
            wgpu::TextureFormat::Depth32Float,
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT,
        )
    } else {
        (
            wgpu::TextureFormat::Rgba8Unorm,
            vk::ImageUsageFlags::COLOR_ATTACHMENT,
            vk::FormatFeatureFlags::COLOR_ATTACHMENT,
        )
    };
    let (image, memory) = raw.image(format, usage, resolution_in_pixel);
    owned.push((image, memory));
    SenderImage {
        image,
        format: wgpu_format,
        resolution_in_pixel,
        format_features: features,
        layout: vk::ImageLayout::UNDEFINED,
        queue_family_index: vk::QUEUE_FAMILY_IGNORED,
    }
}

fn release_everything(
    raw: &Raw,
    semaphore: vk::Semaphore,
    owned: Vec<(vk::Image, vk::DeviceMemory)>,
) {
    unsafe {
        raw.device.device_wait_idle().unwrap();
        raw.device.destroy_semaphore(semaphore, None);
        for (image, memory) in owned {
            raw.device.destroy_image(image, None);
            raw.device.free_memory(memory, None);
        }
    }
}

fn binary_semaphore(raw: &Raw) -> vk::Semaphore {
    unsafe {
        raw.device
            .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
    }
    .unwrap()
}

#[test]
fn frames_render_into_the_sender_images_and_reach_the_consumer_released() {
    let Some(vulkan) = create_device() else {
        return;
    };
    let raw = Raw::of(&vulkan.device);
    let mut owned = Vec::new();
    let images: Vec<SenderImage> = [COLOR_FORMAT, DEPTH_FORMAT, COLOR_FORMAT, DEPTH_FORMAT]
        .into_iter()
        .map(|format| sender_image(&raw, format, &mut owned))
        .collect();
    let semaphore = binary_semaphore(&raw);
    let log = Rc::new(RefCell::new(StreamLog::default()));
    let mut sink = VulkanSink::new(
        &vulkan.device,
        &vulkan.queue,
        LocalStream::new(images.clone(), semaphore, &log),
        VulkanSinkOptions {
            depth_range: depth_range(),
            stop_after_frames: Some(2),
        },
    )
    .unwrap();
    let released = ImageState {
        layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        queue_family_index: vk::QUEUE_FAMILY_EXTERNAL,
    };
    let consumed_images: Vec<ConsumedImage> = images
        .iter()
        .map(|image| ConsumedImage {
            image: image.image,
            aspect: if image.format == wgpu::TextureFormat::Depth32Float {
                vk::ImageAspectFlags::DEPTH
            } else {
                vk::ImageAspectFlags::COLOR
            },
            resolution_in_pixel: RESOLUTION,
        })
        .collect();

    let frames = [
        ([[255_u8, 0, 0, 255], [0, 0, 255, 255]], 0.25_f32),
        ([[0, 255, 0, 255], [255, 255, 255, 0]], 0.75),
    ];
    for (frame_index, (expected_colors, depth)) in frames.into_iter().enumerate() {
        let request = only_request(&mut sink);
        let key = request.request.key;
        assert_eq!(
            key,
            FrameKey {
                user: ONLY_USER,
                frame_id: frame_index as u64
            }
        );
        assert_eq!(request.request.resolution_in_pixel, RESOLUTION);
        assert!((request.request.views[0].pose.position.x + 0.032).abs() < 1e-6);
        assert!(
            sink.begin_frames().unwrap().is_empty(),
            "one frame in flight"
        );
        let commands = render_eyes(
            &vulkan.device,
            &request,
            expected_colors.map(color_of),
            depth,
        );
        drop(request);
        sink.submit_and_show(key, commands, ShowInfo::from_depth_range(depth_range()))
            .unwrap();

        let copies = raw.consume(Some(semaphore), &consumed_images);
        for (eye_index, expected_color) in expected_colors.iter().enumerate() {
            let color = &copies[eye_index * 2];
            assert!(
                color.chunks_exact(4).all(|pixel| pixel == expected_color),
                "eye {eye_index} color in frame {frame_index}: {:?}",
                &color[..4]
            );
            let depth_bytes = &copies[eye_index * 2 + 1];
            assert!(
                depth_bytes
                    .chunks_exact(4)
                    .all(|value| f32::from_ne_bytes(value.try_into().unwrap()) == depth),
                "eye {eye_index} depth in frame {frame_index}"
            );
        }
        let log = log.borrow();
        let (states, show) = log.shown.last().unwrap();
        assert_eq!(states, &vec![released; 4]);
        assert_eq!(show.near_far, depth_range().near_far);
    }
    assert!(!sink.is_streaming(), "stops after two shown frames");
    assert_eq!(sink.shown_frame_count(), 2);

    drop(sink);
    release_everything(&raw, semaphore, owned);
}

#[test]
fn depth_images_smaller_than_color_are_handed_out_at_their_own_size() {
    let Some(vulkan) = create_device() else {
        return;
    };
    let raw = Raw::of(&vulkan.device);
    let mut owned = Vec::new();
    let depth_resolution = [RESOLUTION[0] / 2, RESOLUTION[1] / 2];
    let images: Vec<SenderImage> = [COLOR_FORMAT, DEPTH_FORMAT, COLOR_FORMAT, DEPTH_FORMAT]
        .into_iter()
        .map(|format| {
            let resolution = if format == DEPTH_FORMAT {
                depth_resolution
            } else {
                RESOLUTION
            };
            sized_sender_image(&raw, format, resolution, &mut owned)
        })
        .collect();
    let semaphore = binary_semaphore(&raw);
    let log = Rc::new(RefCell::new(StreamLog::default()));
    let mut sink = VulkanSink::new(
        &vulkan.device,
        &vulkan.queue,
        LocalStream::new(images.clone(), semaphore, &log),
        VulkanSinkOptions {
            depth_range: depth_range(),
            stop_after_frames: None,
        },
    )
    .unwrap();

    let request = only_request(&mut sink);
    let key = request.request.key;
    assert_eq!(request.request.resolution_in_pixel, RESOLUTION);
    for eye in &request.eyes {
        let depth = eye.depth.as_ref().expect("a depth target");
        assert_eq!([depth.width(), depth.height()], depth_resolution);
        assert_eq!([eye.color.width(), eye.color.height()], RESOLUTION);
    }
    let commands = render_eyes(&vulkan.device, &request, [color_of([9, 8, 7, 255]); 2], 0.5);
    drop(request);
    sink.submit_and_show(key, commands, ShowInfo::from_depth_range(depth_range()))
        .unwrap();

    let consumed_images: Vec<ConsumedImage> = images
        .iter()
        .map(|image| ConsumedImage {
            image: image.image,
            aspect: if image.format == wgpu::TextureFormat::Depth32Float {
                vk::ImageAspectFlags::DEPTH
            } else {
                vk::ImageAspectFlags::COLOR
            },
            resolution_in_pixel: image.resolution_in_pixel,
        })
        .collect();
    let copies = raw.consume(Some(semaphore), &consumed_images);
    let depth_pixels = (depth_resolution[0] * depth_resolution[1]) as usize;
    for eye_index in 0..2 {
        let depth_bytes = &copies[eye_index * 2 + 1];
        assert_eq!(depth_bytes.len(), depth_pixels * 4);
        assert!(
            depth_bytes
                .chunks_exact(4)
                .all(|value| f32::from_ne_bytes(value.try_into().unwrap()) == 0.5)
        );
    }
    assert_eq!(log.borrow().shown.len(), 1);

    drop(sink);
    release_everything(&raw, semaphore, owned);
}

#[test]
fn an_abandoned_frame_still_signals_its_semaphore() {
    let Some(vulkan) = create_device() else {
        return;
    };
    let raw = Raw::of(&vulkan.device);
    let mut owned = Vec::new();
    let images: Vec<SenderImage> = [COLOR_FORMAT, COLOR_FORMAT]
        .into_iter()
        .map(|format| sender_image(&raw, format, &mut owned))
        .collect();
    let semaphore = binary_semaphore(&raw);
    let log = Rc::new(RefCell::new(StreamLog::default()));
    let mut sink = VulkanSink::new(
        &vulkan.device,
        &vulkan.queue,
        ColorOnly(LocalStream::new(images, semaphore, &log)),
        VulkanSinkOptions {
            depth_range: depth_range(),
            stop_after_frames: None,
        },
    )
    .unwrap();

    let request = only_request(&mut sink);
    let key = request.request.key;
    assert!(request.eyes.iter().all(|eye| eye.depth.is_none()));
    sink.abandon_frame(key);
    raw.wait_signalled(semaphore);
    assert_eq!(log.borrow().discarded, 1);
    assert!(log.borrow().shown.is_empty());

    let unknown = sink.submit_and_show(
        key,
        vulkan
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default())
            .finish(),
        ShowInfo::from_depth_range(depth_range()),
    );
    assert!(matches!(unknown, Err(SinkError::UnknownFrame(unknown_key)) if unknown_key == key));

    // A frame open when the sink goes is discarded, and its semaphore signalled, too.
    let request = only_request(&mut sink);
    drop(request);
    drop(sink);
    raw.wait_signalled(semaphore);
    assert_eq!(log.borrow().discarded, 2);

    assert!(
        VulkanSink::new(
            &vulkan.device,
            &vulkan.queue,
            ColorOnly(LocalStream::new(Vec::new(), semaphore, &log)),
            VulkanSinkOptions {
                depth_range: DepthRange::new(0.1, 10.0, DepthEncoding::LinearMeters).unwrap(),
                stop_after_frames: None,
            },
        )
        .is_err(),
        "linear depth cannot go into a depth attachment"
    );

    release_everything(&raw, semaphore, owned);
}

#[test]
fn each_user_renders_into_its_own_sender_and_a_leaving_user_stops_getting_frames() {
    let Some(vulkan) = create_device() else {
        return;
    };
    let raw = Raw::of(&vulkan.device);
    let mut owned = Vec::new();
    let log = Rc::new(RefCell::new(StreamLog::default()));
    let users = [UserId(1), UserId(2)];
    let mut streams = BTreeMap::new();
    let mut user_images = BTreeMap::new();
    let mut semaphores = BTreeMap::new();
    for user in users {
        let images: Vec<SenderImage> = [COLOR_FORMAT, DEPTH_FORMAT, COLOR_FORMAT, DEPTH_FORMAT]
            .into_iter()
            .map(|format| sender_image(&raw, format, &mut owned))
            .collect();
        let semaphore = binary_semaphore(&raw);
        streams.insert(user, LocalStream::new(images.clone(), semaphore, &log));
        user_images.insert(user, images);
        semaphores.insert(user, semaphore);
    }
    let mut sink = VulkanSink::new(
        &vulkan.device,
        &vulkan.queue,
        PerUserStreams {
            streams,
            leaving: Vec::new(),
        },
        VulkanSinkOptions {
            depth_range: depth_range(),
            stop_after_frames: None,
        },
    )
    .unwrap();
    let user_colors = BTreeMap::from([
        (UserId(1), [200_u8, 0, 0, 255]),
        (UserId(2), [0, 0, 200, 255]),
    ]);

    let requests = sink.begin_frames().unwrap();
    let keys: Vec<FrameKey> = requests.iter().map(|request| request.request.key).collect();
    assert_eq!(
        keys,
        users.map(|user| FrameKey { user, frame_id: 0 }),
        "one frame per user, ids per user"
    );
    for request in requests {
        let key = request.request.key;
        let commands = render_eyes(
            &vulkan.device,
            &request,
            [color_of(user_colors[&key.user]); 2],
            0.5,
        );
        drop(request);
        sink.submit_and_show(key, commands, ShowInfo::from_depth_range(depth_range()))
            .unwrap();
    }
    for user in users {
        let consumed: Vec<ConsumedImage> = user_images[&user]
            .iter()
            .map(|image| ConsumedImage {
                image: image.image,
                aspect: if image.format == wgpu::TextureFormat::Depth32Float {
                    vk::ImageAspectFlags::DEPTH
                } else {
                    vk::ImageAspectFlags::COLOR
                },
                resolution_in_pixel: RESOLUTION,
            })
            .collect();
        let copies = raw.consume(Some(semaphores[&user]), &consumed);
        for eye_index in 0..2 {
            assert!(
                copies[eye_index * 2]
                    .chunks_exact(4)
                    .all(|pixel| pixel == user_colors[&user]),
                "{user} eye {eye_index} carries its own picture"
            );
        }
    }
    assert_eq!(sink.shown_frame_count(), 2);

    sink.stream_mut().leaving.push(UserId(2));
    let request = only_request(&mut sink);
    let key = request.request.key;
    assert_eq!(
        key,
        FrameKey {
            user: UserId(1),
            frame_id: 1
        }
    );
    drop(request);
    sink.abandon_frame(key);
    raw.wait_signalled(semaphores[&UserId(1)]);
    assert!(sink.is_streaming(), "one user is left");

    sink.stream_mut().leaving.push(UserId(1));
    assert!(sink.begin_frames().unwrap().is_empty());
    assert!(
        !sink.is_streaming(),
        "nobody to render for: the viewer may rest"
    );
    assert!(!sink.is_finished(), "idle, not finished");

    drop(sink);
    for (user, semaphore) in semaphores {
        let images = if user == UserId(1) {
            std::mem::take(&mut owned)
        } else {
            Vec::new()
        };
        release_everything(&raw, semaphore, images);
    }
}
