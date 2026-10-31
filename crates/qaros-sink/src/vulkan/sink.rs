use std::collections::BTreeMap;

use frame_sink::{
    DepthEncoding, DepthRange, EYE_COUNT, FrameId, FrameKey, FrameRequest, GpuEyeTargets,
    GpuFrameRequest, GpuFrameSink, ShowInfo, SinkError, UserId, ViewRequest, perf,
};
use vulkan_device::{
    HandoverQueue, ImageAcquire, ImageRelease, SemaphoreSignal, attachment_layout, image_aspect,
    submit_sync_batch, texture_from_vk_image, vk, wait_queue_idle,
};

use super::stream::{ImageState, SenderFrame, SenderImage, VulkanFrameStream};

/// Where QAROS reads a released image from: `TRANSFER_SRC_OPTIMAL`, owned by the external
/// queue family (the cube example's release, `renderer.cpp` around L560-580).
const RELEASED_STATE: ImageState = ImageState {
    layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
    queue_family_index: vk::QUEUE_FAMILY_EXTERNAL,
};

/// Handover slots: one frame per user is in flight and shown in the viewer frame it was
/// rendered in, so eight slots cover four users over two viewer frames without waiting; with
/// more users a slot waits for its previous handover to finish.
const HANDOVER_SLOTS: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct VulkanSinkOptions {
    /// Must be a normalized encoding: a depth attachment holds values in `[0, 1]`.
    pub depth_range: DepthRange,
    /// Stop streaming after this many shown frames across all users; `None` streams until the
    /// viewer closes.
    pub stop_after_frames: Option<u64>,
}

struct OpenFrame {
    frame_id: FrameId,
    frame: SenderFrame,
}

/// A [`GpuFrameSink`] over a Vulkan frame stream on the viewer's own wgpu device, one render
/// sender per user: each frame's images are wrapped as wgpu textures (again every frame, since
/// wgpu assumes a wrapped image starts `UNDEFINED` and QAROS hands the images back in the
/// layout the last release left them in), the renderer's commands are submitted between an
/// acquire batch (from the layout and queue family the sender reports to attachment layouts)
/// and the release batch that moves the images to `TRANSFER_SRC_OPTIMAL`, releases them to
/// `VK_QUEUE_FAMILY_EXTERNAL` and signals the frame's semaphore, and the new state is written
/// back before the frame is shown. A frame that is not shown still gets its semaphore
/// signalled.
pub struct VulkanSink<Stream: VulkanFrameStream> {
    device: wgpu::Device,
    queue: wgpu::Queue,
    options: VulkanSinkOptions,
    open_frames: BTreeMap<UserId, OpenFrame>,
    /// Per user with a sender; a user admitted again starts at 0.
    next_frame_ids: BTreeMap<UserId, FrameId>,
    shown_frame_count: u64,
    handover_queue: HandoverQueue,
    /// Last, so the sender (and its images) goes after the handover batches have finished.
    stream: Stream,
}

