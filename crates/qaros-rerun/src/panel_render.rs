//! The headless runner's off-screen UI render for panel capture: `egui_kittest`'s
//! `WgpuTestRenderer` (0.37.0) with only its two queue submits under
//! `re_renderer::queue_submission_lock`. Holding the lock across the whole render, with its
//! tessellation and two waits for the GPU, held the render thread of `--decoupled-render` at
//! its next sink call for a whole window readback every capture.

use std::sync::mpsc;
use std::time::Duration;

use egui::TexturesDelta;
use egui_wgpu::{RenderState, ScreenDescriptor, WgpuSetup};
use image::RgbaImage;
use rerun::external::eframe;

/// As `egui_kittest`'s `WAIT_TIMEOUT`: a lost device fails the render instead of hanging it.
const GPU_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

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

    fn submit(
        &self,
        command_buffers: impl IntoIterator<Item = wgpu::CommandBuffer>,
    ) -> wgpu::SubmissionIndex {
        let _submission_lock = re_renderer::queue_submission_lock();
        self.render_state.queue.submit(command_buffers)
    }

    fn wait_for(&self, submission: wgpu::SubmissionIndex) -> Result<(), String> {
        self.render_state
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(GPU_WAIT_TIMEOUT),
            })
            .map(|_| ())
            .map_err(|error| format!("waiting for the GPU: {error}"))
    }

    fn read_back(&self, texture: &wgpu::Texture) -> Result<RgbaImage, String> {
        let device = &self.render_state.device;
        let unpadded_row = texture.width() as usize * 4;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
        let padded_row = unpadded_row.div_ceil(alignment) * alignment;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("panel capture readback"),
            size: (padded_row * texture.height() as usize) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("panel capture readback"),
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(
                        u32::try_from(padded_row).map_err(|error| error.to_string())?,
                    ),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        let submission = self.submit([encoder.finish()]);
        let (mapped_sender, mapped_receiver) = mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |mapped| {
                drop(mapped_sender.send(mapped));
            });
        self.wait_for(submission)?;
        mapped_receiver
            .recv()
            .map_err(|error| format!("mapping the readback: {error}"))?
            .map_err(|error| format!("mapping the readback: {error}"))?;
        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|error| format!("mapping the readback: {error}"))?;
        let pixels: Vec<u8> = mapped
            .chunks_exact(padded_row)
            .flat_map(|row| row.iter().take(unpadded_row))
            .copied()
            .collect();
        RgbaImage::from_raw(texture.width(), texture.height(), pixels)
            .ok_or_else(|| "the readback does not fit the window size".to_owned())
    }
}

impl egui_kittest::TestRenderer for PanelRenderer {
    fn setup_eframe(&self, cc: &mut eframe::CreationContext<'_>, frame: &mut eframe::Frame) {
        cc.wgpu_render_state = Some(self.render_state.clone());
        frame.wgpu_render_state = Some(self.render_state.clone());
    }

    fn handle_delta(&mut self, delta: &mut TexturesDelta) {
        let mut renderer = self.render_state.renderer.write();
        for (id, images) in delta.set.drain() {
            for image in images {
                renderer.update_texture(
                    &self.render_state.device,
                    &self.render_state.queue,
                    id,
                    &image,
                );
            }
        }
        for id in delta.free.drain() {
            renderer.free_texture(&id);
        }
    }

    fn render(
        &mut self,
        ctx: &egui::Context,
        output: &egui::FullOutput,
    ) -> Result<RgbaImage, String> {
        let device = &self.render_state.device;
        let mut renderer = self.render_state.renderer.write();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("panel capture"),
        });
        let size = ctx.content_rect().size() * ctx.pixels_per_point();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a window size in pixels, rounded"
        )]
        let screen = ScreenDescriptor {
            pixels_per_point: ctx.pixels_per_point(),
            size_in_pixels: [size.x.round() as u32, size.y.round() as u32],
        };
        let tessellated = ctx.tessellate(output.shapes.clone(), ctx.pixels_per_point());
        let user_buffers = renderer.update_buffers(
            device,
            &self.render_state.queue,
            &mut encoder,
            &tessellated,
            &screen,
        );
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("panel capture"),
            size: wgpu::Extent3d {
                width: screen.size_in_pixels[0],
                height: screen.size_in_pixels[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.render_state.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("panel capture"),
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
            renderer.render(&mut pass, &tessellated, &screen);
        }
        drop(renderer);
        let submission = self.submit(user_buffers.into_iter().chain([encoder.finish()]));
        self.wait_for(submission)?;
        self.read_back(&texture)
    }
}
