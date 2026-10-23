//! Writes one eye's color and depth into the CPU frame textures, at the view rectangles the
//! frame reports, respecting pitch, flips and the pixel format.

use frame_sink::{EyeImage, SinkError};
use qaros::{CpuFrame, CpuTexture, Eye, PixelFormat, VideoFrameView, ViewKind};

/// A view rectangle resolved to a start corner, a size and flips. End coordinates are
/// exclusive; an end before the start mirrors that axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewRegion {
    pub width: u32,
    pub height: u32,
    first_column: u32,
    first_row: u32,
    mirrored_horizontally: bool,
    mirrored_vertically: bool,
}

impl ViewRegion {
    pub fn of(view: &VideoFrameView) -> Self {
        let mirrored_horizontally = view.end_x < view.start_x;
        let mirrored_vertically = view.end_y < view.start_y;
        Self {
            width: view.start_x.abs_diff(view.end_x),
            height: view.start_y.abs_diff(view.end_y),
            first_column: if mirrored_horizontally {
                view.start_x.saturating_sub(1)
            } else {
                view.start_x
            },
            first_row: if mirrored_vertically {
                view.start_y.saturating_sub(1)
            } else {
                view.start_y
            },
            mirrored_horizontally,
            mirrored_vertically,
        }
    }

    pub fn resolution_in_pixel(self) -> [u32; 2] {
        [self.width, self.height]
    }

    fn texture_row(self, image_row: u32) -> u32 {
        if self.mirrored_vertically {
            self.first_row - image_row
        } else {
            self.first_row + image_row
        }
    }

    fn texture_column(self, image_column: u32) -> u32 {
        if self.mirrored_horizontally {
            self.first_column - image_column
        } else {
            self.first_column + image_column
        }
    }
}

fn eye_name(eye: Eye) -> &'static str {
    match eye {
        Eye::Left => "left",
        Eye::Right => "right",
        _ => "unassigned",
    }
}

pub(crate) fn find_view(
    views: &[VideoFrameView],
    kind: ViewKind,
    eye: Eye,
) -> Result<VideoFrameView, SinkError> {
    views
        .iter()
        .find(|view| view.kind == kind && view.eye == eye)
        .copied()
        .ok_or_else(|| {
            SinkError::Backend(format!(
                "the frame has no {kind:?} view for the {} eye",
                eye_name(eye)
            ))
        })
}

/// Copies `image` into the color and depth views of `eye`.
pub(crate) fn write_eye(
    frame: &mut CpuFrame<'_>,
    eye: Eye,
    image: &EyeImage,
) -> Result<(), SinkError> {
    let color_view = find_view(&frame.views, ViewKind::Color, eye)?;
    let depth_view = find_view(&frame.views, ViewKind::Depth, eye)?;
    let color_texture = texture_for(&mut frame.textures, &color_view)?;
    write_color(color_texture, &color_view, image)?;
    let depth_texture = texture_for(&mut frame.textures, &depth_view)?;
    write_depth(depth_texture, &depth_view, image)
}

fn texture_for<'frame, 'textures>(
    textures: &'textures mut [CpuTexture<'frame>],
    view: &VideoFrameView,
) -> Result<&'textures mut CpuTexture<'frame>, SinkError> {
    if view.array_layer_index != 0 {
        return Err(SinkError::Backend(format!(
            "array layer {} requested; only single-layer CPU textures are supported",
            view.array_layer_index
        )));
    }
    let texture_count = textures.len();
    textures
        .get_mut(view.texture_index as usize)
        .ok_or_else(|| {
            SinkError::Backend(format!(
                "view refers to texture {} of {texture_count}",
                view.texture_index
            ))
        })
}

fn check_fits(
    texture: &CpuTexture<'_>,
    region: ViewRegion,
    image: &EyeImage,
    bytes_per_pixel: u32,
) -> Result<(), SinkError> {
    if region.resolution_in_pixel() != image.resolution_in_pixel {
        return Err(SinkError::InvalidImage(format!(
            "eye image is {:?}, the view is {:?}",
            image.resolution_in_pixel,
            region.resolution_in_pixel()
        )));
    }
    if region.width == 0 || region.height == 0 {
        return Ok(());
    }
    let last_row = region
        .texture_row(0)
        .max(region.texture_row(region.height - 1));
    let last_column = region
        .texture_column(0)
        .max(region.texture_column(region.width - 1));
    let row_bytes = (u64::from(last_column) + 1) * u64::from(bytes_per_pixel);
    let needed = u64::from(last_row) * u64::from(texture.pitch) + row_bytes;
    if row_bytes > u64::from(texture.pitch) || needed > texture.data.len() as u64 {
        return Err(SinkError::Backend(format!(
            "view {region:?} does not fit a texture of {} bytes with pitch {}",
            texture.data.len(),
            texture.pitch
        )));
    }
    Ok(())
}

