//! The GPU path's last pass per eye: Rerun's composited eye and the picking layer's depth go
//! into the sink's own textures, without leaving the GPU.
//!
//! `re_renderer` 0.40.0 composites a view only into a target of its output format
//! (`ViewBuilder::composite`, the window's format), so an eye is first composited into a
//! texture of that format. Its depth is the picking layer's depth target, kept on the GPU by
//! the patched `re_renderer` (`ViewBuilder::new_with_picking_depth_on_gpu`): the same
//! single-sampled reverse-Z depth the CPU path reads back. This pass then writes both into the
//! sink's targets (any color format, a depth format through `frag_depth`), converting depth into
//! the sink's encoding and clearing uncovered pixels to transparent. With `--frame-stamp`, a
//! small stamped image lies over the top rows of the eye in the same pass.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use frame_sink::{EyeImage, GpuEyeTargets, UserId};
use wgpu::util::DeviceExt as _;
use xr_math::{DepthEncoding, DepthRange};

const SHADER: &str = include_str!("eye_output.wgsl");

const STAMP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The frame stamp for one eye: `image` is as wide as the eye and covers its top rows, stamped
/// where alpha is set and transparent elsewhere (`frame_sink::stamp_eye` on a transparent image
/// of `frame_sink::frame_stamp::stamp_rows` rows). Stamped pixels get `reverse_depth`, in
/// Rerun's reverse-Z like the picking depth.
#[derive(Clone, Copy, Debug)]
pub struct StampOverlay<'image> {
    pub image: &'image EyeImage,
    pub reverse_depth: f32,
}

/// Rerun's composited eye, made by [`composite_target`] and filled by `ViewBuilder::composite`.
pub fn composite_target(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    resolution_in_pixel: [u32; 2],
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headset composited eye"),
        size: extent(resolution_in_pixel),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// How long a kept eye resource (a composite target, a uniform buffer) may go unused before it
/// is released: a user that left, a resolution that changed, the last frame before nobody
/// streams. Measured in time rather than viewer frames, since an idle viewer repaints only
/// about twice a second.
pub const UNUSED_EYE_RESOURCE_LIFETIME: Duration = Duration::from_secs(10);

fn is_unused_too_long(last_used: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_used) > UNUSED_EYE_RESOURCE_LIFETIME
}

struct KeptCompositeTarget {
    texture: wgpu::Texture,
    last_used: Instant,
}

/// One composite target per user and eye, reused by every frame of that user instead of a new
/// texture per eye frame. Reuse is safe because a user's next frame begins only after its last
/// one left the renderer, and wgpu orders the GPU's use of the texture across submissions on
/// the one queue.
#[derive(Default)]
pub struct KeptCompositeTargets {
    targets: HashMap<(UserId, usize), KeptCompositeTarget>,
    created: u64,
}

impl KeptCompositeTargets {
    pub fn target_for(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        resolution_in_pixel: [u32; 2],
        user_and_eye: (UserId, usize),
        now: Instant,
    ) -> wgpu::Texture {
        self.release_unused(now);
        if let Some(kept_target) = self.targets.get_mut(&user_and_eye)
            && kept_target.texture.format() == format
            && [kept_target.texture.width(), kept_target.texture.height()] == resolution_in_pixel
        {
            kept_target.last_used = now;
            return kept_target.texture.clone();
        }
        let texture = composite_target(device, format, resolution_in_pixel);
        self.created += 1;
        self.targets.insert(
            user_and_eye,
            KeptCompositeTarget {
                texture: texture.clone(),
                last_used: now,
            },
        );
        texture
    }

    /// Drops the targets no frame used for [`UNUSED_EYE_RESOURCE_LIFETIME`]; called every
    /// viewer frame, so once nobody streams they are gone within that lifetime.
    pub fn release_unused(&mut self, now: Instant) {
        self.targets
            .retain(|_, kept_target| !is_unused_too_long(kept_target.last_used, now));
    }
}

/// The composite target of one eye: the kept one of that user and eye when targets are kept,
/// else a new texture.
pub fn composite_target_for_eye(
    kept_targets: Option<&mut KeptCompositeTargets>,
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    resolution_in_pixel: [u32; 2],
    user_and_eye: (UserId, usize),
    now: Instant,
) -> wgpu::Texture {
    match kept_targets {
        Some(kept_targets) => {
            kept_targets.target_for(device, format, resolution_in_pixel, user_and_eye, now)
        }
        None => composite_target(device, format, resolution_in_pixel),
    }
}

fn stamp_texture(device: &wgpu::Device, resolution_in_pixel: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headset frame stamp"),
        size: extent(resolution_in_pixel),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: STAMP_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// A texture holding `stamp`, filled by a copy recorded into `encoder` ahead of the pass.
fn upload_stamp(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    stamp: &EyeImage,
    eye_width: u32,
) -> Result<wgpu::Texture, String> {
    let [width, rows] = stamp.resolution_in_pixel;
    if width != eye_width || rows == 0 {
        return Err(format!(
            "the stamp is {width}x{rows}, the eye {eye_width} pixels wide"
        ));
    }
    let row_bytes = width * 4;
    let padded_row_bytes = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let mut padded = vec![0_u8; padded_row_bytes as usize * rows as usize];
    for (padded_row, row) in padded
        .chunks_exact_mut(padded_row_bytes as usize)
        .zip(stamp.color_rgba8.chunks_exact(row_bytes as usize))
    {
        padded_row[..row.len()].copy_from_slice(row);
    }
    let staging = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("headset frame stamp"),
        contents: &padded,
        usage: wgpu::BufferUsages::COPY_SRC,
    });
    let texture = stamp_texture(device, stamp.resolution_in_pixel);
    encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row_bytes),
                rows_per_image: Some(rows),
            },
        },
        texture.as_image_copy(),
        extent(stamp.resolution_in_pixel),
    );
    Ok(texture)
}