impl<Stream: VulkanFrameStream> VulkanSink<Stream> {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        stream: Stream,
        options: VulkanSinkOptions,
    ) -> Result<Self, SinkError> {
        if options.depth_range.encoding == DepthEncoding::LinearMeters {
            return Err(SinkError::Backend(
                "the Vulkan sink renders depth into a depth attachment, which holds normalized \
                 values only; use a normalized depth encoding"
                    .to_owned(),
            ));
        }
        let handover_queue = HandoverQueue::new(device, HANDOVER_SLOTS)
            .map_err(|error| SinkError::Backend(error.to_string()))?;
        Ok(Self {
            device: device.clone(),
            queue: queue.clone(),
            options,
            open_frames: BTreeMap::new(),
            next_frame_ids: BTreeMap::new(),
            shown_frame_count: 0,
            handover_queue,
            stream,
        })
    }

    pub fn shown_frame_count(&self) -> u64 {
        self.shown_frame_count
    }

    pub fn stream_mut(&mut self) -> &mut Stream {
        &mut self.stream
    }

    pub fn stream(&self) -> &Stream {
        &self.stream
    }

    /// Signals the frame's semaphore with an empty batch and ends the frame in the stream.
    fn discard(&mut self, user: UserId, frame: &SenderFrame) {
        #[allow(unsafe_code)]
        // SAFETY: the semaphore is the frame's, of this device, binary and unsignalled: every
        // path that signals it (the release batch) ends the frame without coming here.
        unsafe {
            signal_discarded_frame(&self.device, frame.semaphore);
        }
        self.stream.discard_frame(user);
    }

    /// Follows the stream's user changes: a released user's open frame went with its sender
    /// (the stream signalled it), and a user admitted again counts frames from 0.
    fn follow_stream_users(&mut self) -> Vec<UserId> {
        let _update = perf::Span::start("sink_update_users");
        self.stream.update_users();
        let users = self.stream.users();
        self.open_frames.retain(|user, _| users.contains(user));
        self.next_frame_ids.retain(|user, _| users.contains(user));
        users
    }

    /// Begins the user's next frame and wraps its images, or `Ok(None)` when the sender has
    /// none ready.
    fn begin_user_frame(&mut self, user: UserId) -> Result<Option<GpuFrameRequest>, SinkError> {
        let try_begin_started = std::time::Instant::now();
        let begun = self.stream.try_begin_frame(user)?;
        let try_begin_micros = try_begin_started.elapsed().as_micros();
        let Some(frame) = begun else {
            perf::record("try_begin_none", Some(&user), try_begin_micros);
            return Ok(None);
        };
        perf::record("try_begin_frame", Some(&user), try_begin_micros);
        let wrap_span = perf::Span::start("wrap_targets");
        let wrapped = self.targets_for(&frame);
        drop(wrap_span);
        let eyes = match wrapped {
            Ok(eyes) => eyes,
            Err(error) => {
                self.discard(user, &frame);
                return Err(error);
            }
        };
        let resolution_in_pixel = eyes[0].color.size();
        let resolution_in_pixel = [resolution_in_pixel.width, resolution_in_pixel.height];
        if eyes
            .iter()
            .any(|eye| [eye.color.width(), eye.color.height()] != resolution_in_pixel)
        {
            self.discard(user, &frame);
            return Err(SinkError::Backend(
                "the eyes' images differ in size".to_owned(),
            ));
        }

        let next_frame_id = self.next_frame_ids.entry(user).or_insert(0);
        let frame_id = *next_frame_id;
        *next_frame_id += 1;
        let views = frame.eyes.map(|eye| ViewRequest {
            pose: eye.pose,
            fov: eye.fov,
        });
        let volume = frame.volume;
        self.open_frames.insert(user, OpenFrame { frame_id, frame });
        Ok(Some(GpuFrameRequest {
            request: FrameRequest {
                key: FrameKey { user, frame_id },
                views,
                resolution_in_pixel,
                volume,
            },
            eyes,
        }))
    }

    fn take_open_frame(&mut self, key: FrameKey) -> Option<SenderFrame> {
        let is_this_frame = self
            .open_frames
            .get(&key.user)
            .is_some_and(|open| open.frame_id == key.frame_id);
        if !is_this_frame {
            return None;
        }
        self.open_frames.remove(&key.user).map(|open| open.frame)
    }

    fn targets_for(&self, frame: &SenderFrame) -> Result<[GpuEyeTargets; EYE_COUNT], SinkError> {
        let mut eyes = Vec::with_capacity(EYE_COUNT);
        for eye in &frame.eyes {
            let color = sender_image(frame, eye.color_image)?;
            check_renderable(color, vk::FormatFeatureFlags::COLOR_ATTACHMENT, "color")?;
            let depth = match eye.depth_image {
                Some(index) => {
                    let depth = sender_image(frame, index)?;
                    check_renderable(
                        depth,
                        vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT,
                        "depth",
                    )?;
                    Some(self.wrap(depth, "qaros depth")?)
                }
                None => None,
            };
            eyes.push(GpuEyeTargets {
                color: self.wrap(color, "qaros color")?,
                depth,
            });
        }
        <[GpuEyeTargets; EYE_COUNT]>::try_from(eyes)
            .map_err(|_| SinkError::Backend("two eyes expected".to_owned()))
    }

    fn wrap(&self, image: &SenderImage, label: &str) -> Result<wgpu::Texture, SinkError> {
        let [width, height] = image.resolution_in_pixel;
        let descriptor = wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: image.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        };
        #[allow(unsafe_code)]
        // SAFETY: the sender created the image on this device (QAROS adopts the device the
        // stream was created with) as a 2D, single-mip, single-sample image of this format and
        // size whose format supports attachment use (checked above); it stays valid until the
        // sender is destroyed, which happens after this sink waited for the device. The drop
        // callback is empty because the sender owns image and memory.
        unsafe { texture_from_vk_image(&self.device, image.image, &descriptor, Box::new(|| {})) }
            .ok_or_else(|| SinkError::Backend("the viewer's device is not on Vulkan".to_owned()))
    }

    /// The images the renderer drew into, each once, in frame order.
    fn rendered_images(frame: &SenderFrame) -> Vec<usize> {
        let mut rendered: Vec<usize> = frame
            .eyes
            .iter()
            .flat_map(|eye| std::iter::once(eye.color_image).chain(eye.depth_image))
            .collect();
        rendered.sort_unstable();
        rendered.dedup();
        rendered
    }
}