/// Writes `pixel_bytes(source_index)` for every pixel of the view, row by row.
fn write_pixels<const BYTES_PER_PIXEL: usize>(
    texture: &mut CpuTexture<'_>,
    region: ViewRegion,
    mut pixel_bytes: impl FnMut(usize) -> [u8; BYTES_PER_PIXEL],
) {
    let width = region.width as usize;
    let pitch = texture.pitch as usize;
    for image_row in 0..region.height {
        let row_start = region.texture_row(image_row) as usize * pitch;
        let source_row_start = image_row as usize * width;
        for image_column in 0..region.width {
            let offset = row_start + region.texture_column(image_column) as usize * BYTES_PER_PIXEL;
            let bytes = pixel_bytes(source_row_start + image_column as usize);
            texture.data[offset..offset + BYTES_PER_PIXEL].copy_from_slice(&bytes);
        }
    }
}

fn write_color(
    texture: &mut CpuTexture<'_>,
    view: &VideoFrameView,
    image: &EyeImage,
) -> Result<(), SinkError> {
    let region = ViewRegion::of(view);
    check_fits(texture, region, image, 4)?;
    let rgba = &image.color_rgba8;
    match view.format {
        PixelFormat::R8G8B8A8 => write_pixels(texture, region, |index| {
            let pixel = index * 4;
            [
                rgba[pixel],
                rgba[pixel + 1],
                rgba[pixel + 2],
                rgba[pixel + 3],
            ]
        }),
        PixelFormat::B8G8R8A8 => write_pixels(texture, region, |index| {
            let pixel = index * 4;
            [
                rgba[pixel + 2],
                rgba[pixel + 1],
                rgba[pixel],
                rgba[pixel + 3],
            ]
        }),
        other => {
            return Err(SinkError::Backend(format!(
                "color format {other:?} is not supported; use R8G8B8A8 or B8G8R8A8"
            )));
        }
    }
    Ok(())
}

