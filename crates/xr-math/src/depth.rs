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
//! - "near > far declares reverse-Z" (`QarNearFar`, `qar_streaming.h` L715-717 at `e0c0195`),
//!   so [`DepthRange::reported_near_far`] hands the planes over swapped for the reversed
//!   encodings.
//!
//! What the QAROS implementation does beyond the header (qar-streaming at `e487a31`): under
//! reverse-Z a declared `near_plane` above 1e20 is an infinite far plane (`IsInfiniteFar`,
//! `PixelShaderHelper.hpp` L397-404), and such depth is linearized as `far_plane / depth`
//! (`LinearizeDepth`, `DepthNearFarConvertShared.slang` L15-20), which is Rerun's
//! `near / distance` read back.
//!
//! [`DepthEncoding::ReverseInfinite`] is the default (ADR-0084): Rerun's depth reaches QAROS
//! unchanged and QAROS converts it once. The finite-far encodings remain selectable.

use crate::NearFar;

/// The `near_plane` reported with [`DepthEncoding::ReverseInfinite`], in room meters.
///
/// QAROS reads a reverse-Z `near_plane` above 1e20 as an infinite far plane. The QAROS sink
/// multiplies the planes by `1 / app_scale` on the way to app units, so the marker stays far
/// enough below `f32::MAX` to remain finite there: no shader or log downstream meets an
/// infinity.
pub const INFINITE_FAR_PLANE_MARKER: f32 = 1.0e30;

/// How depth values are written into the QAROS frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DepthEncoding {
    /// Rerun's infinite reverse-Z as it is: `near / distance`, 0 where nothing was drawn.
    /// Reported as reverse-Z with an infinite far plane, so the far plane is not used.
    #[default]
    ReverseInfinite,
    /// D3D/OpenXR style: 0 at the near plane, 1 at the far plane, hyperbolic in between.
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
    /// encodings report the far plane as `near_plane` and the near plane as `far_plane`, the
    /// infinite one as [`INFINITE_FAR_PLANE_MARKER`].
    pub fn reported_near_far(self) -> NearFar {
        match self.encoding {
            DepthEncoding::ReverseInfinite => NearFar {
                near_plane: INFINITE_FAR_PLANE_MARKER,
                far_plane: self.near(),
            },
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
            // No far plane to clamp to: infinitely far is 0, Rerun's background.
            DepthEncoding::ReverseInfinite => near / distance.max(near),
            DepthEncoding::ForwardNormalized => far * (clamped - near) / (clamped * (far - near)),
            DepthEncoding::ReverseNormalized => near * (far - clamped) / (clamped * (far - near)),
            DepthEncoding::LinearMeters => clamped,
        }
    }

    pub fn decode_distance(self, encoded: f32) -> f32 {
        let near = self.near();
        let far = self.far();
        match self.encoding {
            DepthEncoding::ReverseInfinite => self.distance_from_reverse_z(encoded),
            DepthEncoding::ForwardNormalized => near * far / (far - encoded * (far - near)),
            DepthEncoding::ReverseNormalized => near * far / (near + encoded * (far - near)),
            DepthEncoding::LinearMeters => encoded,
        }
    }

    /// The value written for a pixel of Rerun's depth buffer. Rerun's own value is written
    /// as it is when the encoding is Rerun's, so not even rounding changes it.
    pub fn encode_from_reverse_z(self, reverse_depth: f32) -> f32 {
        match self.encoding {
            DepthEncoding::ReverseInfinite => reverse_depth,
            DepthEncoding::ForwardNormalized
            | DepthEncoding::ReverseNormalized
            | DepthEncoding::LinearMeters => {
                self.encode_distance(self.distance_from_reverse_z(reverse_depth))
            }
        }
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
    fn reverse_infinite_is_the_default() {
        assert_eq!(DepthEncoding::default(), DepthEncoding::ReverseInfinite);
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "the planes are copied, not computed")]
    fn reverse_infinite_reports_an_infinite_far_plane_as_near_and_the_near_plane_as_far() {
        let reported = range(DepthEncoding::ReverseInfinite).reported_near_far();
        assert_eq!((reported.near_plane, reported.far_plane), (1.0e30, 0.1));
        assert!(reported.near_plane > 1.0e20 && reported.near_plane.is_finite());
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "Rerun's value is copied, not computed")]
    fn reverse_infinite_writes_rerun_depth_unchanged() {
        let range = range(DepthEncoding::ReverseInfinite);
        let reverse_depths = [0.0, 1.0e-7, 0.002, 0.1, 0.5, 1.0];
        assert_eq!(
            range.convert_buffer_from_reverse_z(&reverse_depths),
            reverse_depths
        );
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "divisions by a power of two are exact")]
    fn reverse_infinite_encodes_past_the_far_plane_without_clamping() {
        let range = DepthRange::new(0.125, 2.0, DepthEncoding::ReverseInfinite).unwrap();
        assert_eq!(range.encode_distance(0.125), 1.0);
        assert_eq!(range.encode_distance(1.0), 0.125);
        assert_eq!(range.encode_distance(64.0), 0.001_953_125);
        assert_eq!(range.encode_distance(f32::INFINITY), 0.0);
        assert_eq!(range.decode_distance(0.001_953_125), 64.0);
        assert_eq!(range.decode_distance(0.0), f32::INFINITY);
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "the planes are copied, not computed")]
    fn reverse_normalized_reports_near_greater_than_far_and_forward_does_not() {
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
        assert!(
            range(DepthEncoding::ReverseInfinite)
                .encode_from_reverse_z(0.0)
                .abs()
                < f32::EPSILON
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
            DepthEncoding::ReverseInfinite,
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
