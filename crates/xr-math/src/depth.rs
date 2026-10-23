//! Depth conversion from Rerun's infinite reverse-Z buffer to what the QAROS mixer reprojects.
//!
//! What QAROS states (header and developer guide at `b1338df`):
//! - `QarRenderFrameShow` carries `rendered_near_far` and `depth_scale` (`qar_streaming.h`
//!   L1996-1999); the defaults are near 0.1, far 100 and `depth_scale` 1.0.
//! - "Report the near/far you actually used in `show_frame`"; wrong values fail silently as
//!   swimming content (`rendering-streams.mdx` L159, `troubleshooting.mdx` L87).
//! - Depth pixels are 32-bit float (`rendering-streams.mdx` L59 uses `R32_FLOAT`, the header
//!   default is `D32_FLOAT`).
//!
//! - "near > far declares reverse-Z" (`QarNearFar`, `qar_streaming.h` L458-460 at `b1338df`),
//!   so [`DepthRange::reported_near_far`] hands the planes over swapped for the reversed
//!   encoding.
//!
//! [`DepthEncoding::ForwardNormalized`] with a finite far plane and `depth_scale` 1.0 is the
//! default, the convention of QuaternAR's `vulkan_cube` sample (forward `[0, 1]` depth, `LESS`
//! compare). The other encodings exist for mixers that read depth differently.

use crate::NearFar;

/// How depth values are written into the QAROS frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DepthEncoding {
    /// D3D/OpenXR style: 0 at the near plane, 1 at the far plane, hyperbolic in between.
    #[default]
    ForwardNormalized,
    /// 1 at the near plane, 0 at the far plane, hyperbolic in between.
    ReverseNormalized,
    /// View-space distance along the optical axis in room meters, clamped to the far plane.
    LinearMeters,
}

/// The near and far planes reported to QAROS, in room meters, with the encoding used to fill
/// the depth texture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthRange {
    pub near_far: NearFar,
    pub encoding: DepthEncoding,
}

impl DepthRange {
    pub fn new(near_plane: f32, far_plane: f32, encoding: DepthEncoding) -> Option<Self> {
        let is_valid = near_plane.is_finite()
            && far_plane.is_finite()
            && near_plane > 0.0
            && far_plane > near_plane;
        is_valid.then_some(Self {
            near_far: NearFar {
                near_plane,
                far_plane,
            },
            encoding,
        })
    }

    pub fn near(self) -> f32 {
        self.near_far.near_plane
    }

    pub fn far(self) -> f32 {
        self.near_far.far_plane
    }

    /// `QarNearFar` as QAROS reads it: near > far declares reverse-Z, so the reversed
    /// encoding reports the far plane as `near_plane` and the near plane as `far_plane`.
    pub fn reported_near_far(self) -> NearFar {
        match self.encoding {
            DepthEncoding::ReverseNormalized => NearFar {
                near_plane: self.far(),
                far_plane: self.near(),
            },
            DepthEncoding::ForwardNormalized | DepthEncoding::LinearMeters => self.near_far,
        }
    }

    /// `QarRenderFrameShow::depth_scale` for this range. Every encoding already carries room
    /// meters through the near and far planes, so the header default applies.
    pub fn depth_scale(self) -> f32 {
        1.0
    }

    /// Distance along the optical axis, in room meters, of a value from Rerun's depth buffer.
    ///
    /// Rerun clears to 0 and writes `near / distance`, where `near` is the near plane used to
    /// render in Rerun-world units. Distances in Rerun-world units and room meters differ by the
    /// app scale, and so do the two near planes, so the ratio is the same in both:
    /// `room_distance = room_near / reverse_depth`.
    pub fn distance_from_reverse_z(self, reverse_depth: f32) -> f32 {
        if reverse_depth > 0.0 {
            self.near() / reverse_depth
        } else {
            f32::INFINITY
        }
    }

    pub fn encode_distance(self, distance: f32) -> f32 {
        let near = self.near();
        let far = self.far();
        let clamped = distance.clamp(near, far);
        match self.encoding {
            DepthEncoding::ForwardNormalized => far * (clamped - near) / (clamped * (far - near)),
            DepthEncoding::ReverseNormalized => near * (far - clamped) / (clamped * (far - near)),
            DepthEncoding::LinearMeters => clamped,
        }
    }

    pub fn decode_distance(self, encoded: f32) -> f32 {
        let near = self.near();
        let far = self.far();
        match self.encoding {
            DepthEncoding::ForwardNormalized => near * far / (far - encoded * (far - near)),
            DepthEncoding::ReverseNormalized => near * far / (near + encoded * (far - near)),
            DepthEncoding::LinearMeters => encoded,
        }
    }

