use glam::{Affine3A, Quat, Vec3};

use crate::{CoordinatesError, Pose, ViewCoordinates};

/// Where Rerun's world sits in the QAROS room.
///
/// Mirrors the QAROS app-volume model (`coordinate-systems.md` L45-64): the volume `pose` is
/// the box center in room space, `app_pose` is the app origin in unscaled volume meters, and
/// `app_scale` is room meters per app meter. On top of that, `world_coordinates` says what
/// Rerun's world axes mean (for example `RFU` for Z up), so Rerun's up ends up on the room's
/// Y axis. `world_center` is the Rerun-world point that lands on the app origin, so data
/// logged far from Rerun's origin can still sit in the middle of the volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppPlacement {
    pub volume_pose: Pose,
    pub app_pose: Pose,
    pub app_scale: f32,
    pub world_coordinates: ViewCoordinates,
    pub world_center: Vec3,
}

impl Default for AppPlacement {
    fn default() -> Self {
        Self {
            volume_pose: Pose::IDENTITY,
            app_pose: Pose::IDENTITY,
            app_scale: 1.0,
            world_coordinates: ViewCoordinates::RIGHT_UP_BACK,
            world_center: Vec3::ZERO,
        }
    }
}

/// The QAROS app volume's part of an [`AppPlacement`]: the volume pose in the room, the app
/// pose in unscaled volume meters and the app scale (room meters per app meter). QAROS holds
/// these values and a person may change them in the Hub while the viewer runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeTransform {
    pub volume_pose: Pose,
    pub app_pose: Pose,
    pub app_scale: f32,
}

impl VolumeTransform {
    pub fn room_from_app(self) -> Pose {
        self.volume_pose * self.app_pose
    }

    pub fn is_valid(self) -> bool {
        self.app_scale.is_finite()
            && self.app_scale > 0.0
            && self.volume_pose.is_finite()
            && self.app_pose.is_finite()
    }
}

impl AppPlacement {
    pub fn volume_transform(self) -> VolumeTransform {
        VolumeTransform {
            volume_pose: self.volume_pose,
            app_pose: self.app_pose,
            app_scale: self.app_scale,
        }
    }

    /// This placement in the app volume `volume`; `world_coordinates` and `world_center`, the
    /// viewer's own placement of Rerun's world, stay.
    #[must_use]
    pub fn with_volume_transform(self, volume: VolumeTransform) -> Self {
        Self {
            volume_pose: volume.volume_pose,
            app_pose: volume.app_pose,
            app_scale: volume.app_scale,
            ..self
        }
    }

    /// The rigid part of `room_from_world`; the full transform is this pose applied after a
    /// uniform scale by `app_scale`. Uniform scale commutes with rotation, so the coordinate
    /// rotation can be folded into the rigid part, and the center offset becomes a
    /// translation by the scaled center.
    pub fn room_from_world_rigid(self) -> Result<Pose, CoordinatesError> {
        let room_from_coordinates = self.world_coordinates.room_from_coordinates()?;
        Ok(self.volume_pose
            * self.app_pose
            * Pose::new(room_from_coordinates, Vec3::ZERO)
            * Pose::from_translation(-self.app_scale * self.world_center))
    }

    pub fn room_from_world(self) -> Result<Affine3A, CoordinatesError> {
        Ok(self.room_from_world_rigid()?.to_affine()
            * Affine3A::from_scale(Vec3::splat(self.app_scale)))
    }

    /// Pose of an eye in Rerun-world units, given its QAROS pose in the room.
    ///
    /// The eye's own space is shrunk by `1 / app_scale` along with the room, which leaves the
    /// result rigid (what `re_renderer`'s `view_from_world` accepts) and every angle intact.
    /// Distances seen from this eye are in Rerun-world units: room meters divided by
    /// `app_scale`.
    pub fn world_from_scaled_eye(self, room_from_eye: Pose) -> Result<Pose, CoordinatesError> {
        let world_rigid_from_eye = self.room_from_world_rigid()?.inverse() * room_from_eye;
        Ok(Pose {
            orientation: world_rigid_from_eye.orientation,
            position: world_rigid_from_eye.position / self.app_scale,
        })
    }

    pub fn is_valid(self) -> bool {
        self.volume_transform().is_valid() && self.world_center.is_finite()
    }

