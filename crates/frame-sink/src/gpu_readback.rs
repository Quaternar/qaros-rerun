//! A [`GpuFrameSink`] over any [`FrameSink`]: hands the renderer textures of its own, the way
//! a sink with GPU images does, reads them back once the frame is submitted, and passes the
//! pixels on. With the image sink this writes the GPU path's output as PNGs, comparable byte
//! for byte with the CPU path's.

use std::collections::BTreeMap;

use crate::{
    DepthRange, EYE_COUNT, EyeImage, FrameKey, FrameSink, GpuEyeTargets, GpuFrameRequest,
    GpuFrameSink, ShowInfo, SinkError,
};

/// The color format of the targets; the CPU path's pixels are RGBA too.
pub const READBACK_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
pub const READBACK_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Bytes per pixel of both formats.
const BYTES_PER_PIXEL: u32 = 4;

pub struct GpuReadbackSink<Sink: FrameSink> {
    device: wgpu::Device,
    queue: wgpu::Queue,
    sink: Sink,
    depth_scale_down: u32,
    open_frames: BTreeMap<FrameKey, [GpuEyeTargets; EYE_COUNT]>,
}

impl<Sink: FrameSink> GpuReadbackSink<Sink> {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, sink: Sink) -> Self {
        Self {
            device: device.clone(),
            queue: queue.clone(),
            sink,
            depth_scale_down: 1,
            open_frames: BTreeMap::new(),
        }
    }

    /// Hands out depth targets `divisor` times smaller than color in each dimension (rounded
    /// up), the way a sender with lower-resolution depth would. The depth read back is
    /// stretched to the eye's resolution by repeating pixels before it reaches the wrapped sink.
    #[must_use]
    pub fn with_depth_scale_down(mut self, divisor: u32) -> Self {
        self.depth_scale_down = divisor.max(1);
        self
    }

    pub fn sink(&self) -> &Sink {
        &self.sink
    }

    fn target(&self, label: &str, format: wgpu::TextureFormat, size: [u32; 2]) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent(size),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Submits `commands` and copies of every target, waits for the device, and returns the
    /// tightly packed bytes of each copy, in target order.
    fn submit_and_read_back(
        &self,
        commands: wgpu::CommandBuffer,
        targets: &[&wgpu::Texture],
    ) -> Result<Vec<Vec<u8>>, SinkError> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gpu readback sink"),
            });
        let buffers: Vec<(wgpu::Buffer, u32, [u32; 2])> = targets
            .iter()
            .map(|texture| {
                let size = [texture.width(), texture.height()];
                let padded_row = padded_bytes_per_row(size[0]);
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpu readback sink"),
                    size: u64::from(padded_row) * u64::from(size[1]),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                encoder.copy_texture_to_buffer(
                    texture.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(padded_row),
                            rows_per_image: Some(size[1]),
                        },
                    },
                    extent(size),
                );
                (buffer, padded_row, size)
            })
            .collect();
        self.queue.submit([commands, encoder.finish()]);
        for (buffer, _, _) in &buffers {
            buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        }
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| SinkError::Backend(format!("waiting for the readback: {error}")))?;
        buffers
            .iter()
            .map(|(buffer, padded_row, size)| {
                let mapped = buffer.slice(..).get_mapped_range().map_err(|error| {
                    SinkError::Backend(format!("mapping the readback: {error}"))
                })?;
                let row = (size[0] * BYTES_PER_PIXEL) as usize;
                Ok(mapped
                    .chunks_exact(*padded_row as usize)
                    .flat_map(|padded| &padded[..row])
                    .copied()
                    .collect())
            })
            .collect()
    }
}

fn extent(size: [u32; 2]) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: size[0],
        height: size[1],
        depth_or_array_layers: 1,
    }
}

fn scaled_down(size: [u32; 2], divisor: u32) -> [u32; 2] {
    size.map(|length| length.div_ceil(divisor).max(1))
}

/// Row-major `values` of `from` size, resampled to `to` by taking the pixel each target pixel
/// falls into.
fn stretch_nearest(values: &[f32], from: [u32; 2], to: [u32; 2]) -> Vec<f32> {
    if from == to {
        return values.to_vec();
    }
    let source_index = |target: u32, target_length: u32, source_length: u32| {
        let scaled = u64::from(target) * u64::from(source_length) / u64::from(target_length);
        usize::try_from(scaled).unwrap_or(usize::MAX)
    };
    let source_width = usize::try_from(from[0]).unwrap_or(usize::MAX);
    (0..to[1])
        .flat_map(|row| (0..to[0]).map(move |column| (row, column)))
        .map(|(row, column)| {
            let source_row = source_index(row, to[1], from[1]);
            let source_column = source_index(column, to[0], from[0]);
            values[source_row * source_width + source_column]
        })
        .collect()
}

