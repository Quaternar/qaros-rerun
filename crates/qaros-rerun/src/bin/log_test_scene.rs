//! Logs the test scene to a running viewer over the standard SDK connection,
//! with no headset-specific code: a 1 m cube standing on the ground, a ring of points at its
//! mid-height, axes at the origin long enough to stick out of the cube, a scalar time series and a small image.
//!
//! `cargo run -p qaros-rerun --bin log-test-scene -- [steps] [url] [heavy entities] [distance]`
//!
//! `heavy entities` (default 0) adds that many small point clouds on a grid around the cube, a
//! scene whose update in the viewer takes long (every entity is queried and visualized every
//! viewer frame), for measuring the headset rate against the scene update time.
//!
//! `distance` adds a magenta PNG-encoded image under a pinhole camera at the cube's center,
//! looking along -Y, its image plane `distance` meters away. Rerun skips encoded images and video
//! frames outside the desktop camera's view. The default desktop camera looks along +Y from
//! about 2 m on the -Y side, and a pinhole counts only its origin toward the scene bounds, so at
//! 8 m the image lies behind the desktop camera; the headset images show whether the eyes still
//! get it.

use std::f32::consts::TAU;

fn ring_points(count: usize, radius: f32, height: f32, phase: f32) -> Vec<[f32; 3]> {
    (0..count)
        .map(|index| {
            #[allow(clippy::cast_precision_loss)]
            let angle = TAU * index as f32 / count as f32 + phase;
            [radius * angle.cos(), radius * angle.sin(), height]
        })
        .collect()
}

fn gradient_image(width: usize, height: usize, step: usize) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        for column in 0..width {
            let red = u8::try_from((column * 255) / width).unwrap_or(u8::MAX);
            let green = u8::try_from((row * 255) / height).unwrap_or(u8::MAX);
            let blue = u8::try_from((step * 4) % 256).unwrap_or(u8::MAX);
            pixels.extend_from_slice(&[red, green, blue]);
        }
    }
    pixels
}

/// A square grid of small point clouds, 3 m wide, around the cube.
fn log_heavy_entities(
    recording: &rerun::RecordingStream,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let side = (count as f64).sqrt().ceil().max(1.0) as usize;
    for index in 0..count {
        #[allow(clippy::cast_precision_loss)]
        let (x, y) = (
            (index % side) as f32 / side as f32 * 3.0 - 1.5,
            (index / side) as f32 / side as f32 * 3.0 - 1.5,
        );
        recording.log_static(
            format!("world/heavy/{index}"),
            &rerun::Points3D::new([[x, y, 0.0], [x, y, 0.02], [x + 0.02, y, 0.0]])
                .with_radii([0.005])
                .with_colors([rerun::Color::from_rgb(120, 220, 120)]),
        )?;
    }
    Ok(())
}

/// A magenta PNG under a pinhole at the cube's center, its image plane `distance` ahead along -Y.
fn log_encoded_image(
    recording: &rerun::RecordingStream,
    distance: f32,
) -> Result<(), Box<dyn std::error::Error>> {
    let side = 64;
    let magenta = image::RgbImage::from_pixel(side, side, image::Rgb([255, 0, 255]));
    let mut png = std::io::Cursor::new(Vec::new());
    magenta.write_to(&mut png, image::ImageFormat::Png)?;
    #[allow(clippy::cast_precision_loss)]
    let side_pixels = side as f32;
    // Columns are the camera's right, down and forward axes (RDF) in the world.
    let world_from_camera = rerun::Transform3D::from_translation_mat3x3(
        [0.0, 0.0, 0.5],
        [[-1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]],
    );
    recording.log_static("world/offscreen_camera", &world_from_camera)?;
    recording.log_static(
        "world/offscreen_camera",
        &rerun::Pinhole::from_focal_length_and_resolution(
            [side_pixels, side_pixels],
            [side_pixels, side_pixels],
        )
        .with_image_plane_distance(distance),
    )?;
    recording.log_static(
        "world/offscreen_camera",
        &rerun::EncodedImage::from_file_contents(png.into_inner()),
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let steps: usize = arguments.next().map_or(Ok(200), |text| text.parse())?;
    let url = arguments
        .next()
        .unwrap_or_else(|| "rerun+http://127.0.0.1:9876/proxy".to_owned());
    let heavy_entities: usize = arguments.next().map_or(Ok(0), |text| text.parse())?;
    let encoded_image_distance: Option<f32> =
        arguments.next().map(|text| text.parse()).transpose()?;

    let recording =
        rerun::RecordingStreamBuilder::new("qaros_rerun_test_scene").connect_grpc_opts(url)?;

    recording.log_static("/", &rerun::ViewCoordinates::RIGHT_HAND_Z_UP())?;
    recording.log_static(
        "world/cube",
        &rerun::Boxes3D::from_centers_and_half_sizes([(0.0, 0.0, 0.5)], [(0.5, 0.5, 0.5)])
            .with_fill_mode(rerun::FillMode::Solid)
            .with_colors([rerun::Color::from_rgb(230, 120, 40)]),
    )?;
    recording.log_static(
        "world/origin_axes",
        &rerun::Arrows3D::from_vectors([[0.9, 0.0, 0.0], [0.0, 0.9, 0.0], [0.0, 0.0, 1.4]])
            .with_colors([
                rerun::Color::from_rgb(255, 0, 0),
                rerun::Color::from_rgb(0, 255, 0),
                rerun::Color::from_rgb(0, 0, 255),
            ]),
    )?;

    log_heavy_entities(&recording, heavy_entities)?;
    if let Some(distance) = encoded_image_distance {
        log_encoded_image(&recording, distance)?;
    }

    let (image_width, image_height) = (96, 64);
    for step in 0..steps {
        recording.set_time_sequence("step", i64::try_from(step)?);
        #[allow(clippy::cast_precision_loss)]
        let phase = step as f32 * 0.05;
        recording.log(
            "world/ring",
            &rerun::Points3D::new(ring_points(48, 1.0, 0.5, phase))
                .with_radii([0.03])
                .with_colors([rerun::Color::from_rgb(80, 200, 255)]),
        )?;
        recording.log(
            "plots/sine",
            &rerun::Scalars::single(f64::from(phase.sin())),
        )?;
        recording.log(
            "camera/image",
            &rerun::Image::from_rgb24(
                gradient_image(image_width, image_height, step),
                [u32::try_from(image_width)?, u32::try_from(image_height)?],
            ),
        )?;
    }
    recording.flush_blocking()?;
    Ok(())
}
