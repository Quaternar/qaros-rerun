use glam::{Affine3A, Quat, Vec3};

use crate::{CoordinatesError, Pose, ViewCoordinates};

/// How much of the app volume's smallest side an automatic fit gives the data's largest
/// extent. Below 1 / sqrt(2), so a square footprint turned by any yaw still fits the volume.
pub const AUTO_FIT_FILL: f32 = 0.7;

/// Bounds whose largest extent is no more than this share of their distance from Rerun's
/// origin are float noise around a point (a single point logged at map coordinates), not
/// data worth fitting.
pub const MEANINGFUL_EXTENT_RELATIVE: f32 = 1.0e-5;

/// How one recording's data sits in the app volume, before the user's zoom: `center` is the
/// Rerun-world point placed at the app origin and `scale` is app meters per Rerun-world unit.
/// The viewer chooses it per recording (flags, or fitted to the scene's bounds); QAROS never
/// sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DataFit {
    pub center: Vec3,
    pub scale: f32,
}

impl Default for DataFit {
    fn default() -> Self {
        Self {
            center: Vec3::ZERO,
            scale: 1.0,
        }
    }
}

impl DataFit {
    pub fn is_valid(self) -> bool {
        self.center.is_finite() && self.scale.is_finite() && self.scale > 0.0
    }

    /// The fit that centers the bounds `min`..`max` (Rerun-world units) on the app origin and
    /// gives their largest extent [`AUTO_FIT_FILL`] of the smallest side of a volume of
    /// `volume_size` meters. `None` for bounds that are empty, not finite or not meaningful
    /// (see [`MEANINGFUL_EXTENT_RELATIVE`]).
    pub fn enclosing(min: Vec3, max: Vec3, volume_size: Vec3) -> Option<Self> {
        if !bounds_are_meaningful(min, max) || !volume_size.is_finite() {
            return None;
        }
        let smallest_side = volume_size.min_element();
        if smallest_side <= 0.0 {
            return None;
        }
        let fit = Self {
            center: (min + max) * 0.5,
            scale: AUTO_FIT_FILL * smallest_side / (max - min).max_element(),
        };
        fit.is_valid().then_some(fit)
    }
}

/// Finite, not empty, and larger than float noise around their center.
pub fn bounds_are_meaningful(min: Vec3, max: Vec3) -> bool {
    if !(min.is_finite() && max.is_finite()) || min.cmpgt(max).any() {
        return false;
    }
    let largest_extent = (max - min).max_element();
    let center_distance = ((min + max) * 0.5).abs().max_element().max(1.0);
    largest_extent > MEANINGFUL_EXTENT_RELATIVE * center_distance
}

/// Where Rerun's world sits in the QAROS room.
///
/// Mirrors the QAROS app-volume model (`coordinate-systems.md` L45-64): the volume `pose` is
/// the box center in room space, `app_pose` is the app origin in unscaled volume meters, and
/// `app_scale` is room meters per app meter, which this viewer leaves at 1 and a person
/// changes as a zoom (a two-hand spread, the Hub). On top of that, `world_coordinates` says
/// what Rerun's world axes mean (for example `RFU` for Z up), so Rerun's up ends up on the
/// room's Y axis, and `data_fit` places the recording's data in app meters: its center on the
/// app origin, scaled to the volume.
///
/// Room from world = volume pose x app pose x app scale x coordinates x data scale x
/// translation by minus the data center.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppPlacement {
    pub volume_pose: Pose,
    pub app_pose: Pose,
    pub app_scale: f32,
    pub world_coordinates: ViewCoordinates,
    pub data_fit: DataFit,
}

impl Default for AppPlacement {
    fn default() -> Self {
        Self {
            volume_pose: Pose::IDENTITY,
            app_pose: Pose::IDENTITY,
            app_scale: 1.0,
            world_coordinates: ViewCoordinates::RIGHT_UP_BACK,
            data_fit: DataFit::default(),
        }
    }
}