    #[must_use]
    pub fn with_up_axis_rotation(self, yaw_radians: f32) -> Self {
        Self {
            app_pose: Pose::new(Quat::from_rotation_y(yaw_radians), Vec3::ZERO) * self.app_pose,
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-5;

    fn table_placement() -> AppPlacement {
        AppPlacement {
            volume_pose: Pose::new(Quat::from_rotation_y(0.4), Vec3::new(0.5, 0.9, -1.5)),
            app_pose: Pose::new(Quat::from_rotation_y(-0.1), Vec3::new(0.0, -0.3, 0.0)),
            app_scale: 0.25,
            world_coordinates: ViewCoordinates::RIGHT_FORWARD_UP,
            world_center: Vec3::new(4.0, -2.0, 1.0),
        }
    }

    #[test]
    fn identity_placement_keeps_points() {
        let room_from_world = AppPlacement::default().room_from_world().unwrap();
        let point = Vec3::new(1.0, 2.0, 3.0);
        assert!(
            room_from_world
                .transform_point3(point)
                .abs_diff_eq(point, EPSILON)
        );
    }

    #[test]
    fn z_up_world_point_lands_above_room_origin() {
        let placement = AppPlacement {
            world_coordinates: ViewCoordinates::RIGHT_FORWARD_UP,
            ..AppPlacement::default()
        };
        let room_point = placement
            .room_from_world()
            .unwrap()
            .transform_point3(Vec3::new(0.0, 2.0, 1.0));
        assert!(room_point.abs_diff_eq(Vec3::new(0.0, 1.0, -2.0), EPSILON));
    }

    #[test]
    fn app_scale_shrinks_a_one_meter_edge() {
        let placement = table_placement();
        let room_from_world = placement.room_from_world().unwrap();
        let edge_start = room_from_world.transform_point3(Vec3::ZERO);
        let edge_end = room_from_world.transform_point3(Vec3::X);
        assert!((edge_start.distance(edge_end) - 0.25).abs() < EPSILON);
    }

    #[test]
    fn scaled_eye_sees_the_same_directions_as_the_room_eye() {
        let placement = table_placement();
        let room_from_world = placement.room_from_world().unwrap();
        let room_from_eye = Pose::new(
            Quat::from_euler(glam::EulerRot::YXZ, 0.3, -0.2, 0.05),
            Vec3::new(0.1, 1.6, 0.4),
        );
        let world_from_eye = placement.world_from_scaled_eye(room_from_eye).unwrap();

        for world_point in [
            Vec3::ZERO,
            Vec3::new(1.0, 2.0, 0.5),
            Vec3::new(-3.0, 0.2, 1.0),
        ] {
            let room_point = room_from_world.transform_point3(world_point);
            let in_room_eye = room_from_eye.inverse().transform_point(room_point);
            let in_scaled_eye = world_from_eye.inverse().transform_point(world_point);
            assert!(
                in_scaled_eye.abs_diff_eq(in_room_eye / placement.app_scale, 1e-4),
                "{world_point}: {in_scaled_eye} vs {in_room_eye}"
            );
        }
    }

    #[test]
    fn rejects_non_positive_scale() {
        let placement = AppPlacement {
            app_scale: 0.0,
            ..AppPlacement::default()
        };
        assert!(!placement.is_valid());
        assert!(AppPlacement::default().is_valid());
    }

    #[test]
    fn up_axis_rotation_turns_content_about_room_y() {
        let placement = AppPlacement::default().with_up_axis_rotation(std::f32::consts::FRAC_PI_2);
        let room_point = placement
            .room_from_world()
            .unwrap()
            .transform_point3(Vec3::NEG_Z);
        assert!(room_point.abs_diff_eq(Vec3::NEG_X, EPSILON));
    }

    #[test]
    fn a_volume_transform_replaces_the_volume_part_and_keeps_the_world_placement() {
        let requested = table_placement();
        let live = VolumeTransform {
            volume_pose: Pose::from_translation(Vec3::new(-1.0, 0.8, -2.0)),
            app_pose: Pose::new(Quat::from_rotation_y(1.2), Vec3::new(0.1, 0.0, 0.0)),
            app_scale: 0.001,
        };
        let followed = requested.with_volume_transform(live);
        assert_eq!(followed.volume_transform(), live);
        assert_eq!(followed.world_coordinates, requested.world_coordinates);
        assert_eq!(followed.world_center, requested.world_center);
        assert!(followed.is_valid());
        assert!(
            !VolumeTransform {
                app_scale: f32::NAN,
                ..live
            }
            .is_valid()
        );
    }

    #[test]
    fn world_center_lands_on_the_volume_center() {
        let placement = AppPlacement {
            volume_pose: Pose::from_translation(Vec3::new(0.0, 1.0, -1.5)),
            app_scale: 0.01,
            world_coordinates: ViewCoordinates::RIGHT_FORWARD_UP,
            world_center: Vec3::new(120.0, -40.0, 15.0),
            ..AppPlacement::default()
        };
        let room_from_world = placement.room_from_world().unwrap();
        let center_in_room = room_from_world.transform_point3(placement.world_center);
        assert!(center_in_room.abs_diff_eq(Vec3::new(0.0, 1.0, -1.5), EPSILON));

        let one_unit_up = room_from_world.transform_point3(placement.world_center + Vec3::Z);
        assert!(one_unit_up.abs_diff_eq(Vec3::new(0.0, 1.01, -1.5), EPSILON));
    }
}
