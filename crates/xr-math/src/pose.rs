use glam::{Affine3A, Quat, Vec3};

/// Rigid transform with the layout of `QarPose`: orientation as `{x, y, z, w}` (scalar last),
/// then position. A QAROS view pose is `room_from_eye`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub orientation: Quat,
    pub position: Vec3,
}

impl Default for Pose {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Pose {
    pub const IDENTITY: Self = Self {
        orientation: Quat::IDENTITY,
        position: Vec3::ZERO,
    };

    pub fn new(orientation: Quat, position: Vec3) -> Self {
        Self {
            orientation: orientation.normalize(),
            position,
        }
    }

    pub fn from_xyzw_and_position(orientation_xyzw: [f32; 4], position: [f32; 3]) -> Self {
        Self::new(
            Quat::from_array(orientation_xyzw),
            Vec3::from_array(position),
        )
    }

    pub fn from_translation(position: Vec3) -> Self {
        Self::new(Quat::IDENTITY, position)
    }

    #[must_use]
    pub fn inverse(self) -> Self {
        let inverse_orientation = self.orientation.inverse();
        Self {
            orientation: inverse_orientation,
            position: -(inverse_orientation * self.position),
        }
    }

    pub fn transform_point(self, point: Vec3) -> Vec3 {
        self.orientation * point + self.position
    }

    pub fn transform_vector(self, vector: Vec3) -> Vec3 {
        self.orientation * vector
    }

    pub fn to_affine(self) -> Affine3A {
        Affine3A::from_rotation_translation(self.orientation, self.position)
    }

    pub fn is_finite(self) -> bool {
        self.orientation.is_finite() && self.position.is_finite()
    }
}

impl std::ops::Mul for Pose {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self {
        Self::new(
            self.orientation * rhs.orientation,
            self.transform_point(rhs.position),
        )
    }
}

/// Field of view as four angles in radians, OpenXR style (`XrFovf`): left and down are
/// usually negative, right and up positive. Same field order as `QarFov`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fov {
    pub angle_left: f32,
    pub angle_right: f32,
    pub angle_up: f32,
    pub angle_down: f32,
}

impl Fov {
    pub fn symmetric(horizontal_fov: f32, vertical_fov: f32) -> Self {
        Self {
            angle_left: -horizontal_fov * 0.5,
            angle_right: horizontal_fov * 0.5,
            angle_up: vertical_fov * 0.5,
            angle_down: -vertical_fov * 0.5,
        }
    }

    pub fn tan_left(self) -> f32 {
        self.angle_left.tan()
    }

    pub fn tan_right(self) -> f32 {
        self.angle_right.tan()
    }

    pub fn tan_up(self) -> f32 {
        self.angle_up.tan()
    }

    pub fn tan_down(self) -> f32 {
        self.angle_down.tan()
    }

    pub fn tan_width(self) -> f32 {
        self.tan_right() - self.tan_left()
    }

    pub fn tan_height(self) -> f32 {
        self.tan_up() - self.tan_down()
    }

    /// The aspect ratio that gives square pixels for this field of view.
    pub fn aspect_ratio(self) -> f32 {
        self.tan_width() / self.tan_height()
    }

    pub fn is_valid(self) -> bool {
        let half_pi = std::f32::consts::FRAC_PI_2;
        let angles = [
            self.angle_left,
            self.angle_right,
            self.angle_up,
            self.angle_down,
        ];
        angles
            .iter()
            .all(|angle| angle.is_finite() && angle.abs() < half_pi)
            && self.angle_left < self.angle_right
            && self.angle_down < self.angle_up
    }
}

/// Mirrors `QarNearFar`: the near and far plane distances, in room meters, reported with
/// every shown frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NearFar {
    pub near_plane: f32,
    pub far_plane: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-5;

    fn sample_pose() -> Pose {
        Pose::new(
            Quat::from_euler(glam::EulerRot::YXZ, 0.7, -0.3, 0.2),
            Vec3::new(1.0, 1.6, -0.5),
        )
    }

    #[test]
    fn inverse_round_trips_a_point() {
        let pose = sample_pose();
        let point = Vec3::new(0.3, -2.0, 4.0);
        let round_tripped = pose.inverse().transform_point(pose.transform_point(point));
        assert!(round_tripped.abs_diff_eq(point, EPSILON));
    }

    #[test]
    fn composition_with_inverse_is_identity() {
        let pose = sample_pose();
        let identity = pose * pose.inverse();
        assert!(identity.position.abs_diff_eq(Vec3::ZERO, EPSILON));
        assert!(identity.orientation.abs_diff_eq(Quat::IDENTITY, EPSILON));
    }

    #[test]
    fn composition_matches_affine_product() {
        let first = sample_pose();
        let second = Pose::new(Quat::from_rotation_z(1.1), Vec3::new(-2.0, 0.5, 0.25));
        let point = Vec3::new(0.1, 0.2, 0.3);
        let by_pose = (first * second).transform_point(point);
        let by_affine = (first.to_affine() * second.to_affine()).transform_point3(point);
        assert!(by_pose.abs_diff_eq(by_affine, EPSILON));
    }

    #[test]
    fn xyzw_constructor_keeps_scalar_last() {
        let half_turn_about_y = Pose::from_xyzw_and_position([0.0, 1.0, 0.0, 0.0], [0.0; 3]);
        let rotated = half_turn_about_y.transform_vector(Vec3::NEG_Z);
        assert!(rotated.abs_diff_eq(Vec3::Z, EPSILON));
    }

    #[test]
    fn symmetric_fov_has_matching_tangents() {
        let fov = Fov::symmetric(90_f32.to_radians(), 60_f32.to_radians());
        assert!((fov.tan_right() - 1.0).abs() < EPSILON);
        assert!((fov.tan_left() + 1.0).abs() < EPSILON);
        assert!((fov.tan_up() - 30_f32.to_radians().tan()).abs() < EPSILON);
        assert!((fov.aspect_ratio() - 1.0 / 30_f32.to_radians().tan()).abs() < 1e-4);
        assert!(fov.is_valid());
    }

    #[test]
    fn inverted_fov_is_invalid() {
        let fov = Fov {
            angle_left: 0.5,
            angle_right: -0.5,
            angle_up: 0.5,
            angle_down: -0.5,
        };
        assert!(!fov.is_valid());
    }
}