fn extent(resolution_in_pixel: [u32; 2]) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: resolution_in_pixel[0],
        height: resolution_in_pixel[1],
        depth_or_array_layers: 1,
    }
}

/// How the pass turns Rerun's reverse-Z depth into the sink's depth and coverage.
#[derive(Clone, Copy, Debug)]
pub struct DepthOutput {
    pub depth_range: DepthRange,
    /// Clear pixels where nothing was drawn to transparent.
    pub transparent_background: bool,
}

impl DepthOutput {
    /// The shader's `DepthOutput` uniform. Linear depth cannot go through `frag_depth` (a depth
    /// attachment holds `[0, 1]`), so it is refused for a sink with a depth target.
    fn uniform_bytes(
        self,
        has_depth_target: bool,
        source_per_target: [f32; 2],
        stamp: Option<StampOverlay<'_>>,
    ) -> Result<[u8; 32], String> {
        let encoding: u32 = match self.depth_range.encoding {
            DepthEncoding::LinearMeters if has_depth_target => {
                return Err(
                    "linear depth cannot be written into a depth target; use forward or reverse"
                        .to_owned(),
                );
            }
            DepthEncoding::ReverseNormalized => 1,
            // Without a depth target the shader never reads the encoding.
            DepthEncoding::ForwardNormalized | DepthEncoding::LinearMeters => 0,
        };
        let covers_everything = u32::from(!self.transparent_background);
        let mut bytes = [0_u8; 32];
        bytes[0..4].copy_from_slice(&self.depth_range.near().to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.depth_range.far().to_ne_bytes());
        bytes[8..12].copy_from_slice(&encoding.to_ne_bytes());
        bytes[12..16].copy_from_slice(&covers_everything.to_ne_bytes());
        bytes[16..20].copy_from_slice(&source_per_target[0].to_ne_bytes());
        bytes[20..24].copy_from_slice(&source_per_target[1].to_ne_bytes());
        let (stamp_rows, stamp_reverse_depth) = stamp.map_or((0, 0.0), |stamp| {
            (stamp.image.resolution_in_pixel[1], stamp.reverse_depth)
        });
        bytes[24..28].copy_from_slice(&stamp_rows.to_ne_bytes());
        bytes[28..32].copy_from_slice(&stamp_reverse_depth.to_ne_bytes());
        Ok(bytes)
    }
}

/// Eye pixels per depth target pixel in each dimension.
#[allow(clippy::cast_precision_loss)]
fn source_per_target(eye: &wgpu::Texture, depth_target: &wgpu::Texture) -> [f32; 2] {
    [
        eye.width() as f32 / depth_target.width() as f32,
        eye.height() as f32 / depth_target.height() as f32,
    ]
}

/// One kind of pass, with its target formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum OutputPass {
    /// Color, and depth of the same size, pixel for pixel.
    ColorAndDepth {
        color: wgpu::TextureFormat,
        depth: wgpu::TextureFormat,
    },
    Color(wgpu::TextureFormat),
    /// Depth of another size than color, resampled to the nearest depth per footprint.
    ResampledDepth(wgpu::TextureFormat),
}

impl OutputPass {
    fn fragment_entry_point(self) -> &'static str {
        match self {
            Self::ColorAndDepth { .. } => "fragment_color_and_depth",
            Self::Color(_) => "fragment_color",
            Self::ResampledDepth(_) => "fragment_resampled_depth",
        }
    }

    fn color_format(self) -> Option<wgpu::TextureFormat> {
        match self {
            Self::ColorAndDepth { color, .. } | Self::Color(color) => Some(color),
            Self::ResampledDepth(_) => None,
        }
    }

    fn depth_format(self) -> Option<wgpu::TextureFormat> {
        match self {
            Self::ColorAndDepth { depth, .. } | Self::ResampledDepth(depth) => Some(depth),
            Self::Color(_) => None,
        }
    }
}

/// Uniform buffers kept by content, at most this many.
const KEPT_UNIFORM_BUFFERS: usize = 8;

/// The pass's uniform buffers by content. `create_buffer_init` stages through a new
/// host-visible buffer each time; on Windows that staging can need a new gpu-allocator block,
/// created and freed again for every eye frame. The uniform repeats from frame to frame (near,
/// far, encoding and sizes stay), so a kept buffer serves every frame after the first. A kept
/// buffer is never written again, so frames still in flight read what they were given. A buffer
/// unused for [`UNUSED_EYE_RESOURCE_LIFETIME`] is released.
#[derive(Default)]
struct KeptUniformBuffers {
    /// Oldest first.
    buffers: Vec<KeptUniformBuffer>,
    created: u64,
}

struct KeptUniformBuffer {
    uniform_bytes: [u8; 32],
    buffer: wgpu::Buffer,
    last_used: Instant,
}