fn write_depth(
    texture: &mut CpuTexture<'_>,
    view: &VideoFrameView,
    image: &EyeImage,
) -> Result<(), SinkError> {
    let region = ViewRegion::of(view);
    check_fits(texture, region, image, 4)?;
    match view.format {
        PixelFormat::D32Float | PixelFormat::R32Float => {
            write_pixels(texture, region, |index| image.depth[index].to_ne_bytes());
            Ok(())
        }
        other => Err(SinkError::Backend(format!(
            "depth format {other:?} is not supported; use D32_FLOAT or R32_FLOAT"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qaros::TextureSize;

    fn view(kind: ViewKind, eye: Eye, format: PixelFormat, corners: [u32; 4]) -> VideoFrameView {
        let [start_x, start_y, end_x, end_y] = corners;
        VideoFrameView {
            start_x,
            start_y,
            end_x,
            end_y,
            texture_index: 0,
            array_layer_index: 0,
            format,
            kind,
            eye,
        }
    }

    fn texture(data: &mut [u8], width: u32, height: u32, pitch: u32) -> CpuTexture<'_> {
        CpuTexture {
            size: TextureSize {
                format: PixelFormat::B8G8R8A8,
                width,
                height,
                array_layers: 1,
            },
            pitch,
            data,
        }
    }

    /// 2x2 eye: red, green on the top row; blue, white on the bottom row.
    fn two_by_two() -> EyeImage {
        EyeImage {
            resolution_in_pixel: [2, 2],
            color_rgba8: vec![
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 255, 255, 255, 128,
            ],
            depth: vec![0.25, 0.5, 0.75, 1.0],
        }
    }

    fn pixel(data: &[u8], pitch: usize, column: usize, row: usize) -> [u8; 4] {
        let offset = row * pitch + column * 4;
        [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]
    }

    #[test]
    fn bgra_swizzles_and_respects_pitch_and_offset() {
        // Right half of a 4x2 side-by-side texture, rows padded to 20 bytes.
        let mut bytes = vec![0_u8; 20 * 2];
        let mut target = texture(&mut bytes, 4, 2, 20);
        let right = view(
            ViewKind::Color,
            Eye::Right,
            PixelFormat::B8G8R8A8,
            [2, 0, 4, 2],
        );
        write_color(&mut target, &right, &two_by_two()).unwrap();
        assert_eq!(pixel(&bytes, 20, 2, 0), [0, 0, 255, 255], "red as BGRA");
        assert_eq!(pixel(&bytes, 20, 3, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&bytes, 20, 2, 1), [255, 0, 0, 255], "blue as BGRA");
        assert_eq!(pixel(&bytes, 20, 3, 1), [255, 255, 255, 128]);
        assert_eq!(pixel(&bytes, 20, 0, 0), [0, 0, 0, 0], "left half untouched");
        assert_eq!(&bytes[16..20], &[0, 0, 0, 0], "row padding untouched");
    }

    #[test]
    fn rgba_copies_and_flipped_views_mirror() {
        let mut bytes = vec![0_u8; 8 * 2];
        let mut target = texture(&mut bytes, 2, 2, 8);
        let flipped = view(
            ViewKind::Color,
            Eye::Left,
            PixelFormat::R8G8B8A8,
            [0, 2, 2, 0],
        );
        write_color(&mut target, &flipped, &two_by_two()).unwrap();
        assert_eq!(
            pixel(&bytes, 8, 0, 1),
            [255, 0, 0, 255],
            "top row lands at the bottom"
        );
        assert_eq!(pixel(&bytes, 8, 0, 0), [0, 0, 255, 255]);

        let mut bytes = vec![0_u8; 8 * 2];
        let mut target = texture(&mut bytes, 2, 2, 8);
        let mirrored = view(
            ViewKind::Color,
            Eye::Left,
            PixelFormat::R8G8B8A8,
            [2, 0, 0, 2],
        );
        write_color(&mut target, &mirrored, &two_by_two()).unwrap();
        assert_eq!(
            pixel(&bytes, 8, 1, 0),
            [255, 0, 0, 255],
            "first column lands last"
        );
        assert_eq!(pixel(&bytes, 8, 0, 0), [0, 255, 0, 255]);
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "the copy must be bit-exact")]
    fn depth_is_written_as_native_floats() {
        let mut bytes = vec![0_u8; 16 * 2];
        let mut target = texture(&mut bytes, 4, 2, 16);
        let left = view(
            ViewKind::Depth,
            Eye::Left,
            PixelFormat::D32Float,
            [0, 0, 2, 2],
        );
        write_depth(&mut target, &left, &two_by_two()).unwrap();
        let read = |column: usize, row: usize| {
            let offset = row * 16 + column * 4;
            f32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
        };
        assert_eq!(
            [read(0, 0), read(1, 0), read(0, 1), read(1, 1)],
            [0.25, 0.5, 0.75, 1.0]
        );
        assert_eq!(read(2, 0), 0.0, "the right eye's half untouched");
    }

    #[test]
    fn rejects_size_mismatch_overflow_and_unknown_formats() {
        let mut bytes = vec![0_u8; 8 * 2];
        let mut target = texture(&mut bytes, 2, 2, 8);
        let too_big = view(
            ViewKind::Color,
            Eye::Left,
            PixelFormat::R8G8B8A8,
            [0, 0, 3, 2],
        );
        assert!(matches!(
            write_color(&mut target, &too_big, &two_by_two()),
            Err(SinkError::InvalidImage(_))
        ));
        let outside = view(
            ViewKind::Color,
            Eye::Left,
            PixelFormat::R8G8B8A8,
            [1, 1, 3, 3],
        );
        assert!(matches!(
            write_color(&mut target, &outside, &two_by_two()),
            Err(SinkError::Backend(_))
        ));
        let half_float = view(
            ViewKind::Color,
            Eye::Left,
            PixelFormat::R16G16B16A16,
            [0, 0, 2, 2],
        );
        assert!(matches!(
            write_color(&mut target, &half_float, &two_by_two()),
            Err(SinkError::Backend(_))
        ));
        assert_eq!(bytes, vec![0_u8; 16], "nothing written on rejection");
    }
}
