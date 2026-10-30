//! Off-screen egui renders, read back to the CPU.
//!
//! - [`PanelRenderer`] is the headless runner's `egui_kittest` renderer for panel capture: it
//!   draws the harness's own context with eframe's `egui_wgpu::Renderer`, as `egui_kittest`'s
//!   `WgpuTestRenderer` (0.37.0) does.
//! - [`OffscreenEguiRenderer`] draws a private context, the timeline panel's, with an
//!   `egui_wgpu::Renderer` of its own on the viewer's device. Every `egui::Context` numbers its
//!   managed textures from the font atlas at `TextureId::Managed(0)`, so a second context fed
//!   into eframe's renderer would replace the desktop UI's fonts and icons. Its constructor
//!   takes only the device, the queue and the target format, so it cannot reach eframe's.
//!
//! Both hold `re_renderer::queue_submission_lock` only around the one submit: holding it across
//! the whole render, with its tessellation and the wait for the GPU, would hold the render
//! thread of `--decoupled-render` at its next sink call for a whole readback.

use std::sync::mpsc;
use std::time::Duration;

use egui::TexturesDelta;
use egui_wgpu::{RenderState, ScreenDescriptor, WgpuSetup};
use image::RgbaImage;
use rerun::external::eframe;

/// As `egui_kittest`'s `WAIT_TIMEOUT`: a lost device fails the render instead of hanging it.
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

const BYTES_PER_PIXEL: usize = 4;

/// The commands that draw one egui frame into a fresh texture, not yet submitted.
struct EncodedFrame {
    command_buffers: Vec<wgpu::CommandBuffer>,
    texture: wgpu::Texture,
}

fn apply_textures_delta(
    renderer: &mut egui_wgpu::Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    delta: &mut TexturesDelta,
) {
    for (id, images) in delta.set.drain() {
        for image in images {
            renderer.update_texture(device, queue, id, &image);
        }
    }
    for id in delta.free.drain() {
        renderer.free_texture(&id);
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a window size in pixels, rounded"
)]
fn screen_of(ctx: &egui::Context) -> ScreenDescriptor {
    let size = ctx.content_rect().size() * ctx.pixels_per_point();
    ScreenDescriptor {
        pixels_per_point: ctx.pixels_per_point(),
        size_in_pixels: [size.x.round() as u32, size.y.round() as u32],
    }
}

fn encode_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut egui_wgpu::Renderer,
    format: wgpu::TextureFormat,
    tessellated: &[egui::ClippedPrimitive],
    screen: &ScreenDescriptor,
) -> EncodedFrame {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("off-screen egui"),
    });
    let user_buffers = renderer.update_buffers(device, queue, &mut encoder, tessellated, screen);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("off-screen egui"),
        size: wgpu::Extent3d {
            width: screen.size_in_pixels[0],
            height: screen.size_in_pixels[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("off-screen egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &texture_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            })
            .forget_lifetime();
        renderer.render(&mut pass, tessellated, screen);
    }
    EncodedFrame {
        command_buffers: user_buffers.into_iter().chain([encoder.finish()]).collect(),
        texture,
    }
}

fn submit(
    queue: &wgpu::Queue,
    command_buffers: impl IntoIterator<Item = wgpu::CommandBuffer>,
) -> wgpu::SubmissionIndex {
    let _submission_lock = re_renderer::queue_submission_lock();
    queue.submit(command_buffers)
}

fn wait_for(device: &wgpu::Device, submission: wgpu::SubmissionIndex) -> Result<(), String> {
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(GPU_WAIT_TIMEOUT),
        })
        .map(|_| ())
        .map_err(|error| format!("waiting for the GPU: {error}"))
}

fn is_bgra(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    )
}

/// A frame's draw and its copy into a mappable buffer, submitted together, with the map
/// requested; the pixels can be read once the map callback has run.
struct PendingReadback {
    buffer: wgpu::Buffer,
    mapped: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    submission: wgpu::SubmissionIndex,
    width: u32,
    height: u32,
    padded_row: usize,
    is_bgra: bool,
}