/// Signals a begun frame's semaphore with an empty batch, so the sender can consume it although
/// nothing was rendered ("Discarded frames" in QAROS `qar-streaming-rust/docs/vulkan-api.md`).
///
/// # Safety
///
/// `semaphore` is a binary semaphore of `device`, unsignalled with no pending signal, and no
/// other thread submits to `device`'s queue meanwhile.
#[allow(unsafe_code)]
pub(crate) unsafe fn signal_discarded_frame(device: &wgpu::Device, semaphore: vk::Semaphore) {
    // SAFETY: as the caller guarantees.
    let signalled = unsafe {
        submit_sync_batch(
            device,
            &[],
            &[SemaphoreSignal {
                semaphore,
                value: None,
            }],
            None,
            vk::Fence::null(),
        )
    };
    if let Err(result) = signalled {
        log::error!("qaros: cannot signal the semaphore of a discarded frame: {result}");
    }
}

fn sender_image(frame: &SenderFrame, index: usize) -> Result<&SenderImage, SinkError> {
    frame.images.get(index).ok_or_else(|| {
        SinkError::Backend(format!(
            "image {index} named by an eye, but the frame has {} images",
            frame.images.len()
        ))
    })
}

fn check_renderable(
    image: &SenderImage,
    needed: vk::FormatFeatureFlags,
    kind: &str,
) -> Result<(), SinkError> {
    let is_depth_format = image.format.is_depth_stencil_format();
    let format_matches = if needed == vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT {
        is_depth_format
    } else {
        !is_depth_format
    };
    if !format_matches || !image.format_features.contains(needed) {
        return Err(SinkError::Backend(format!(
            "the {kind} image ({:?}, features {:?}) cannot be rendered into",
            image.format, image.format_features
        )));
    }
    if image.resolution_in_pixel.contains(&0) {
        return Err(SinkError::Backend(format!("the {kind} image is empty")));
    }
    Ok(())
}

impl<Stream: VulkanFrameStream> GpuFrameSink for VulkanSink<Stream> {
    fn update_users(&mut self) {
        self.follow_stream_users();
    }

    fn begin_frames(&mut self) -> Result<Vec<GpuFrameRequest>, SinkError> {
        self.stream.release_stalled_users();
        let users = self.follow_stream_users();
        if self.is_finished() {
            return Ok(Vec::new());
        }
        let mut requests = Vec::with_capacity(users.len());
        for user in users {
            if self.open_frames.contains_key(&user) {
                continue;
            }
            match self.begin_user_frame(user) {
                Ok(Some(request)) => requests.push(request),
                Ok(None) => {}
                Err(error) => self.stream.begin_failed(user, &error),
            }
        }
        Ok(requests)
    }