impl KeptUniformBuffers {
    fn buffer_for(
        &mut self,
        device: &wgpu::Device,
        uniform_bytes: [u8; 32],
        now: Instant,
    ) -> wgpu::Buffer {
        self.release_unused(now);
        if let Some(kept_buffer) = self
            .buffers
            .iter_mut()
            .find(|kept_buffer| kept_buffer.uniform_bytes == uniform_bytes)
        {
            kept_buffer.last_used = now;
            return kept_buffer.buffer.clone();
        }
        let buffer = create_uniform_buffer(device, &uniform_bytes);
        self.created += 1;
        if self.buffers.len() == KEPT_UNIFORM_BUFFERS {
            self.buffers.remove(0);
        }
        self.buffers.push(KeptUniformBuffer {
            uniform_bytes,
            buffer: buffer.clone(),
            last_used: now,
        });
        buffer
    }

    fn release_unused(&mut self, now: Instant) {
        self.buffers
            .retain(|kept_buffer| !is_unused_too_long(kept_buffer.last_used, now));
    }
}

fn create_uniform_buffer(device: &wgpu::Device, uniform_bytes: &[u8; 32]) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("headset eye output"),
        contents: uniform_bytes,
        usage: wgpu::BufferUsages::UNIFORM,
    })
}

/// The pipelines for each kind of pass and target formats met so far.
pub struct EyeOutput {
    shader: wgpu::ShaderModule,
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: HashMap<OutputPass, wgpu::RenderPipeline>,
    /// Bound when there is no stamp; the shader reads no texel of it.
    no_stamp: wgpu::Texture,
    /// `None` creates a uniform buffer for every eye (`QAROS_RERUN_EYE_REUSE=0`).
    kept_uniform_buffers: Option<KeptUniformBuffers>,
}

