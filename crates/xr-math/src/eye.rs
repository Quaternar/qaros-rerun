use glam::{Mat4, Vec3, Vec4Swizzles};

use crate::{AppPlacement, CoordinatesError, DepthRange, Fov, Pose, SymmetricDecomposition};

#[derive(Clone, Debug, PartialEq)]
pub enum EyeCameraError {
    InvalidFov(Fov),
    InvalidPose(Pose),
    InvalidPlacement,
    Coordinates(CoordinatesError),
}

impl std::fmt::Display for EyeCameraError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFov(fov) => write!(formatter, "invalid field of view {fov:?}"),
            Self::InvalidPose(pose) => write!(formatter, "invalid eye pose {pose:?}"),
            Self::InvalidPlacement => write!(formatter, "invalid app placement"),
            Self::Coordinates(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for EyeCameraError {}

/// Everything `re_renderer` needs to draw one eye: the rigid `view_from_world`, the symmetric
/// perspective plus viewport shift, and the near plane in Rerun-world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EyeCamera {
    pub fov: Fov,
    pub view_from_world: Pose,
    pub decomposition: SymmetricDecomposition,
    pub near_plane_in_world_units: f32,
}

impl EyeCamera {
    pub fn new(
        room_from_eye: Pose,
        fov: Fov,
        placement: AppPlacement,
        depth_range: DepthRange,
    ) -> Result<Self, EyeCameraError> {
        if !fov.is_valid() {
            return Err(EyeCameraError::InvalidFov(fov));
        }
        if !room_from_eye.is_finite() {
            return Err(EyeCameraError::InvalidPose(room_from_eye));
        }
        if !placement.is_valid() {
            return Err(EyeCameraError::InvalidPlacement);
        }
        let world_from_eye = placement
            .world_from_scaled_eye(room_from_eye)
            .map_err(EyeCameraError::Coordinates)?;
        Ok(Self {
            fov,
            view_from_world: world_from_eye.inverse(),
            decomposition: SymmetricDecomposition::from_fov(fov),
            near_plane_in_world_units: depth_range.near() / placement.app_scale,
        })
    }

    pub fn projection_from_view(self) -> Mat4 {
        self.decomposition
            .projection_from_view(self.near_plane_in_world_units)
    }

    pub fn projection_from_world(self) -> Mat4 {
        self.projection_from_view() * self.view_from_world.to_affine()
    }

    /// Normalized device coordinates of a Rerun-world point, Rerun's depth in `z`.
    pub fn project_world_point(self, world_point: Vec3) -> Vec3 {
        let clip = self.projection_from_world() * world_point.extend(1.0);
        clip.xyz() / clip.w
    }
}

#[cfg(test)]
mod tests {
    use glam::{Quat, Vec2};

    use super::*;
    use crate::{DepthEncoding, ViewCoordinates, principal_point_in_pixels};

    const EPSILON: f32 = 1e-4;

    fn depth_range() -> DepthRange {
        DepthRange::new(0.1, 100.0, DepthEncoding::ForwardNormalized).unwrap()
    }

    fn headset_fov() -> Fov {
        Fov {
            angle_left: -0.9,
            angle_right: 0.7,
            angle_up: 0.8,
            angle_down: -0.85,
        }
    }

    fn ndc_to_pixels(ndc: Vec3, resolution: [u32; 2]) -> Vec2 {
        #[allow(clippy::cast_precision_loss)]
        let size = Vec2::new(resolution[0] as f32, resolution[1] as f32);
        Vec2::new((ndc.x + 1.0) * 0.5, (1.0 - ndc.y) * 0.5) * size
    }

    #[test]
    fn z_up_point_ahead_of_a_standing_user_hits_the_principal_point() {
        let placement = AppPlacement {
            world_coordinates: ViewCoordinates::RIGHT_FORWARD_UP,
            ..AppPlacement::default()
        };
        let eye_height = 1.6;
        let room_from_eye = Pose::from_translation(Vec3::new(0.0, eye_height, 0.0));
        let camera =
            EyeCamera::new(room_from_eye, headset_fov(), placement, depth_range()).unwrap();

        let world_point_two_meters_ahead_at_eye_height = Vec3::new(0.0, 2.0, eye_height);
        let ndc = camera.project_world_point(world_point_two_meters_ahead_at_eye_height);
        let resolution = [1600, 1700];
        let pixel = ndc_to_pixels(ndc, resolution);
        assert!(pixel.abs_diff_eq(principal_point_in_pixels(headset_fov(), resolution), 1e-2));
        assert!((ndc.z - 0.1 / 2.0).abs() < 1e-6);
    }