    fn submit_and_show(
        &mut self,
        key: FrameKey,
        commands: wgpu::CommandBuffer,
        show: ShowInfo,
    ) -> Result<(), SinkError> {
        let frame = self
            .take_open_frame(key)
            .ok_or(SinkError::UnknownFrame(key))?;
        let rendered = Self::rendered_images(&frame);
        let acquires: Vec<ImageAcquire> = rendered
            .iter()
            .map(|&index| {
                let image = &frame.images[index];
                ImageAcquire {
                    image: image.image,
                    aspect: image_aspect(image.format),
                    old_layout: image.layout,
                    new_layout: attachment_layout(image.format),
                    source_queue_family: image.queue_family_index,
                }
            })
            .collect();
        let releases: Vec<ImageRelease> = rendered
            .iter()
            .map(|&index| {
                let image = &frame.images[index];
                ImageRelease {
                    image: image.image,
                    aspect: image_aspect(image.format),
                    old_layout: attachment_layout(image.format),
                    new_layout: RELEASED_STATE.layout,
                    destination_queue_family: RELEASED_STATE.queue_family_index,
                }
            })
            .collect();

        let handover_span = perf::Span::start("handover_submit");
        #[allow(unsafe_code)]
        // SAFETY: the images are in the layout and queue family the sender reported, and
        // `commands` is the renderer's last use of them, as attachments (the `GpuFrameSink`
        // contract); the images and the binary semaphore belong to this device, and the
        // semaphore has not been signalled for this frame. Submissions happen on the thread
        // that renders, the only one submitting to wgpu's queue.
        let handover_outcome = unsafe {
            self.handover_queue.submit_handover(
                &self.queue,
                &acquires,
                vec![commands],
                &releases,
                &[SemaphoreSignal {
                    semaphore: frame.semaphore,
                    value: None,
                }],
            )
        };
        drop(handover_span);
        let submission = match handover_outcome {
            Ok(submission) => submission,
            Err(error) => {
                self.discard(key.user, &frame);
                return Err(SinkError::Backend(format!("frame handover: {error}")));
            }
        };
        if perf::gpu_sync() {
            let _gpu = perf::Span::start("gpu_wait_output");
            if let Err(error) = self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            }) {
                log::warn!("perf: waiting for the output pass: {error}");
            }
        }

        let states: Vec<ImageState> = frame
            .images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                if rendered.contains(&index) {
                    RELEASED_STATE
                } else {
                    ImageState {
                        layout: image.layout,
                        queue_family_index: image.queue_family_index,
                    }
                }
            })
            .collect();
        perf::time_for_user("qaros_show_frame", &key.user, || {
            self.stream.show_frame(key.user, &states, show)
        })?;
        self.shown_frame_count += 1;
        Ok(())
    }

    fn abandon_frame(&mut self, key: FrameKey) {
        if let Some(frame) = self.take_open_frame(key) {
            self.discard(key.user, &frame);
        }
    }

    fn is_streaming(&self) -> bool {
        !self.is_finished() && !self.stream.users().is_empty()
    }

    fn is_finished(&self) -> bool {
        self.options
            .stop_after_frames
            .is_some_and(|limit| self.shown_frame_count >= limit)
    }

    fn depth_range(&self) -> DepthRange {
        self.options.depth_range
    }

    fn submits_outside_wgpu(&self) -> bool {
        true
    }
}

impl<Stream: VulkanFrameStream> Drop for VulkanSink<Stream> {
    fn drop(&mut self) {
        for (user, open) in std::mem::take(&mut self.open_frames) {
            self.discard(user, &open.frame);
        }
        // The senders destroy their images and semaphores when the stream goes.
        #[allow(unsafe_code)]
        // SAFETY: this is the thread that submits to the device's queue.
        unsafe {
            wait_for_device(&self.device);
        }
    }
}

/// Waits until the device is done with everything submitted so far: wgpu's own work and the
/// raw batches beside it (release batches, discard signals), which `wgpu::Device::poll` does
/// not cover. A sender's images and semaphore may go only after this.
///
/// # Safety
///
/// No other thread submits to `device`'s queue meanwhile.
#[allow(unsafe_code)]
pub(crate) unsafe fn wait_for_device(device: &wgpu::Device) {
    if let Err(error) = device.poll(wgpu::PollType::wait_indefinitely()) {
        log::error!("qaros: waiting for wgpu's work before a sender goes: {error}");
    }
    // SAFETY: as the caller guarantees.
    if let Err(result) = unsafe { wait_queue_idle(device) } {
        log::error!("qaros: waiting for the queue before a sender goes: {result}");
    }
}
