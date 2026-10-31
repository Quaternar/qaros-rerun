use glam::{Quat, Vec3};

use crate::{EYE_COUNT, Fov, Pose, ViewRequest};

/// Supplies eye poses and fields of view where no headset exists.
pub trait PoseSource {
    fn views(&mut self, frame_index: u64) -> [ViewRequest; EYE_COUNT];
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HeadMotion {
    /// Standing still at `position` (eye height included), turned by `yaw` about room Y.
    Still { position: Vec3, yaw: f32 },
    /// Walking a full circle around `center` once per `period_seconds`, always facing it:
    /// the check that an object stays in place while walking around it.
    CircleAround {
        center: Vec3,
        radius: f32,
        eye_height: f32,
        period_seconds: f32,
        /// Where on the circle frame 0 stands, in radians about room Y (0: on the +Z side).
        start_angle: f32,
    },
    /// The circle walk of `CircleAround` with the head also pitched toward `center`, the way a
    /// person keeps a tabletop model in view while walking around it.
    Orbit {
        center: Vec3,
        radius: f32,
        eye_height: f32,
        period_seconds: f32,
        start_angle: f32,
    },
}

/// Deterministic head motion for Mac runs: time comes from the frame index, not the clock,
/// so the same frame always gets the same poses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScriptedHeadMotion {
    pub motion: HeadMotion,
    pub frames_per_second: f32,
    pub interpupillary_distance: f32,
    /// The left eye's field of view; the right eye's is its mirror image.
    pub left_eye_fov: Fov,
}

impl ScriptedHeadMotion {
    /// Illustrative values in the range of current standalone headsets, wider toward the
    /// temple than toward the nose.
    pub const HEADSET_LIKE_LEFT_EYE_FOV: Fov = Fov {
        angle_left: -0.96,
        angle_right: 0.79,
        angle_up: 0.87,
        angle_down: -0.92,
    };

    pub fn new(motion: HeadMotion) -> Self {
        Self {
            motion,
            frames_per_second: 60.0,
            interpupillary_distance: 0.063,
            left_eye_fov: Self::HEADSET_LIKE_LEFT_EYE_FOV,
        }
    }

    pub fn head_pose(&self, frame_index: u64) -> Pose {
        match self.motion {
            HeadMotion::Still { position, yaw } => Pose::new(Quat::from_rotation_y(yaw), position),
            HeadMotion::CircleAround {
                center,
                radius,
                eye_height,
                period_seconds,
                start_angle,
            } => {
                let angle = self.circle_angle(frame_index, start_angle, period_seconds);
                let position = circle_position(center, radius, eye_height, angle);
                Pose::new(Quat::from_rotation_y(angle), position)
            }
            HeadMotion::Orbit {
                center,
                radius,
                eye_height,
                period_seconds,
                start_angle,
            } => {
                let angle = self.circle_angle(frame_index, start_angle, period_seconds);
                let position = circle_position(center, radius, eye_height, angle);
                let pitch = (center.y - eye_height).atan2(radius);
                Pose::new(
                    Quat::from_rotation_y(angle) * Quat::from_rotation_x(pitch),
                    position,
                )
            }
        }
    }

    fn circle_angle(&self, frame_index: u64, start_angle: f32, period_seconds: f32) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let seconds = frame_index as f32 / self.frames_per_second;
        start_angle + std::f32::consts::TAU * seconds / period_seconds
    }

    pub fn right_eye_fov(&self) -> Fov {
        Fov {
            angle_left: -self.left_eye_fov.angle_right,
            angle_right: -self.left_eye_fov.angle_left,
            ..self.left_eye_fov
        }
    }
}

/// Where on a circle of `radius` around `center` the head stands at `angle` (0: on the +Z side).
fn circle_position(center: Vec3, radius: f32, eye_height: f32, angle: f32) -> Vec3 {
    Vec3::new(
        center.x + radius * angle.sin(),
        eye_height,
        center.z + radius * angle.cos(),
    )
}

/// A centered field of view `horizontal_degrees` wide whose vertical extent keeps square pixels
/// at `eye_resolution`: a camera-like eye for renders meant to be looked at on a screen.
pub fn camera_like_fov(horizontal_degrees: f32, eye_resolution: [u32; 2]) -> Fov {
    let half_horizontal_tangent = (horizontal_degrees.to_radians() * 0.5).tan();
    #[allow(clippy::cast_precision_loss)]
    let aspect = eye_resolution[1] as f32 / eye_resolution[0] as f32;
    let half_vertical = (half_horizontal_tangent * aspect).atan();
    Fov::symmetric(horizontal_degrees.to_radians(), half_vertical * 2.0)
}

