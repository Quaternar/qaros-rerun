//! One `re_renderer` view per eye, drawn inside the viewer frame.
//!
//! `re_renderer` 0.40.0 keeps the main depth buffer to itself: multisampled, never exposed, and
//! stored past the pass only while a volume is queued. Depth comes from the picking layer
//! instead, configured to cover the whole eye: it is drawn without MSAA from the same draw
//! data. On the CPU path color comes from `ViewBuilder::schedule_screenshot` (RGBA8, the same
//! compositing as the desktop view) and the picking layer carries its depth to the CPU; on the
//! GPU path the view is composited into a texture and the picking layer's depth stays on the
//! GPU.

use frame_sink::{FrameKey, UserId, perf};
use re_renderer::view_builder::{Projection, TargetConfiguration};
use re_renderer::{
    BlendWithBackground, GpuReadbackIdentifier, RectInt, RectTransform, RenderContext, RenderMode,
    Rgba, ViewBuilder, ViewBuilderId, ViewPickingConfiguration,
};
use xr_math::EyeCamera;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EyeReadbackIdentifiers {
    pub color: GpuReadbackIdentifier,
    pub depth: GpuReadbackIdentifier,
}

impl EyeReadbackIdentifiers {
    const COLOR_TAG: u64 = 0x5141_524f_535f_4330;
    const DEPTH_TAG: u64 = 0x5141_524f_535f_4430;

    /// Distinct for every frame the renderer prepares (`frame_sequence`, counted by the
    /// renderer across all users, low 48 bits) and eye. A user released and admitted again
    /// starts its frame ids over, so the frame key alone could name a frame whose readback is
    /// still pending from before.
    pub fn for_eye(frame_sequence: u64, eye_index: usize) -> Self {
        let eye_bits = u64::try_from(eye_index).unwrap_or(u64::MAX) & 0xff;
        let frame_and_eye = ((frame_sequence & 0xffff_ffff_ffff) << 8) | eye_bits;
        Self {
            color: Self::COLOR_TAG ^ frame_and_eye.rotate_left(17),
            depth: Self::DEPTH_TAG ^ frame_and_eye.rotate_left(17),
        }
    }
}

/// Tagged onto the color readback so the result can be matched to its frame and eye.
#[derive(Clone, Copy, Debug)]
pub struct EyeReadbackTag {
    pub frame_sequence: u64,
    pub eye_index: usize,
}

fn to_iso_transform(pose: xr_math::Pose) -> re_math::IsoTransform {
    re_math::IsoTransform::from_rotation_translation(pose.orientation, pose.position)
}

fn to_rect(rect: xr_math::RectF32) -> re_renderer::RectF32 {
    re_renderer::RectF32 {
        min: rect.min,
        extent: rect.extent,
    }
}

pub fn target_configuration(
    camera: &EyeCamera,
    resolution_in_pixel: [u32; 2],
    eye_name: &str,
    depth_identifier: GpuReadbackIdentifier,
) -> TargetConfiguration {
    let decomposition = camera.decomposition;
    let full_eye = RectInt {
        min: glam::IVec2::ZERO,
        extent: glam::UVec2::from_array(resolution_in_pixel),
    };
    TargetConfiguration {
        name: format!("headset {eye_name} eye").into(),
        render_mode: RenderMode::Beautiful,
        resolution_in_pixel,
        view_from_world: to_iso_transform(camera.view_from_world),
        projection_from_view: Projection::Perspective {
            vertical_fov: decomposition.vertical_fov,
            near_plane_distance: camera.near_plane_in_world_units,
            aspect_ratio: decomposition.aspect_ratio,
        },
        viewport_transformation: RectTransform {
            region_of_interest: to_rect(decomposition.region_of_interest),
            region: to_rect(decomposition.region),
        },
        pixels_per_point: 1.0,
        outline_config: None,
        blend_with_background: BlendWithBackground::No,
        picking_config: Some(ViewPickingConfiguration {
            picking_rect: full_eye,
            readback_identifier: depth_identifier,
            show_debug_view: false,
        }),
    }
}

/// Stable per-user, per-eye identity so `re_renderer`'s per-view caches (transparency sort
/// order) stay with the right eye across frames.
pub fn eye_view_builder_id(view_seed: u64, user: UserId, eye_index: usize) -> ViewBuilderId {
    let eye_bits = u64::try_from(eye_index).unwrap_or(0) + 1;
    let user_bits = u64::from(user.0) << 8;
    ViewBuilderId::new(view_seed ^ (0x4845_4144_5345_5400 | user_bits | eye_bits))
}

/// One eye view to draw, and where to composite it on the GPU path.
pub struct EyeViewSubmission {
    pub key: FrameKey,
    pub view_builder: ViewBuilder,
    /// A texture of `RenderContext::output_format_color` at the eye's resolution; `None` on
    /// the CPU path, where the scheduled screenshot composites and reads back instead.
    pub composite_into: Option<wgpu::Texture>,
}