/// Submits the frame's draw and a copy of its texture into a mappable buffer in one submit,
/// and requests the map; nothing here waits for the GPU.
fn submit_with_readback(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: EncodedFrame,
) -> Result<PendingReadback, String> {
    let texture = &frame.texture;
    let unpadded_row = texture.width() as usize * BYTES_PER_PIXEL;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded_row = unpadded_row.div_ceil(alignment) * alignment;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("off-screen egui readback"),
        size: (padded_row * texture.height() as usize) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("off-screen egui readback"),
    });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u32::try_from(padded_row).map_err(|error| error.to_string())?),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    let submission = submit(
        queue,
        frame.command_buffers.into_iter().chain([encoder.finish()]),
    );
    let (mapped_sender, mapped) = mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            drop(mapped_sender.send(result));
        });
    Ok(PendingReadback {
        buffer,
        mapped,
        submission,
        width: texture.width(),
        height: texture.height(),
        padded_row,
        is_bgra: is_bgra(texture.format()),
    })
}

impl PendingReadback {
    /// Waits for the GPU, then reads the pixels.
    fn wait_and_read(self, device: &wgpu::Device) -> Result<RgbaImage, String> {
        wait_for(device, self.submission.clone())?;
        let mapped = self
            .mapped
            .recv()
            .map_err(|error| format!("mapping the readback: {error}"))?;
        self.read(mapped)
    }

    /// The pixels if the map has completed, without waiting; `None` while the GPU is busy.
    fn try_read(&self, device: &wgpu::Device) -> Option<Result<RgbaImage, String>> {
        if let Err(error) = device.poll(wgpu::PollType::Poll) {
            return Some(Err(format!("polling the GPU: {error}")));
        }
        match self.mapped.try_recv() {
            Ok(mapped) => Some(self.read(mapped)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("mapping the readback: the map was dropped".to_owned()))
            }
        }
    }

    /// Reads the mapped buffer as RGBA, whatever the 8-bit target order.
    fn read(&self, mapped: Result<(), wgpu::BufferAsyncError>) -> Result<RgbaImage, String> {
        mapped.map_err(|error| format!("mapping the readback: {error}"))?;
        let unpadded_row = self.width as usize * BYTES_PER_PIXEL;
        let mut pixels: Vec<u8> = {
            let mapped_range = self
                .buffer
                .slice(..)
                .get_mapped_range()
                .map_err(|error| format!("mapping the readback: {error}"))?;
            mapped_range
                .chunks_exact(self.padded_row)
                .flat_map(|row| row.iter().take(unpadded_row))
                .copied()
                .collect()
        };
        self.buffer.unmap();
        if self.is_bgra {
            for pixel in pixels.chunks_exact_mut(BYTES_PER_PIXEL) {
                pixel.swap(0, 2);
            }
        }
        RgbaImage::from_raw(self.width, self.height, pixels)
            .ok_or_else(|| "the readback does not fit the window size".to_owned())
    }
}

/// Renders a private egui context off screen on a device it shares, with a renderer of its
/// own (see the module docs for why it never takes eframe's). A render is submitted in one
/// call and collected in a later one, so the caller never waits for the GPU: the timeline
/// panel renders on the UI thread, which also renders the eye frames.
#[cfg_attr(
    not(feature = "qaros"),
    allow(
        dead_code,
        reason = "only the stream route's timeline panel draws with it"
    )
)]
pub struct OffscreenEguiRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    renderer: egui_wgpu::Renderer,
    in_flight: Option<PendingReadback>,
}