impl EyeOutput {
    /// `keep_uniform_buffers` reuses uniform buffers of the same content across frames.
    pub fn new(device: &wgpu::Device, keep_uniform_buffers: bool) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("headset eye output"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let fragment_texture = |binding, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("headset eye output"),
            entries: &[
                fragment_texture(0, wgpu::TextureSampleType::Float { filterable: false }),
                fragment_texture(1, wgpu::TextureSampleType::Depth),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                fragment_texture(3, wgpu::TextureSampleType::Float { filterable: false }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("headset eye output"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        Self {
            shader,
            bind_group_layout,
            pipeline_layout,
            pipelines: HashMap::new(),
            no_stamp: stamp_texture(device, [1, 1]),
            kept_uniform_buffers: keep_uniform_buffers.then(KeptUniformBuffers::default),
        }
    }

    /// Uniform buffers created so far while they are kept; `None` when they are not.
    #[cfg(test)]
    fn uniform_buffers_created(&self) -> Option<u64> {
        self.kept_uniform_buffers
            .as_ref()
            .map(|kept_buffers| kept_buffers.created)
    }

    fn uniform_buffer(&mut self, device: &wgpu::Device, uniform_bytes: [u8; 32]) -> wgpu::Buffer {
        match &mut self.kept_uniform_buffers {
            Some(kept_buffers) => kept_buffers.buffer_for(device, uniform_bytes, Instant::now()),
            None => create_uniform_buffer(device, &uniform_bytes),
        }
    }

    /// Drops the kept uniform buffers no frame used for [`UNUSED_EYE_RESOURCE_LIFETIME`];
    /// called every viewer frame, like [`KeptCompositeTargets::release_unused`].
    pub fn release_unused(&mut self, now: Instant) {
        if let Some(kept_buffers) = &mut self.kept_uniform_buffers {
            kept_buffers.release_unused(now);
        }
    }

    /// Records the passes that write one eye into its targets. Every target pixel is written.
    /// `reverse_depth` is a sampleable `Depth32Float` texture at the eye's resolution. A depth
    /// target of the color's size is written in the color's pass; one of another size in a pass
    /// of its own, resampled. `stamp` lies over the top rows of the eye.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        composited_color: &wgpu::Texture,
        reverse_depth: &wgpu::Texture,
        depth_output: DepthOutput,
        stamp: Option<StampOverlay<'_>>,
        targets: &GpuEyeTargets,
    ) -> Result<(), String> {
        let color_format = targets.color.format();
        if color_format.is_srgb() {
            return Err(format!(
                "the sink's color format {color_format:?} is sRGB; Rerun's composited color is \
                 already sRGB-encoded"
            ));
        }
        let depth_target = targets.depth.as_ref();
        let source_per_target =
            depth_target.map_or([1.0, 1.0], |depth| source_per_target(&targets.color, depth));
        let uniform_bytes =
            depth_output.uniform_bytes(depth_target.is_some(), source_per_target, stamp)?;
        let stamp_texture = match stamp {
            Some(stamp) => upload_stamp(device, encoder, stamp.image, targets.color.width())?,
            None => self.no_stamp.clone(),
        };
        let uniform_buffer = self.uniform_buffer(device, uniform_bytes);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("headset eye output"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &composited_color.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &reverse_depth.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(
                        &stamp_texture.create_view(&wgpu::TextureViewDescriptor::default()),
                    ),
                },
            ],
        });
        let color_view = targets
            .color
            .create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view =
            depth_target.map(|depth| depth.create_view(&wgpu::TextureViewDescriptor::default()));
        match (depth_target, &depth_view) {
            (Some(depth), Some(depth_view)) if depth.size() == targets.color.size() => {
                let pass = OutputPass::ColorAndDepth {
                    color: color_format,
                    depth: depth.format(),
                };
                self.record(
                    device,
                    encoder,
                    pass,
                    Some(&color_view),
                    Some(depth_view),
                    &bind_group,
                );
            }
            (Some(depth), Some(depth_view)) => {
                let color_pass = OutputPass::Color(color_format);
                self.record(
                    device,
                    encoder,
                    color_pass,
                    Some(&color_view),
                    None,
                    &bind_group,
                );
                let depth_pass = OutputPass::ResampledDepth(depth.format());
                self.record(
                    device,
                    encoder,
                    depth_pass,
                    None,
                    Some(depth_view),
                    &bind_group,
                );
            }
            _ => {
                let pass = OutputPass::Color(color_format);
                self.record(device, encoder, pass, Some(&color_view), None, &bind_group);
            }
        }
        Ok(())
    }

    fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pass_kind: OutputPass,
        color_view: Option<&wgpu::TextureView>,
        depth_view: Option<&wgpu::TextureView>,
        bind_group: &wgpu::BindGroup,
    ) {
        let pipeline = self.pipeline(device, pass_kind).clone();
        let color_attachments = [color_view.map(|view| wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })];
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("headset eye output"),
            color_attachments: if color_view.is_some() {
                &color_attachments
            } else {
                &[]
            },
            depth_stencil_attachment: depth_view.map(|view| {
                wgpu::RenderPassDepthStencilAttachment {
                    view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }
            }),
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    fn pipeline(&mut self, device: &wgpu::Device, pass_kind: OutputPass) -> &wgpu::RenderPipeline {
        let Self {
            shader,
            pipeline_layout,
            pipelines,
            ..
        } = self;
        pipelines.entry(pass_kind).or_insert_with(|| {
            let color_targets = [pass_kind
                .color_format()
                .map(|format| wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })];
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("headset eye output"),
                layout: Some(pipeline_layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vertex_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                // Every fragment writes its depth, whatever was there.
                depth_stencil: pass_kind
                    .depth_format()
                    .map(|format| wgpu::DepthStencilState {
                        format,
                        depth_write_enabled: Some(true),
                        depth_compare: Some(wgpu::CompareFunction::Always),
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some(pass_kind.fragment_entry_point()),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: if color_targets[0].is_some() {
                        &color_targets
                    } else {
                        &[]
                    },
                }),
                multiview_mask: None,
                cache: None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use vulkan_device::{PhysicalDeviceSelector, VulkanDevice, VulkanDeviceConfig};

    use super::*;

    /// Largest difference allowed between the shader's depth and `xr_math`'s; GPU division is
    /// not guaranteed to round like the CPU's.
    const DEPTH_TOLERANCE: f32 = 1e-6;
    const NEAR_PLANE: f32 = 0.05;
    const FAR_PLANE: f32 = 20.0;
    const DRAWN_COLOR: [u8; 4] = [162, 82, 25, 255];
    /// Writes [`NEAR_PIXEL_REVERSE_Z`] into the top-left pixel of a depth target and
    /// [`ELSEWHERE_REVERSE_Z`] everywhere else.
    const ONE_NEAR_PIXEL_SHADER: &str = include_str!("test_fixtures/one_near_pixel.wgsl");
    const NEAR_PIXEL_REVERSE_Z: f32 = 0.5;
    const ELSEWHERE_REVERSE_Z: f32 = 0.25;

    /// Rerun's reverse-Z for a distance in room meters, plus nothing drawn (0).
    fn reverse_depths() -> Vec<f32> {
        let mut values = vec![0.0, 1.0];
        values.extend([0.3, 1.7, 7.5, 19.9, 50.0].map(|distance| NEAR_PLANE / distance));
        values
    }

    fn create_device() -> Option<VulkanDevice> {
        let config = VulkanDeviceConfig {
            instance_descriptor: wgpu::InstanceDescriptor::new_without_display_handle(),
            picker: PhysicalDeviceSelector::Default.into_picker(),
            extra_extensions: Vec::new(),
            device_descriptor: Box::new(|adapter| wgpu::DeviceDescriptor {
                label: Some("eye output test"),
                required_limits: adapter.limits(),
                ..Default::default()
            }),
        };
        match pollster::block_on(VulkanDevice::create(config)) {
            Ok(device) => Some(device),
            Err(vulkan_device::VulkanDeviceError::NoVulkanInstance) => {
                eprintln!("skipped: no Vulkan loader on the library path");
                None
            }
            Err(error) => panic!("creating the Vulkan device: {error}"),
        }
    }

    fn test_texture(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
        resolution_in_pixel: [u32; 2],
    ) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("eye output test"),
            size: extent(resolution_in_pixel),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    }

    fn clear_color(encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture, color: [u8; 4]) {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let [red, green, blue, alpha] = color.map(|channel| f64::from(channel) / 255.0);
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("eye output test color"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: red,
                        g: green,
                        b: blue,
                        a: alpha,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
    }

    fn clear_depth(encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture, depth: f32) {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("eye output test depth"),
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(depth),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
    }

    /// A texture with four bytes per pixel, copied into a buffer with one aligned row per row.
    struct Readback {
        buffer: wgpu::Buffer,
        resolution_in_pixel: [u32; 2],
    }

    fn copy_to_buffer(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        aspect: wgpu::TextureAspect,
    ) -> Readback {
        let resolution_in_pixel = [texture.width(), texture.height()];
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("eye output test readback"),
            size: u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * u64::from(resolution_in_pixel[1]),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                aspect,
                ..texture.as_image_copy()
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                    rows_per_image: Some(resolution_in_pixel[1]),
                },
            },
            extent(resolution_in_pixel),
        );
        Readback {
            buffer,
            resolution_in_pixel,
        }
    }

    /// The four bytes of every pixel, row by row.
    fn pixels(device: &wgpu::Device, readback: &Readback) -> Vec<[u8; 4]> {
        readback
            .buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("waiting for the readback");
        let mapped = readback
            .buffer
            .slice(..)
            .get_mapped_range()
            .expect("mapping the readback");
        let row_length = readback.resolution_in_pixel[0] as usize * 4;
        mapped
            .chunks_exact(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize)
            .flat_map(|row| row[..row_length].chunks_exact(4))
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            .collect()
    }

    fn depths(device: &wgpu::Device, readback: &Readback) -> Vec<f32> {
        pixels(device, readback)
            .into_iter()
            .map(f32::from_ne_bytes)
            .collect()
    }

    fn sampleable_depth(device: &wgpu::Device, resolution_in_pixel: [u32; 2]) -> wgpu::Texture {
        test_texture(
            device,
            wgpu::TextureFormat::Depth32Float,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            resolution_in_pixel,
        )
    }

    fn eye_targets(
        device: &wgpu::Device,
        color_resolution: [u32; 2],
        depth_resolution: [u32; 2],
    ) -> GpuEyeTargets {
        let target_usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        GpuEyeTargets {
            color: test_texture(
                device,
                wgpu::TextureFormat::Rgba8Unorm,
                target_usage,
                color_resolution,
            ),
            depth: Some(test_texture(
                device,
                wgpu::TextureFormat::Depth32Float,
                target_usage,
                depth_resolution,
            )),
        }
    }

    /// Fills `reverse_depth` with [`ONE_NEAR_PIXEL_SHADER`]'s pattern.
    fn draw_one_near_pixel(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        reverse_depth: &wgpu::Texture,
    ) {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("eye output test fixture"),
            source: wgpu::ShaderSource::Wgsl(ONE_NEAR_PIXEL_SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("eye output test fixture"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[],
            }),
            multiview_mask: None,
            cache: None,
        });
        let view = reverse_depth.create_view(&wgpu::TextureViewDescriptor::default());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("eye output test fixture"),
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_pipeline(&pipeline);
        pass.draw(0..3, 0..1);
    }

    /// One pixel of [`DRAWN_COLOR`] through the pass: returns the output color and depth.
    fn render_one_pixel(
        vulkan_device: &VulkanDevice,
        eye_output: &mut EyeOutput,
        reverse_depth_value: f32,
        depth_output: DepthOutput,
    ) -> ([u8; 4], f32) {
        render_one_pixel_of(
            vulkan_device,
            eye_output,
            DRAWN_COLOR,
            reverse_depth_value,
            depth_output,
        )
    }

    /// One pixel of `composited_color` through the pass: returns the output color and depth.
    fn render_one_pixel_of(
        vulkan_device: &VulkanDevice,
        eye_output: &mut EyeOutput,
        composited_color: [u8; 4],
        reverse_depth_value: f32,
        depth_output: DepthOutput,
    ) -> ([u8; 4], f32) {
        let device = &vulkan_device.device;
        let composited = composite_target(device, wgpu::TextureFormat::Rgba8Unorm, [1, 1]);
        let reverse_depth = sampleable_depth(device, [1, 1]);
        let targets = eye_targets(device, [1, 1], [1, 1]);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("eye output test"),
        });
        clear_color(&mut encoder, &composited, composited_color);
        clear_depth(&mut encoder, &reverse_depth, reverse_depth_value);
        eye_output
            .encode(
                device,
                &mut encoder,
                &composited,
                &reverse_depth,
                depth_output,
                None,
                &targets,
            )
            .expect("encoding the output pass");
        let color_buffer = copy_to_buffer(
            device,
            &mut encoder,
            &targets.color,
            wgpu::TextureAspect::All,
        );
        let depth_target = targets.depth.as_ref().expect("a depth target");
        let depth_buffer = copy_to_buffer(
            device,
            &mut encoder,
            depth_target,
            wgpu::TextureAspect::DepthOnly,
        );
        vulkan_device.queue.submit([encoder.finish()]);
        (
            pixels(device, &color_buffer)[0],
            depths(device, &depth_buffer)[0],
        )
    }

    fn depth_output(encoding: DepthEncoding, transparent_background: bool) -> DepthOutput {
        DepthOutput {
            depth_range: DepthRange::new(NEAR_PLANE, FAR_PLANE, encoding).expect("a valid range"),
            transparent_background,
        }
    }

    #[test]
    fn depth_matches_the_cpu_conversion_in_both_normalized_encodings() {
        let Some(vulkan_device) = create_device() else {
            return;
        };
        let mut eye_output = EyeOutput::new(&vulkan_device.device, false);
        for encoding in [
            DepthEncoding::ForwardNormalized,
            DepthEncoding::ReverseNormalized,
        ] {
            let settings = depth_output(encoding, true);
            for reverse_depth in reverse_depths() {
                let (_, depth) =
                    render_one_pixel(&vulkan_device, &mut eye_output, reverse_depth, settings);
                let expected = settings.depth_range.encode_from_reverse_z(reverse_depth);
                assert!(
                    (depth - expected).abs() <= DEPTH_TOLERANCE,
                    "{encoding:?}, reverse-Z {reverse_depth}: GPU {depth}, CPU {expected}"
                );
            }
        }
    }

    #[test]
    fn the_frame_stamp_lies_over_the_top_rows_with_its_own_depth() {
        let Some(vulkan_device) = create_device() else {
            return;
        };
        let device = &vulkan_device.device;
        let mut eye_output = EyeOutput::new(device, false);
        let resolution = [64, 16];
        let stamp_rows = frame_sink::frame_stamp::stamp_rows(resolution[0]);
        let mut stamp_image = EyeImage {
            resolution_in_pixel: [resolution[0], stamp_rows],
            color_rgba8: vec![0; (resolution[0] * stamp_rows * 4) as usize],
            depth: vec![0.0; (resolution[0] * stamp_rows) as usize],
        };
        let stamp = frame_sink::FrameStamp {
            user: frame_sink::UserId(3),
            frame_id: 4321,
            eye_index: 1,
        };
        frame_sink::stamp_eye(&mut stamp_image, stamp, 0.0);
        let settings = depth_output(DepthEncoding::ForwardNormalized, true);
        let stamp_distance = 1.0;
        let overlay = StampOverlay {
            image: &stamp_image,
            reverse_depth: NEAR_PLANE / stamp_distance,
        };

        let composited = composite_target(device, wgpu::TextureFormat::Rgba8Unorm, resolution);
        let nothing_drawn = sampleable_depth(device, resolution);
        let targets = eye_targets(device, resolution, resolution);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("eye output stamp test"),
        });
        clear_color(&mut encoder, &composited, DRAWN_COLOR);
        clear_depth(&mut encoder, &nothing_drawn, 0.0);
        eye_output
            .encode(
                device,
                &mut encoder,
                &composited,
                &nothing_drawn,
                settings,
                Some(overlay),
                &targets,
            )
            .expect("encoding the output pass with a stamp");
        let color = copy_to_buffer(
            device,
            &mut encoder,
            &targets.color,
            wgpu::TextureAspect::All,
        );
        let depth_target = targets.depth.as_ref().expect("a depth target");
        let depth = copy_to_buffer(
            device,
            &mut encoder,
            depth_target,
            wgpu::TextureAspect::DepthOnly,
        );
        vulkan_device.queue.submit([encoder.finish()]);

        let rgba8: Vec<u8> = pixels(device, &color).into_iter().flatten().collect();
        let strip = frame_sink::frame_stamp::read_strip(&rgba8, resolution).expect("a stamp");
        assert!(strip.matches(stamp.frame_id, stamp.eye_index), "{strip:?}");
        let depths = depths(device, &depth);
        let stamp_depth = settings.depth_range.encode_distance(stamp_distance);
        assert!(
            (depths[0] - stamp_depth).abs() <= DEPTH_TOLERANCE,
            "a stamped pixel sits at 1 m: {} against {stamp_depth}",
            depths[0]
        );
        let below_the_stamp = (stamp_rows * resolution[0]) as usize;
        assert_eq!(
            rgba8[below_the_stamp * 4..below_the_stamp * 4 + 4],
            [0, 0, 0, 0],
            "nothing drawn below the stamp stays transparent"
        );
    }

    #[test]
    fn uncovered_pixels_become_transparent_unless_the_background_is_opaque() {
        let Some(vulkan_device) = create_device() else {
            return;
        };
        let mut eye_output = EyeOutput::new(&vulkan_device.device, false);
        let transparent = depth_output(DepthEncoding::ForwardNormalized, true);
        let opaque = depth_output(DepthEncoding::ForwardNormalized, false);

        let (uncovered, _) = render_one_pixel(&vulkan_device, &mut eye_output, 0.0, transparent);
        let (covered, _) = render_one_pixel(&vulkan_device, &mut eye_output, 0.25, transparent);
        let (kept, _) = render_one_pixel(&vulkan_device, &mut eye_output, 0.0, opaque);

        assert_eq!(uncovered, [0, 0, 0, 0]);
        assert_eq!(covered, DRAWN_COLOR);
        assert_eq!(kept, DRAWN_COLOR);
    }

    /// Rerun's compositor writes sRGB-encoded color premultiplied by alpha
    /// (`patches/re_renderer/shader/composite.wgsl`); the pass hands it to QAROS as it is, the
    /// convention of ADR-0050, so translucent content keeps `rgb <= alpha`.
    #[test]
    fn translucent_content_goes_out_premultiplied() {
        let Some(vulkan_device) = create_device() else {
            return;
        };
        let mut eye_output = EyeOutput::new(&vulkan_device.device, false);
        let premultiplied_half_alpha = [81, 41, 12, 128];
        let (covered, _) = render_one_pixel_of(
            &vulkan_device,
            &mut eye_output,
            premultiplied_half_alpha,
            0.25,
            depth_output(DepthEncoding::ForwardNormalized, true),
        );
        assert_eq!(covered, premultiplied_half_alpha);
        let [red, green, blue, alpha] = covered;
        assert!(red.max(green).max(blue) <= alpha, "{covered:?}");
    }

    #[test]
    fn kept_uniform_buffers_serve_repeated_settings_with_the_same_output() {
        let Some(vulkan_device) = create_device() else {
            return;
        };
        let mut kept_output = EyeOutput::new(&vulkan_device.device, true);
        let forward = depth_output(DepthEncoding::ForwardNormalized, true);
        let reverse = depth_output(DepthEncoding::ReverseNormalized, true);
        let reverse_depth = NEAR_PLANE / 1.7;

        let mut depths_seen = Vec::new();
        for settings in [forward, forward, reverse, forward] {
            let (color, depth) =
                render_one_pixel(&vulkan_device, &mut kept_output, reverse_depth, settings);
            assert_eq!(color, DRAWN_COLOR);
            let expected = settings.depth_range.encode_from_reverse_z(reverse_depth);
            assert!(
                (depth - expected).abs() <= DEPTH_TOLERANCE,
                "{:?}: GPU {depth}, CPU {expected}",
                settings.depth_range.encoding
            );
            depths_seen.push(depth);
        }
        assert_eq!(kept_output.uniform_buffers_created(), Some(2));
        assert_eq!(
            depths_seen[0].to_bits(),
            depths_seen[3].to_bits(),
            "the kept forward buffer gives the first frame's depth again"
        );
        assert!(
            (depths_seen[0] - depths_seen[2]).abs() > 0.1,
            "the reverse frame used a buffer of its own: {depths_seen:?}"
        );
        assert_eq!(
            EyeOutput::new(&vulkan_device.device, false).uniform_buffers_created(),
            None
        );
    }

    #[test]
    fn kept_uniform_buffers_drop_the_oldest_beyond_the_limit() {
        let Some(vulkan_device) = create_device() else {
            return;
        };
        let device = &vulkan_device.device;
        let mut kept_buffers = KeptUniformBuffers::default();
        let uniform = |index: u8| [index; 32];
        let now = Instant::now();
        for index in 0..=u8::try_from(KEPT_UNIFORM_BUFFERS).unwrap() {
            kept_buffers.buffer_for(device, uniform(index), now);
        }
        assert_eq!(kept_buffers.buffers.len(), KEPT_UNIFORM_BUFFERS);
        assert_eq!(kept_buffers.created, KEPT_UNIFORM_BUFFERS as u64 + 1);
        assert_eq!(
            kept_buffers.buffers[0].uniform_bytes,
            uniform(1),
            "the oldest went first"
        );
        kept_buffers.buffer_for(device, uniform(1), now);
        assert_eq!(
            kept_buffers.created,
            KEPT_UNIFORM_BUFFERS as u64 + 1,
            "a kept one is reused"
        );
        kept_buffers.buffer_for(device, uniform(0), now);
        assert_eq!(
            kept_buffers.created,
            KEPT_UNIFORM_BUFFERS as u64 + 2,
            "a dropped one is made again"
        );
    }

    #[test]
    fn composite_targets_are_kept_per_user_and_eye_until_unused() {
        let Some(vulkan_device) = create_device() else {
            return;
        };
        let device = &vulkan_device.device;
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut kept_targets = KeptCompositeTargets::default();
        let left_of_first = (frame_sink::UserId(1), 0);
        let right_of_first = (frame_sink::UserId(1), 1);
        let start = Instant::now();
        let after = |milliseconds| start + Duration::from_millis(milliseconds);

        let first = kept_targets.target_for(device, format, [64, 32], left_of_first, start);
        let again = kept_targets.target_for(device, format, [64, 32], left_of_first, after(20));
        assert_eq!(first, again, "the same user and eye get the same texture");
        let right = kept_targets.target_for(device, format, [64, 32], right_of_first, after(20));
        assert_ne!(first, right);
        assert_eq!(kept_targets.created, 2);

        let resized = kept_targets.target_for(device, format, [32, 32], left_of_first, after(30));
        assert_eq!([resized.width(), resized.height()], [32, 32]);
        assert_eq!(
            kept_targets.created, 3,
            "a new resolution replaces the texture"
        );

        let much_later = after(30) + UNUSED_EYE_RESOURCE_LIFETIME + Duration::from_millis(1);
        kept_targets.target_for(device, format, [32, 32], left_of_first, much_later);
        assert_eq!(
            kept_targets.created, 4,
            "a target unused for too long was released"
        );
        assert!(
            !kept_targets.targets.contains_key(&right_of_first),
            "the right eye, unused since 20 ms, was released too"
        );

        kept_targets.release_unused(much_later + UNUSED_EYE_RESOURCE_LIFETIME);
        assert_eq!(
            kept_targets.targets.len(),
            1,
            "kept at exactly the lifetime"
        );
        kept_targets
            .release_unused(much_later + UNUSED_EYE_RESOURCE_LIFETIME + Duration::from_millis(1));
        assert!(
            kept_targets.targets.is_empty(),
            "nothing stays once nobody streams for the lifetime"
        );
    }

    #[test]
    fn kept_uniform_buffers_are_released_once_unused_for_the_lifetime() {
        let Some(vulkan_device) = create_device() else {
            return;
        };
        let device = &vulkan_device.device;
        let mut kept_buffers = KeptUniformBuffers::default();
        let start = Instant::now();
        let forward = [1; 32];
        let reverse = [2; 32];
        kept_buffers.buffer_for(device, forward, start);
        let reverse_last_used = start + Duration::from_secs(4);
        kept_buffers.buffer_for(device, reverse, reverse_last_used);

        kept_buffers.release_unused(start + UNUSED_EYE_RESOURCE_LIFETIME);
        assert_eq!(
            kept_buffers.buffers.len(),
            2,
            "kept at exactly the lifetime"
        );
        kept_buffers
            .release_unused(start + UNUSED_EYE_RESOURCE_LIFETIME + Duration::from_millis(1));
        let kept_contents: Vec<[u8; 32]> = kept_buffers
            .buffers
            .iter()
            .map(|kept_buffer| kept_buffer.uniform_bytes)
            .collect();
        assert_eq!(
            kept_contents,
            [reverse],
            "only the one unused too long went"
        );

        kept_buffers.release_unused(
            reverse_last_used + UNUSED_EYE_RESOURCE_LIFETIME + Duration::from_millis(1),
        );
        assert!(kept_buffers.buffers.is_empty());
        kept_buffers.buffer_for(
            device,
            reverse,
            reverse_last_used + UNUSED_EYE_RESOURCE_LIFETIME * 2,
        );
        assert_eq!(kept_buffers.created, 3, "a released one is made again");
    }

    #[test]
    fn smaller_depth_keeps_the_nearest_depth_of_each_footprint() {
        let Some(vulkan_device) = create_device() else {
            return;
        };
        let device = &vulkan_device.device;
        let mut eye_output = EyeOutput::new(device, false);
        let settings = depth_output(DepthEncoding::ForwardNormalized, true);
        let composited = composite_target(device, wgpu::TextureFormat::Rgba8Unorm, [4, 4]);
        let pattern = sampleable_depth(device, [4, 4]);
        let nothing_drawn = sampleable_depth(device, [4, 4]);
        let pattern_targets = eye_targets(device, [4, 4], [2, 2]);
        let empty_targets = eye_targets(device, [4, 4], [2, 2]);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("eye output test"),
        });
        clear_color(&mut encoder, &composited, DRAWN_COLOR);
        draw_one_near_pixel(device, &mut encoder, &pattern);
        clear_depth(&mut encoder, &nothing_drawn, 0.0);
        for (reverse_depth, targets) in [
            (&pattern, &pattern_targets),
            (&nothing_drawn, &empty_targets),
        ] {
            eye_output
                .encode(
                    device,
                    &mut encoder,
                    &composited,
                    reverse_depth,
                    settings,
                    None,
                    targets,
                )
                .expect("encoding the output passes");
        }
        let depth_of = |targets: &GpuEyeTargets, encoder: &mut wgpu::CommandEncoder| {
            copy_to_buffer(
                device,
                encoder,
                targets.depth.as_ref().expect("a depth target"),
                wgpu::TextureAspect::DepthOnly,
            )
        };
        let pattern_depth = depth_of(&pattern_targets, &mut encoder);
        let empty_depth = depth_of(&empty_targets, &mut encoder);
        let pattern_color = copy_to_buffer(
            device,
            &mut encoder,
            &pattern_targets.color,
            wgpu::TextureAspect::All,
        );
        vulkan_device.queue.submit([encoder.finish()]);

        let near = settings
            .depth_range
            .encode_from_reverse_z(NEAR_PIXEL_REVERSE_Z);
        let elsewhere = settings
            .depth_range
            .encode_from_reverse_z(ELSEWHERE_REVERSE_Z);
        let expected = [near, elsewhere, elsewhere, elsewhere];
        for (depth, expected) in depths(device, &pattern_depth).into_iter().zip(expected) {
            assert!(
                (depth - expected).abs() <= DEPTH_TOLERANCE,
                "GPU {depth}, CPU {expected}"
            );
        }
        assert_eq!(
            depths(device, &empty_depth),
            [1.0; 4],
            "nothing drawn is far"
        );
        assert!(
            pixels(device, &pattern_color)
                .iter()
                .all(|pixel| *pixel == DRAWN_COLOR),
            "color stays at the eye's resolution"
        );
    }

    #[test]
    fn linear_depth_is_refused_for_a_depth_target_only() {
        let linear = depth_output(DepthEncoding::LinearMeters, true);
        assert!(linear.uniform_bytes(true, [1.0, 1.0], None).is_err());
        assert!(linear.uniform_bytes(false, [1.0, 1.0], None).is_ok());
    }

    #[test]
    fn uniform_carries_near_far_encoding_coverage_mode_and_scale() {
        let bytes = depth_output(DepthEncoding::ReverseNormalized, false)
            .uniform_bytes(true, [2.0, 4.0], None)
            .expect("a normalized encoding");
        let word = |index: usize| {
            let mut four = [0_u8; 4];
            four.copy_from_slice(&bytes[index * 4..index * 4 + 4]);
            four
        };
        assert_eq!(word(0), NEAR_PLANE.to_ne_bytes());
        assert_eq!(word(1), FAR_PLANE.to_ne_bytes());
        assert_eq!(u32::from_ne_bytes(word(2)), 1);
        assert_eq!(u32::from_ne_bytes(word(3)), 1);
        assert_eq!(word(4), 2.0_f32.to_ne_bytes());
        assert_eq!(word(5), 4.0_f32.to_ne_bytes());
    }
}
