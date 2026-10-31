use glam::{Mat4, Vec2, Vec4};

use crate::Fov;

/// Axis-aligned rectangle with the same layout as `re_renderer::RectF32`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF32 {
    pub min: Vec2,
    pub extent: Vec2,
}

impl RectF32 {
    pub fn max(self) -> Vec2 {
        self.min + self.extent
    }
}

/// Off-axis perspective projection for an OpenXR-style field of view, with Rerun's depth
/// convention: right-handed view space looking down minus Z, infinite far plane, reverse Z
/// (depth 1 at the near plane, approaching 0 at infinity).
pub fn off_axis_infinite_reverse_z(fov: Fov, near_plane_distance: f32) -> Mat4 {
    let tan_left = fov.tan_left();
    let tan_right = fov.tan_right();
    let tan_up = fov.tan_up();
    let tan_down = fov.tan_down();
    let tan_width = tan_right - tan_left;
    let tan_height = tan_up - tan_down;

    Mat4::from_cols(
        Vec4::new(2.0 / tan_width, 0.0, 0.0, 0.0),
        Vec4::new(0.0, 2.0 / tan_height, 0.0, 0.0),
        Vec4::new(
            (tan_right + tan_left) / tan_width,
            (tan_up + tan_down) / tan_height,
            0.0,
            -1.0,
        ),
        Vec4::new(0.0, 0.0, near_plane_distance, 0.0),
    )
}

/// The projection `re_renderer` builds for `Projection::Perspective`
/// (`view_builder.rs`, `Projection::projection_from_view` in 0.40.0).
pub fn rerun_perspective_infinite_reverse_z(
    vertical_fov: f32,
    aspect_ratio: f32,
    near_plane_distance: f32,
) -> Mat4 {
    glam::camera::rh::proj::directx::perspective_infinite_reverse(
        vertical_fov,
        aspect_ratio,
        near_plane_distance,
    )
}

/// The matrix `re_renderer` multiplies onto the projection for a viewport transformation
/// (`RectTransform::to_ndc_scale_and_translation` in `transform.rs`, 0.40.0).
pub fn rerun_ndc_scale_and_translation(region_of_interest: RectF32, region: RectF32) -> Mat4 {
    let texcoord_min = (region_of_interest.min - region.min) / region.extent;
    let texcoord_max = texcoord_min + region_of_interest.extent / region.extent;

    let ndc_from_texcoord =
        |texcoord: Vec2| Vec2::new(texcoord.x * 2.0 - 1.0, 1.0 - texcoord.y * 2.0);
    let rect_min_ndc = ndc_from_texcoord(Vec2::new(texcoord_min.x, texcoord_max.y));
    let rect_max_ndc = ndc_from_texcoord(Vec2::new(texcoord_max.x, texcoord_min.y));

    let scale = 2.0 / (rect_max_ndc - rect_min_ndc);
    let translation = -0.5 * (rect_min_ndc + rect_max_ndc);

    Mat4::from_scale(scale.extend(1.0)) * Mat4::from_translation(translation.extend(0.0))
}

/// An off-axis field of view expressed the way `re_renderer` 0.40.0 can draw it: a symmetric
/// perspective that has the same angular extent, shifted by a viewport transformation.
///
/// The symmetric frustum is chosen with exactly the size of the asymmetric one, so the
/// viewport transformation is a pure translation (`RectTransform::scale() == 1`). That keeps
/// `re_renderer`'s per-pixel world size, used for screen-space point and line radii, correct
/// for each eye.
///
/// Rectangles live in tangent space with Y pointing down, matching Rerun's texture-coordinate
/// convention: the frustum edge at angle `a` sits at `tan(a)` horizontally and `-tan(a)`
/// vertically.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SymmetricDecomposition {
    pub vertical_fov: f32,
    pub aspect_ratio: f32,
    pub region_of_interest: RectF32,
    pub region: RectF32,
}

impl SymmetricDecomposition {
    pub fn from_fov(fov: Fov) -> Self {
        let half_tan_width = fov.tan_width() * 0.5;
        let half_tan_height = fov.tan_height() * 0.5;
        Self {
            vertical_fov: 2.0 * half_tan_height.atan(),
            aspect_ratio: half_tan_width / half_tan_height,
            region_of_interest: RectF32 {
                min: Vec2::new(fov.tan_left(), -fov.tan_up()),
                extent: Vec2::new(fov.tan_width(), fov.tan_height()),
            },
            region: RectF32 {
                min: Vec2::new(-half_tan_width, -half_tan_height),
                extent: Vec2::new(2.0 * half_tan_width, 2.0 * half_tan_height),
            },
        }
    }

    /// The full projection `re_renderer` ends up using for this decomposition.
    pub fn projection_from_view(self, near_plane_distance: f32) -> Mat4 {
        rerun_ndc_scale_and_translation(self.region_of_interest, self.region)
            * rerun_perspective_infinite_reverse_z(
                self.vertical_fov,
                self.aspect_ratio,
                near_plane_distance,
            )
    }

    pub fn viewport_scale(self) -> Vec2 {
        self.region_of_interest.extent / self.region.extent
    }
}

/// Where the eye's optical axis (a point straight ahead) lands in an image of the given
/// resolution, in pixels from the top-left corner.
pub fn principal_point_in_pixels(fov: Fov, resolution_in_pixel: [u32; 2]) -> Vec2 {
    #[allow(clippy::cast_precision_loss)]
    let resolution = Vec2::new(resolution_in_pixel[0] as f32, resolution_in_pixel[1] as f32);
    Vec2::new(
        -fov.tan_left() / fov.tan_width(),
        fov.tan_up() / fov.tan_height(),
    ) * resolution
}