    #[test]
    fn half_app_scale_halves_the_angular_size() {
        let fov = Fov::symmetric(90_f32.to_radians(), 90_f32.to_radians());
        let room_from_eye = Pose::from_translation(Vec3::new(0.0, 0.0, 2.0));

        let width_at = |app_scale: f32| {
            let placement = AppPlacement {
                app_scale,
                ..AppPlacement::default()
            };
            let camera = EyeCamera::new(room_from_eye, fov, placement, depth_range()).unwrap();
            let left = camera.project_world_point(Vec3::new(-0.5, 0.0, 0.0));
            let right = camera.project_world_point(Vec3::new(0.5, 0.0, 0.0));
            right.x - left.x
        };

        let full_size = width_at(1.0);
        assert!((full_size - 2.0 * 0.5 / 2.0 / 1.0).abs() < EPSILON);
        assert!((width_at(0.5) - full_size * 0.5).abs() < EPSILON);
    }

    #[test]
    fn depth_reports_room_distance_regardless_of_scale() {
        let room_from_eye = Pose::from_translation(Vec3::new(0.0, 0.0, 3.0));
        for app_scale in [0.01, 0.5, 1.0, 10.0] {
            let placement = AppPlacement {
                app_scale,
                ..AppPlacement::default()
            };
            let camera =
                EyeCamera::new(room_from_eye, headset_fov(), placement, depth_range()).unwrap();
            let reverse_depth = camera.project_world_point(Vec3::ZERO).z;
            let room_distance = depth_range().distance_from_reverse_z(reverse_depth);
            assert!(
                (room_distance - 3.0).abs() < 1e-3,
                "{app_scale}: {room_distance}"
            );
        }
    }

    #[test]
    fn stereo_eyes_see_a_far_point_at_the_same_pixel() {
        let fov = Fov::symmetric(1.5, 1.5);
        let interpupillary_half = 0.032;
        let left = Pose::from_translation(Vec3::new(-interpupillary_half, 0.0, 0.0));
        let right = Pose::from_translation(Vec3::new(interpupillary_half, 0.0, 0.0));
        let far_point = Vec3::new(0.0, 0.0, -10_000.0);
        let project = |eye| {
            EyeCamera::new(eye, fov, AppPlacement::default(), depth_range())
                .unwrap()
                .project_world_point(far_point)
        };
        assert!((project(left).x - project(right).x).abs() < 1e-4);
    }

    #[test]
    fn turned_head_moves_content_off_center() {
        let room_from_eye = Pose::new(Quat::from_rotation_y(0.3), Vec3::ZERO);
        let camera = EyeCamera::new(
            room_from_eye,
            Fov::symmetric(1.5, 1.5),
            AppPlacement::default(),
            depth_range(),
        )
        .unwrap();
        let ndc = camera.project_world_point(Vec3::new(0.0, 0.0, -2.0));
        assert!(ndc.x > 0.1, "turning left moves a fixed point right: {ndc}");
    }

    #[test]
    fn rejects_invalid_inputs() {
        let bad_fov = Fov::symmetric(-1.0, 1.0);
        assert!(matches!(
            EyeCamera::new(
                Pose::IDENTITY,
                bad_fov,
                AppPlacement::default(),
                depth_range()
            ),
            Err(EyeCameraError::InvalidFov(_))
        ));
        let left_handed = AppPlacement {
            world_coordinates: ViewCoordinates::parse("RFD").unwrap(),
            ..AppPlacement::default()
        };
        assert!(matches!(
            EyeCamera::new(Pose::IDENTITY, headset_fov(), left_handed, depth_range()),
            Err(EyeCameraError::Coordinates(_))
        ));
    }
}