/// Draws the eye views, composites those with a target, and submits everything on the viewer's
/// queue, returning the frames whose eye failed to draw.
///
/// Called after the viewer's UI pass, once `re_renderer` has submitted the frame's uploads
/// (`RenderContext::before_submit`) and before egui submits the window: the queue order is
/// uploads, eyes, window, all inside one viewer frame.
pub fn draw_and_submit(
    render_context: &RenderContext,
    submissions: Vec<EyeViewSubmission>,
) -> Vec<FrameKey> {
    let encode_span = perf::Span::start("eyes_encode");
    let mut failed_frames = Vec::new();
    let mut command_buffers = Vec::with_capacity(submissions.len() + 1);
    let mut composite_encoder: Option<wgpu::CommandEncoder> = None;
    for submission in submissions {
        let EyeViewSubmission {
            key,
            mut view_builder,
            composite_into,
        } = submission;
        match view_builder.draw(render_context, Rgba::TRANSPARENT) {
            Ok(command_buffer) => command_buffers.push(command_buffer),
            Err(error) => {
                re_log::error_once!("headset: eye draw failed: {error}");
                if !failed_frames.contains(&key) {
                    failed_frames.push(key);
                }
                continue;
            }
        }
        if let Some(target) = composite_into {
            let encoder = composite_encoder.get_or_insert_with(|| {
                render_context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("headset eye composite"),
                    })
            });
            composite(render_context, &view_builder, encoder, &target);
        }
    }
    command_buffers.extend(composite_encoder.map(wgpu::CommandEncoder::finish));
    drop(encode_span);
    if !command_buffers.is_empty() {
        let submit_span = perf::Span::start("eyes_submit");
        let submission = {
            let _submission_lock = re_renderer::queue_submission_lock();
            render_context.queue.submit(command_buffers)
        };
        drop(submit_span);
        if perf::gpu_sync() {
            let _gpu = perf::Span::start("gpu_wait_eyes");
            if let Err(error) = render_context.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            }) {
                re_log::warn_once!("perf: waiting for the eye views: {error}");
            }
        }
    }
    failed_frames
}

/// `ViewBuilder::composite` into `target`, the way Rerun composites a view into the window,
/// but alone in its pass.
pub(crate) fn composite(
    render_context: &RenderContext,
    view_builder: &ViewBuilder,
    encoder: &mut wgpu::CommandEncoder,
    target: &wgpu::Texture,
) {
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("headset eye composite"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    view_builder.composite(render_context, &mut pass);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_identifiers_differ_per_prepared_frame_eye_and_kind() {
        let mut seen = std::collections::HashSet::new();
        for frame_sequence in (0..256).chain([1 << 40, (1 << 48) - 1]) {
            for eye_index in 0..2 {
                let identifiers = EyeReadbackIdentifiers::for_eye(frame_sequence, eye_index);
                assert!(
                    seen.insert(identifiers.color),
                    "{frame_sequence} eye {eye_index}"
                );
                assert!(
                    seen.insert(identifiers.depth),
                    "{frame_sequence} eye {eye_index}"
                );
            }
        }
    }

    #[test]
    fn view_builder_ids_differ_per_user_and_eye() {
        let ids: std::collections::HashSet<_> = (1..=3)
            .flat_map(|user| (0..2).map(move |eye| eye_view_builder_id(42, UserId(user), eye)))
            .collect();
        assert_eq!(ids.len(), 6);
    }

    #[test]
    fn target_configuration_carries_the_decomposed_projection() {
        let fov = xr_math::Fov {
            angle_left: -0.9,
            angle_right: 0.7,
            angle_up: 0.8,
            angle_down: -0.85,
        };
        let depth_range =
            xr_math::DepthRange::new(0.1, 50.0, xr_math::DepthEncoding::ForwardNormalized).unwrap();
        let camera = EyeCamera::new(
            xr_math::Pose::IDENTITY,
            fov,
            xr_math::AppPlacement::default(),
            depth_range,
        )
        .unwrap();
        let config = target_configuration(&camera, [640, 480], "left", 7);

        let rerun_projection = config
            .projection_from_view
            .projection_from_view(config.resolution_in_pixel);
        let rerun_full = config
            .viewport_transformation
            .to_ndc_scale_and_translation()
            * rerun_projection;
        let expected = xr_math::off_axis_infinite_reverse_z(fov, 0.1);
        assert!(
            rerun_full.abs_diff_eq(expected, 1e-5),
            "re_renderer's own math disagrees with the off-axis matrix:\n{rerun_full}\n{expected}"
        );
        assert!(
            config
                .viewport_transformation
                .scale()
                .abs_diff_eq(glam::Vec2::ONE, 1e-6)
        );
        let picking = config.picking_config.unwrap();
        assert_eq!(picking.picking_rect.extent, glam::UVec2::new(640, 480));
        assert_eq!(picking.readback_identifier, 7);
    }
}