    pub fn encode_from_reverse_z(self, reverse_depth: f32) -> f32 {
        self.encode_distance(self.distance_from_reverse_z(reverse_depth))
    }

    pub fn convert_buffer_from_reverse_z(self, reverse_depths: &[f32]) -> Vec<f32> {
        reverse_depths
            .iter()
            .map(|&reverse_depth| self.encode_from_reverse_z(reverse_depth))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(encoding: DepthEncoding) -> DepthRange {
        DepthRange::new(0.1, 50.0, encoding).unwrap()
    }

    #[test]
    fn rejects_invalid_ranges() {
        assert!(DepthRange::new(0.0, 10.0, DepthEncoding::default()).is_none());
        assert!(DepthRange::new(1.0, 0.5, DepthEncoding::default()).is_none());
        assert!(DepthRange::new(0.1, f32::INFINITY, DepthEncoding::default()).is_none());
    }

    #[test]
    fn forward_normalized_hits_zero_and_one_at_planes() {
        let range = range(DepthEncoding::ForwardNormalized);
        assert!(range.encode_distance(0.1).abs() < 1e-6);
        assert!((range.encode_distance(50.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn reverse_normalized_hits_one_and_zero_at_planes() {
        let range = range(DepthEncoding::ReverseNormalized);
        assert!((range.encode_distance(0.1) - 1.0).abs() < 1e-6);
        assert!(range.encode_distance(50.0).abs() < 1e-6);
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "the planes are copied, not computed")]
    fn only_reverse_normalized_reports_near_greater_than_far() {
        let reverse = range(DepthEncoding::ReverseNormalized).reported_near_far();
        assert_eq!((reverse.near_plane, reverse.far_plane), (50.0, 0.1));
        for encoding in [
            DepthEncoding::ForwardNormalized,
            DepthEncoding::LinearMeters,
        ] {
            let forward = range(encoding).reported_near_far();
            assert_eq!((forward.near_plane, forward.far_plane), (0.1, 50.0));
        }
    }

    #[test]
    fn cleared_rerun_depth_maps_to_far_plane() {
        assert!(
            (range(DepthEncoding::ForwardNormalized).encode_from_reverse_z(0.0) - 1.0).abs() < 1e-6
        );
        assert!(
            range(DepthEncoding::ReverseNormalized)
                .encode_from_reverse_z(0.0)
                .abs()
                < 1e-6
        );
        assert!(
            (range(DepthEncoding::LinearMeters).encode_from_reverse_z(0.0) - 50.0).abs() < 1e-6
        );
    }

    #[test]
    fn beyond_far_plane_clamps() {
        let range = range(DepthEncoding::ForwardNormalized);
        assert!((range.encode_distance(500.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn reverse_z_round_trips_through_every_encoding() {
        for encoding in [
            DepthEncoding::ForwardNormalized,
            DepthEncoding::ReverseNormalized,
            DepthEncoding::LinearMeters,
        ] {
            let range = range(encoding);
            for distance in [0.1_f32, 0.25, 1.0, 2.0, 9.0, 49.0] {
                let reverse_depth = range.near() / distance;
                let encoded = range.encode_from_reverse_z(reverse_depth);
                let decoded = range.decode_distance(encoded);
                assert!(
                    (decoded - distance).abs() < distance * 1e-4,
                    "{encoding:?} {distance}: {decoded}"
                );
            }
        }
    }

    #[test]
    fn forward_normalized_is_monotonic() {
        let range = range(DepthEncoding::ForwardNormalized);
        let samples: Vec<f32> = (1..200)
            .map(|step| range.encode_distance(f32::from(u8::try_from(step).unwrap()) * 0.25))
            .collect();
        assert!(samples.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn buffer_conversion_matches_per_pixel_conversion() {
        let range = range(DepthEncoding::ForwardNormalized);
        let reverse_depths = [0.0, 0.05, 0.1, 1.0];
        let converted = range.convert_buffer_from_reverse_z(&reverse_depths);
        for (reverse_depth, value) in reverse_depths.iter().zip(&converted) {
            assert!((range.encode_from_reverse_z(*reverse_depth) - value).abs() < 1e-7);
        }
    }

    #[test]
    fn app_scale_does_not_change_room_distance() {
        let range = range(DepthEncoding::LinearMeters);
        let app_scale = 0.25;
        let room_distance = 2.0;
        let app_near = range.near() / app_scale;
        let app_distance = room_distance / app_scale;
        let reverse_depth = app_near / app_distance;
        assert!((range.distance_from_reverse_z(reverse_depth) - room_distance).abs() < 1e-5);
    }
}