#[cfg(test)]
mod tests {
    use glam::{Vec3, Vec4Swizzles};

    use super::*;

    const EPSILON: f32 = 1e-5;

    fn headset_like_fovs() -> Vec<Fov> {
        vec![
            Fov::symmetric(90_f32.to_radians(), 90_f32.to_radians()),
            Fov {
                angle_left: -0.95,
                angle_right: 0.75,
                angle_up: 0.82,
                angle_down: -0.9,
            },
            Fov {
                angle_left: -0.75,
                angle_right: 0.95,
                angle_up: 0.82,
                angle_down: -0.9,
            },
            Fov {
                angle_left: -0.2,
                angle_right: 0.6,
                angle_up: 0.1,
                angle_down: -0.7,
            },
        ]
    }

    fn project(projection: Mat4, point_in_view: Vec3) -> Vec3 {
        let clip = projection * point_in_view.extend(1.0);
        clip.xyz() / clip.w
    }

    fn ndc_to_pixels(ndc: Vec3, resolution: [u32; 2]) -> Vec2 {
        #[allow(clippy::cast_precision_loss)]
        let size = Vec2::new(resolution[0] as f32, resolution[1] as f32);
        Vec2::new((ndc.x + 1.0) * 0.5, (1.0 - ndc.y) * 0.5) * size
    }

    #[test]
    fn decomposition_equals_off_axis_matrix() {
        let near = 0.05;
        for fov in headset_like_fovs() {
            let expected = off_axis_infinite_reverse_z(fov, near);
            let decomposed = SymmetricDecomposition::from_fov(fov).projection_from_view(near);
            assert!(
                expected.abs_diff_eq(decomposed, 1e-5),
                "fov {fov:?}\nexpected {expected}\ngot {decomposed}"
            );
        }
    }

    #[test]
    fn decomposition_keeps_viewport_scale_at_one() {
        for fov in headset_like_fovs() {
            let scale = SymmetricDecomposition::from_fov(fov).viewport_scale();
            assert!(scale.abs_diff_eq(Vec2::ONE, EPSILON), "{scale}");
        }
    }

    #[test]
    fn symmetric_fov_needs_no_viewport_shift() {
        let fov = Fov::symmetric(1.2, 0.9);
        let decomposition = SymmetricDecomposition::from_fov(fov);
        let shift =
            rerun_ndc_scale_and_translation(decomposition.region_of_interest, decomposition.region);
        assert!(shift.abs_diff_eq(Mat4::IDENTITY, EPSILON));
    }

    #[test]
    fn point_straight_ahead_lands_on_principal_point() {
        let resolution = [1832, 1920];
        for fov in headset_like_fovs() {
            let projection = SymmetricDecomposition::from_fov(fov).projection_from_view(0.1);
            for distance in [0.5, 2.0, 40.0] {
                let ndc = project(projection, Vec3::new(0.0, 0.0, -distance));
                let pixel = ndc_to_pixels(ndc, resolution);
                let expected = principal_point_in_pixels(fov, resolution);
                assert!(
                    pixel.abs_diff_eq(expected, 1e-2),
                    "fov {fov:?} distance {distance}: {pixel} vs {expected}"
                );
            }
        }
    }

    #[test]
    fn frustum_edges_map_to_ndc_borders() {
        for fov in headset_like_fovs() {
            let projection = SymmetricDecomposition::from_fov(fov).projection_from_view(0.1);
            let distance = 3.0;
            let left_edge = project(
                projection,
                Vec3::new(fov.tan_left() * distance, 0.0, -distance),
            );
            let right_edge = project(
                projection,
                Vec3::new(fov.tan_right() * distance, 0.0, -distance),
            );
            let top_edge = project(
                projection,
                Vec3::new(0.0, fov.tan_up() * distance, -distance),
            );
            let bottom_edge = project(
                projection,
                Vec3::new(0.0, fov.tan_down() * distance, -distance),
            );
            assert!((left_edge.x + 1.0).abs() < EPSILON);
            assert!((right_edge.x - 1.0).abs() < EPSILON);
            assert!((top_edge.y - 1.0).abs() < EPSILON);
            assert!((bottom_edge.y + 1.0).abs() < EPSILON);
        }
    }

    #[test]
    fn one_meter_cube_at_two_meters_has_expected_angular_size() {
        let fov = Fov::symmetric(90_f32.to_radians(), 90_f32.to_radians());
        let resolution = [2000, 2000];
        let projection = SymmetricDecomposition::from_fov(fov).projection_from_view(0.1);

        let front_face_distance = 1.5;
        let left = ndc_to_pixels(
            project(projection, Vec3::new(-0.5, 0.0, -front_face_distance)),
            resolution,
        );
        let right = ndc_to_pixels(
            project(projection, Vec3::new(0.5, 0.0, -front_face_distance)),
            resolution,
        );
        let width_in_pixels = right.x - left.x;

        let half_angle = (0.5_f32 / front_face_distance).atan();
        let expected_width = 2.0 * half_angle.tan() / fov.tan_width() * 2000.0;
        assert!((width_in_pixels - expected_width).abs() < 1e-2);
        assert!((width_in_pixels - 666.666_7).abs() < 1e-2);
    }

    #[test]
    fn reverse_z_depth_is_near_over_distance() {
        let near = 0.1;
        let projection =
            SymmetricDecomposition::from_fov(headset_like_fovs()[1]).projection_from_view(near);
        for distance in [0.1, 1.0, 7.5, 1000.0] {
            let depth = project(projection, Vec3::new(0.01, -0.02, -distance)).z;
            assert!(
                (depth - near / distance).abs() < 1e-6,
                "{distance}: {depth}"
            );
        }
    }
}