impl PoseSource for ScriptedHeadMotion {
    fn views(&mut self, frame_index: u64) -> [ViewRequest; EYE_COUNT] {
        let head = self.head_pose(frame_index);
        let half_distance = self.interpupillary_distance * 0.5;
        let eye_at = |offset: f32| head * Pose::from_translation(Vec3::new(offset, 0.0, 0.0));
        [
            ViewRequest {
                pose: eye_at(-half_distance),
                fov: self.left_eye_fov,
            },
            ViewRequest {
                pose: eye_at(half_distance),
                fov: self.right_eye_fov(),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-4;

    fn circle() -> ScriptedHeadMotion {
        ScriptedHeadMotion::new(HeadMotion::CircleAround {
            center: Vec3::new(0.0, 1.0, -2.0),
            radius: 1.5,
            eye_height: 1.6,
            period_seconds: 4.0,
            start_angle: 0.7,
        })
    }

    #[test]
    fn start_angle_places_frame_zero_on_the_circle() {
        let head = circle().head_pose(0);
        let expected = Vec3::new(1.5 * 0.7_f32.sin(), 1.6, -2.0 + 1.5 * 0.7_f32.cos());
        assert!(
            head.position.abs_diff_eq(expected, EPSILON),
            "{}",
            head.position
        );
    }

    #[test]
    fn circling_head_always_faces_the_center() {
        let motion = circle();
        for frame_index in [0, 17, 60, 119, 200] {
            let head = motion.head_pose(frame_index);
            let forward = head.transform_vector(Vec3::NEG_Z);
            let to_center = Vec3::new(0.0, 1.6, -2.0) - head.position;
            assert!(
                forward.abs_diff_eq(to_center.normalize(), EPSILON),
                "{frame_index}"
            );
            assert!((to_center.length() - 1.5).abs() < EPSILON);
        }
    }

    #[test]
    fn orbiting_head_looks_at_the_center_itself() {
        let center = Vec3::new(0.0, 1.0, -2.0);
        let motion = ScriptedHeadMotion::new(HeadMotion::Orbit {
            center,
            radius: 1.5,
            eye_height: 1.6,
            period_seconds: 4.0,
            start_angle: 0.7,
        });
        for frame_index in [0, 17, 60, 119] {
            let head = motion.head_pose(frame_index);
            let forward = head.transform_vector(Vec3::NEG_Z);
            let to_center = (center - head.position).normalize();
            assert!(forward.abs_diff_eq(to_center, EPSILON), "{frame_index}");
            let head_right_axis = head.transform_vector(Vec3::X);
            assert!(
                head_right_axis.y.abs() < EPSILON,
                "eyes stay level: {head_right_axis}"
            );
        }
        let circling = circle();
        assert!(
            motion
                .head_pose(42)
                .position
                .abs_diff_eq(circling.head_pose(42).position, EPSILON)
        );
    }

    #[test]
    fn camera_like_fov_keeps_square_pixels() {
        let fov = camera_like_fov(60.0, [1600, 900]);
        assert!((fov.angle_right - 30_f32.to_radians()).abs() < EPSILON);
        assert!((fov.angle_left + fov.angle_right).abs() < EPSILON);
        assert!((fov.angle_up + fov.angle_down).abs() < EPSILON);
        let horizontal_tangent = fov.angle_right.tan();
        let vertical_tangent = fov.angle_up.tan();
        assert!((vertical_tangent / horizontal_tangent - 900.0 / 1600.0).abs() < EPSILON);
    }

    #[test]
    fn circle_closes_after_one_period() {
        let motion = circle();
        let start = motion.head_pose(0);
        let after_period = motion.head_pose(240);
        assert!(start.position.abs_diff_eq(after_period.position, 1e-3));
    }

    #[test]
    fn eyes_sit_one_interpupillary_distance_apart_along_head_x() {
        let mut motion = ScriptedHeadMotion::new(HeadMotion::Still {
            position: Vec3::new(0.0, 1.6, 0.0),
            yaw: std::f32::consts::FRAC_PI_2,
        });
        let [left, right] = motion.views(0);
        let between = right.pose.position - left.pose.position;
        assert!((between.length() - 0.063).abs() < EPSILON);
        let head_right_axis = motion.head_pose(0).transform_vector(Vec3::X);
        assert!(between.normalize().abs_diff_eq(head_right_axis, EPSILON));
    }

    #[test]
    fn right_eye_fov_mirrors_the_left() {
        let motion = circle();
        let right = motion.right_eye_fov();
        let left = motion.left_eye_fov;
        assert!((right.angle_right + left.angle_left).abs() < EPSILON);
        assert!((right.angle_left + left.angle_right).abs() < EPSILON);
        assert!((right.angle_up - left.angle_up).abs() < EPSILON);
        assert!(right.is_valid());
    }
}