/// The QAROS app volume's part of an [`AppPlacement`]: the volume pose in the room, the app
/// pose in unscaled volume meters and the app scale (room meters per app meter, the user's
/// zoom). QAROS holds these values and a person may change them while the viewer runs.
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

    /// This placement in the app volume `volume`; `world_coordinates` and `data_fit`, the
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

    #[must_use]
    pub fn with_data_fit(self, data_fit: DataFit) -> Self {
        Self { data_fit, ..self }
    }

    /// Room meters per Rerun-world unit: the user's zoom times the recording's data scale.
    pub fn room_meters_per_world_unit(self) -> f32 {
        self.app_scale * self.data_fit.scale
    }

    /// The rigid part of `room_from_world`; the full transform is this pose applied after a
    /// uniform scale by [`Self::room_meters_per_world_unit`]. Uniform scale commutes with
    /// rotation, so the coordinate rotation can be folded into the rigid part, and the center
    /// offset becomes a translation by the scaled center.
    pub fn room_from_world_rigid(self) -> Result<Pose, CoordinatesError> {
        let room_from_coordinates = self.world_coordinates.room_from_coordinates()?;
        Ok(self.volume_pose
            * self.app_pose
            * Pose::new(room_from_coordinates, Vec3::ZERO)
            * Pose::from_translation(-self.room_meters_per_world_unit() * self.data_fit.center))
    }

    pub fn room_from_world(self) -> Result<Affine3A, CoordinatesError> {
        Ok(self.room_from_world_rigid()?.to_affine()
            * Affine3A::from_scale(Vec3::splat(self.room_meters_per_world_unit())))
    }

    /// Pose of an eye in Rerun-world units, given its QAROS pose in the room.
    ///
    /// The eye's own space is shrunk by `1 / room_meters_per_world_unit` along with the room,
    /// which leaves the result rigid (what `re_renderer`'s `view_from_world` accepts) and every
    /// angle intact. Distances seen from this eye are in Rerun-world units.
    pub fn world_from_scaled_eye(self, room_from_eye: Pose) -> Result<Pose, CoordinatesError> {
        let world_rigid_from_eye = self.room_from_world_rigid()?.inverse() * room_from_eye;
        Ok(Pose {
            orientation: world_rigid_from_eye.orientation,
            position: world_rigid_from_eye.position / self.room_meters_per_world_unit(),
        })
    }

    pub fn is_valid(self) -> bool {
        self.volume_transform().is_valid()
            && self.data_fit.is_valid()
            && self.room_meters_per_world_unit().is_normal()
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
            app_scale: 0.5,
            world_coordinates: ViewCoordinates::RIGHT_FORWARD_UP,
            data_fit: DataFit {
                center: Vec3::new(4.0, -2.0, 1.0),
                scale: 0.5,
            },
        }
    }

    fn room_distance(placement: AppPlacement, first: Vec3, second: Vec3) -> f32 {
        let room_from_world = placement.room_from_world().unwrap();
        room_from_world
            .transform_point3(first)
            .distance(room_from_world.transform_point3(second))
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
    fn zoom_times_data_scale_shrinks_a_one_unit_edge() {
        let edge = room_distance(table_placement(), Vec3::ZERO, Vec3::X);
        assert!((edge - 0.25).abs() < EPSILON, "{edge}");
    }

    /// Room from world = volume pose x app pose x zoom x coordinates x data fit: the data
    /// center lands on the app origin, and a unit of data spans zoom x data scale meters.
    #[test]
    fn room_from_world_chains_the_volume_the_zoom_and_the_data_fit() {
        let placement = AppPlacement {
            volume_pose: Pose::from_translation(Vec3::new(0.0, 0.5, -1.7)),
            app_pose: Pose::from_translation(Vec3::new(0.2, 0.0, 0.0)),
            app_scale: 2.0,
            world_coordinates: ViewCoordinates::RIGHT_FORWARD_UP,
            data_fit: DataFit {
                center: Vec3::new(10.0, 20.0, 3.0),
                scale: 0.1,
            },
        };
        let room_from_world = placement.room_from_world().unwrap();
        let center_in_room = room_from_world.transform_point3(Vec3::new(10.0, 20.0, 3.0));
        assert!(
            center_in_room.abs_diff_eq(Vec3::new(0.2, 0.5, -1.7), EPSILON),
            "{center_in_room}"
        );
        let one_unit_up = room_from_world.transform_point3(Vec3::new(10.0, 20.0, 4.0));
        assert!(
            one_unit_up.abs_diff_eq(Vec3::new(0.2, 0.7, -1.7), EPSILON),
            "{one_unit_up}"
        );
        let one_unit_forward = room_from_world.transform_point3(Vec3::new(10.0, 21.0, 3.0));
        assert!(
            one_unit_forward.abs_diff_eq(Vec3::new(0.2, 0.5, -1.9), EPSILON),
            "{one_unit_forward}"
        );
    }

    /// A two-hand spread from 1 to 2 doubles what the room shows, at any data scale: the zoom
    /// no longer starts at the data scale.
    #[test]
    fn a_spread_from_one_to_two_doubles_the_room_size_at_any_data_scale() {
        for data_scale in [1.8e-6, 0.024, 0.18, 2.0] {
            let fitted = AppPlacement {
                data_fit: DataFit {
                    center: Vec3::new(0.16, 1.0, 0.02) / data_scale,
                    scale: data_scale,
                },
                ..AppPlacement::default()
            };
            let spread = AppPlacement {
                app_scale: 2.0,
                ..fitted
            };
            let first = fitted.data_fit.center;
            let second = first + Vec3::splat(0.3 / data_scale);
            let before = room_distance(fitted, first, second);
            let after = room_distance(spread, first, second);
            let expected = 0.3 * 3.0_f32.sqrt();
            assert!(
                (before / expected - 1.0).abs() < 1e-3,
                "{data_scale}: {before}"
            );
            assert!(
                (after / before - 2.0).abs() < 1e-3,
                "{data_scale}: {before} to {after}"
            );
        }
    }

    #[test]
    fn auto_fit_centers_air_traffic_like_bounds_at_table_size() {
        let center = Vec3::new(900_000.0, 5_520_000.0, 120_000.0);
        let half_extent = Vec3::new(500_000.0, 500_000.0, 6_000.0);
        let volume_size = Vec3::splat(2.0);
        let fit =
            DataFit::enclosing(center - half_extent, center + half_extent, volume_size).unwrap();
        assert!(fit.center.abs_diff_eq(center, 1.0), "{:?}", fit.center);
        assert!((fit.scale - 1.4e-6).abs() < 1e-9, "{}", fit.scale);

        let volume_center = Vec3::new(0.0, 0.5, -1.7);
        let placement = AppPlacement {
            volume_pose: Pose::from_translation(volume_center),
            world_coordinates: ViewCoordinates::RIGHT_FORWARD_UP,
            data_fit: fit,
            ..AppPlacement::default()
        }
        .with_up_axis_rotation(0.6);
        let room_from_world = placement.room_from_world().unwrap();
        let fitted_center = room_from_world.transform_point3(center);
        assert!(
            fitted_center.abs_diff_eq(volume_center, 1e-3),
            "{fitted_center}"
        );
        for corner_index in 0..8_u8 {
            let sign = |bit: u8| if corner_index & bit == 0 { -1.0 } else { 1.0 };
            let corner = center + half_extent * Vec3::new(sign(1), sign(2), sign(4));
            let offset = room_from_world.transform_point3(corner) - volume_center;
            assert!(
                offset.abs().max_element() <= volume_size.x * 0.5,
                "corner {corner} lands {offset} from the volume center"
            );
        }
        let width = room_distance(
            placement,
            center - Vec3::X * half_extent.x,
            center + Vec3::X * half_extent.x,
        );
        assert!(
            (width - 1.4).abs() < 1e-3,
            "{width} m across, a table's size"
        );
    }

    #[test]
    fn auto_fit_ignores_empty_non_finite_and_point_like_bounds() {
        let volume_size = Vec3::splat(2.0);
        let nothing = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        let inverted = (Vec3::ONE, Vec3::ZERO);
        let not_a_number = (Vec3::ZERO, Vec3::new(f32::NAN, 1.0, 1.0));
        let single_point = (Vec3::splat(3.0), Vec3::splat(3.0));
        let map_point = (
            Vec3::new(900_000.0, 5_520_000.0, 0.0),
            Vec3::new(900_000.5, 5_520_000.5, 0.0),
        );
        for (min, max) in [nothing, inverted, not_a_number, single_point, map_point] {
            assert_eq!(
                DataFit::enclosing(min, max, volume_size),
                None,
                "{min} {max}"
            );
        }
        let flat_but_wide = DataFit::enclosing(Vec3::ZERO, Vec3::new(4.0, 2.0, 0.0), volume_size);
        assert_eq!(
            flat_but_wide,
            Some(DataFit {
                center: Vec3::new(2.0, 1.0, 0.0),
                scale: 0.35,
            })
        );
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
                in_scaled_eye
                    .abs_diff_eq(in_room_eye / placement.room_meters_per_world_unit(), 1e-4),
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
        assert_eq!(followed.data_fit, requested.data_fit);
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
    fn a_data_fit_with_a_zero_scale_is_not_valid() {
        let placement = AppPlacement::default().with_data_fit(DataFit {
            center: Vec3::ZERO,
            scale: 0.0,
        });
        assert!(!placement.is_valid());
    }
}