fn padded_bytes_per_row(width: u32) -> u32 {
    (width * BYTES_PER_PIXEL).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

impl<Sink: FrameSink> GpuFrameSink for GpuReadbackSink<Sink> {
    fn begin_frames(&mut self) -> Result<Vec<GpuFrameRequest>, SinkError> {
        let requests = self.sink.begin_frames()?;
        let mut gpu_requests = Vec::with_capacity(requests.len());
        for request in requests {
            let size = request.resolution_in_pixel;
            let depth_size = scaled_down(size, self.depth_scale_down);
            let eyes = [0, 1].map(|_| GpuEyeTargets {
                color: self.target("gpu readback color", READBACK_COLOR_FORMAT, size),
                depth: Some(self.target("gpu readback depth", READBACK_DEPTH_FORMAT, depth_size)),
            });
            self.open_frames.insert(request.key, eyes.clone());
            gpu_requests.push(GpuFrameRequest { request, eyes });
        }
        Ok(gpu_requests)
    }

    fn update_users(&mut self) {
        self.sink.update_users();
    }

    fn submit_and_show(
        &mut self,
        key: FrameKey,
        commands: wgpu::CommandBuffer,
        show: ShowInfo,
    ) -> Result<(), SinkError> {
        let eyes = self
            .open_frames
            .remove(&key)
            .ok_or(SinkError::UnknownFrame(key))?;
        let shown = self.read_back_and_show(key, commands, &eyes, show);
        if shown.is_err() {
            self.sink.abandon_frame(key);
        }
        shown
    }

    fn abandon_frame(&mut self, key: FrameKey) {
        if self.open_frames.remove(&key).is_some() {
            self.sink.abandon_frame(key);
        }
    }

    fn is_streaming(&self) -> bool {
        self.sink.is_streaming()
    }

    fn is_finished(&self) -> bool {
        self.sink.is_finished()
    }

    fn depth_range(&self) -> DepthRange {
        self.sink.depth_range()
    }
}

impl<Sink: FrameSink> GpuReadbackSink<Sink> {
    fn read_back_and_show(
        &mut self,
        key: FrameKey,
        commands: wgpu::CommandBuffer,
        eyes: &[GpuEyeTargets; EYE_COUNT],
        show: ShowInfo,
    ) -> Result<(), SinkError> {
        let mut targets = Vec::with_capacity(EYE_COUNT * 2);
        for eye in eyes {
            targets.push(&eye.color);
            targets.extend(eye.depth.as_ref());
        }
        let mut copies = self.submit_and_read_back(commands, &targets)?.into_iter();
        for (view_index, eye) in eyes.iter().enumerate() {
            let (Some(color_rgba8), Some(depth_bytes)) = (copies.next(), copies.next()) else {
                return Err(SinkError::Backend("a readback is missing".to_owned()));
            };
            let depth: Vec<f32> = depth_bytes
                .chunks_exact(4)
                .map(|value| f32::from_ne_bytes([value[0], value[1], value[2], value[3]]))
                .collect();
            let resolution_in_pixel = [eye.color.width(), eye.color.height()];
            let depth_size = eye
                .depth
                .as_ref()
                .map_or(resolution_in_pixel, |depth| [depth.width(), depth.height()]);
            let image = EyeImage {
                resolution_in_pixel,
                color_rgba8,
                depth: stretch_nearest(&depth, depth_size, resolution_in_pixel),
            };
            self.sink.submit_view(key, view_index, image)?;
        }
        self.sink.show_frame(key, show)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_padded_to_the_copy_alignment() {
        assert_eq!(padded_bytes_per_row(64), 256);
        assert_eq!(padded_bytes_per_row(65), 512);
        assert_eq!(padded_bytes_per_row(960), 3840);
    }

    #[test]
    fn depth_targets_are_scaled_down_rounding_up() {
        assert_eq!(scaled_down([960, 1000], 1), [960, 1000]);
        assert_eq!(scaled_down([1832, 1920], 2), [916, 960]);
        assert_eq!(scaled_down([5, 3], 2), [3, 2]);
        assert_eq!(scaled_down([1, 1], 4), [1, 1]);
    }

    #[test]
    fn stretched_depth_repeats_each_source_pixel() {
        let source = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(
            stretch_nearest(&source, [2, 2], [4, 2]),
            [1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0]
        );
        assert_eq!(
            stretch_nearest(&source, [2, 2], [3, 3]),
            [1.0, 1.0, 2.0, 1.0, 1.0, 2.0, 3.0, 3.0, 4.0]
        );
        assert_eq!(stretch_nearest(&source, [2, 2], [2, 2]), source);
    }
}