#[cfg_attr(
    any(not(test), not(feature = "qaros")),
    allow(
        dead_code,
        reason = "only the stream route's timeline panel draws with it, once viewer_app runs it"
    )
)]
impl OffscreenEguiRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        Self {
            device: device.clone(),
            queue: queue.clone(),
            format,
            renderer: egui_wgpu::Renderer::new(
                device,
                format,
                egui_wgpu::RendererOptions::PREDICTABLE,
            ),
            in_flight: None,
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Whether a submitted render has not been collected yet; only one is in flight.
    pub fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Draws one pass's output of `ctx` at the context's content size and submits it with
    /// its readback; [`Self::try_collect`] returns the image once the GPU is done.
    pub fn submit(
        &mut self,
        ctx: &egui::Context,
        mut output: egui::FullOutput,
    ) -> Result<(), String> {
        if self.in_flight.is_some() {
            return Err("a render is still in flight".to_owned());
        }
        let mut textures_to_free = TexturesDelta::default();
        textures_to_free.free = std::mem::take(&mut output.textures_delta.free);
        apply_textures_delta(
            &mut self.renderer,
            &self.device,
            &self.queue,
            &mut output.textures_delta,
        );
        let screen = screen_of(ctx);
        let tessellated = ctx.tessellate(output.shapes, ctx.pixels_per_point());
        let frame = encode_frame(
            &self.device,
            &self.queue,
            &mut self.renderer,
            self.format,
            &tessellated,
            &screen,
        );
        let submitted = submit_with_readback(&self.device, &self.queue, frame);
        apply_textures_delta(
            &mut self.renderer,
            &self.device,
            &self.queue,
            &mut textures_to_free,
        );
        self.in_flight = Some(submitted?);
        Ok(())
    }

    /// The submitted render's image once the GPU has finished it, without waiting; `None`
    /// while nothing is in flight or the GPU is still busy.
    pub fn try_collect(&mut self) -> Option<Result<RgbaImage, String>> {
        let collected = self.in_flight.as_ref()?.try_read(&self.device)?;
        self.in_flight = None;
        Some(collected)
    }

    /// Waits for the submitted render; for tests, which have no later frame to collect in.
    #[cfg(test)]
    pub fn wait_and_collect(&mut self) -> Option<Result<RgbaImage, String>> {
        let in_flight = self.in_flight.take()?;
        Some(in_flight.wait_and_read(&self.device))
    }
}

/// The headless harness's renderer, over eframe's `egui_wgpu::Renderer`, which the harness's
/// context is the only one to feed.
pub struct PanelRenderer {
    render_state: RenderState,
}

impl PanelRenderer {
    pub fn new(setup: WgpuSetup) -> Self {
        Self {
            render_state: egui_kittest::wgpu::create_render_state(
                setup,
                egui_wgpu::RendererOptions::PREDICTABLE,
            ),
        }
    }
}

impl egui_kittest::TestRenderer for PanelRenderer {
    fn setup_eframe(&self, cc: &mut eframe::CreationContext<'_>, frame: &mut eframe::Frame) {
        cc.wgpu_render_state = Some(self.render_state.clone());
        frame.wgpu_render_state = Some(self.render_state.clone());
    }

    fn handle_delta(&mut self, delta: &mut TexturesDelta) {
        apply_textures_delta(
            &mut self.render_state.renderer.write(),
            &self.render_state.device,
            &self.render_state.queue,
            delta,
        );
    }

    fn render(
        &mut self,
        ctx: &egui::Context,
        output: &egui::FullOutput,
    ) -> Result<RgbaImage, String> {
        let screen = screen_of(ctx);
        let tessellated = ctx.tessellate(output.shapes.clone(), ctx.pixels_per_point());
        let frame = encode_frame(
            &self.render_state.device,
            &self.render_state.queue,
            &mut self.render_state.renderer.write(),
            self.render_state.target_format,
            &tessellated,
            &screen,
        );
        submit_with_readback(&self.render_state.device, &self.render_state.queue, frame)?
            .wait_and_read(&self.render_state.device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_8_bit_bgra_targets_are_swizzled_on_readback() {
        assert!(is_bgra(wgpu::TextureFormat::Bgra8Unorm));
        assert!(is_bgra(wgpu::TextureFormat::Bgra8UnormSrgb));
        assert!(!is_bgra(wgpu::TextureFormat::Rgba8Unorm));
        assert!(!is_bgra(wgpu::TextureFormat::Rgba8UnormSrgb));
    }
}
