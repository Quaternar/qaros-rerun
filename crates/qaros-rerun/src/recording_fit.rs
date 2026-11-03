//! How each recording's data is placed in the app volume (ADR-0092): its data fit, chosen once
//! per recording the streamed 3D view shows.
//!
//! - `--auto-fit off`: every recording takes the flags' fit (`--center`, `--scale`, or Rerun's
//!   origin at one meter per unit).
//! - Otherwise, the first recording that shows content takes the flags' fit when `--center` or
//!   `--scale` was given: the recording the viewer was started for. Every other recording is
//!   fitted to its scene bounds once they settle: centered on the volume, its largest extent
//!   [`xr_math::AUTO_FIT_FILL`] of the volume's smallest side.
//!
//! A recording keeps its fit while data streams in, and gets it back when it is shown again.
//! Until a recording is fitted it is shown with the fit in effect before it. The up axis is not
//! part of the fit: it follows the view's coordinates every frame.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use glam::Vec3;
use xr_math::{DataFit, bounds_are_meaningful};

use crate::options::DEFAULT_VOLUME_SIDE_METERS;

/// Bounds that moved by no more than this share of their largest extent count as unchanged.
const SETTLE_TOLERANCE: f32 = 0.05;

/// Bounds unchanged this long have settled: a file opened with File > Open loads in about this
/// time, a live stream's first batch too.
const SETTLE_TIME: Duration = Duration::from_millis(500);

/// Bounds that keep growing (data streaming in) are fitted this long after they first meant
/// something, with what they hold then; the recording is never fitted again.
const SETTLE_DEADLINE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitSettings {
    /// `Some` when `--center` or `--scale` was given; a missing one is 0 or 1.
    pub flag_fit: Option<DataFit>,
    /// `--auto-fit`, on by default.
    pub auto_fit: bool,
    /// The app volume's size in meters (`--volume-size`).
    pub volume_size: Vec3,
}

impl Default for FitSettings {
    fn default() -> Self {
        Self {
            flag_fit: None,
            auto_fit: true,
            volume_size: Vec3::splat(DEFAULT_VOLUME_SIDE_METERS),
        }
    }
}

impl FitSettings {
    fn flag_or_origin_fit(self) -> DataFit {
        self.flag_fit.unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitSource {
    /// `--center` and `--scale`.
    Flags,
    /// Neither flag and `--auto-fit off`: Rerun's origin at one meter per unit.
    Origin,
    /// Fitted to the scene bounds.
    AutoFit,
}

impl FitSource {
    fn describe(self) -> &'static str {
        match self {
            Self::Flags => "the flags' fit (--center, --scale)",
            Self::Origin => "Rerun's origin at one meter per unit (--auto-fit off)",
            Self::AutoFit => "auto-fit to the scene bounds",
        }
    }
}

/// Axis-aligned bounds of a 3D view's scene in Rerun-world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneBounds {
    pub min: Vec3,
    pub max: Vec3,
}

impl SceneBounds {
    pub fn is_meaningful(self) -> bool {
        bounds_are_meaningful(self.min, self.max)
    }

    fn moved_beyond_tolerance_from(self, reference: Self) -> bool {
        let tolerance = SETTLE_TOLERANCE * (reference.max - reference.min).max_element();
        let moved_by = (self.min - reference.min)
            .abs()
            .max((self.max - reference.max).abs())
            .max_element();
        moved_by > tolerance
    }
}

/// The bounds of the recording waiting for its fit, and how long they have stood.
#[derive(Clone, Copy, Debug)]
struct SettlingBounds {
    first_meaningful: Instant,
    unchanged_since: Instant,
    reference: SceneBounds,
}

impl SettlingBounds {
    fn new(bounds: SceneBounds, now: Instant) -> Self {
        Self {
            first_meaningful: now,
            unchanged_since: now,
            reference: bounds,
        }
    }

    fn observe(&mut self, bounds: SceneBounds, now: Instant) {
        if bounds.moved_beyond_tolerance_from(self.reference) {
            self.reference = bounds;
            self.unchanged_since = now;
        }
    }

    fn has_settled(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.unchanged_since) >= SETTLE_TIME
            || now.saturating_duration_since(self.first_meaningful) >= SETTLE_DEADLINE
    }
}

/// The fit of every recording the streamed view showed, by recording.
#[derive(Debug)]
pub struct RecordingFits {
    settings: FitSettings,
    flag_fit_taken: bool,
    fitted: BTreeMap<String, (DataFit, FitSource)>,
    shown_recording: Option<String>,
    settling: Option<SettlingBounds>,
    in_effect: DataFit,
}

impl RecordingFits {
    pub fn new(settings: FitSettings) -> Self {
        Self {
            settings,
            flag_fit_taken: false,
            fitted: BTreeMap::new(),
            shown_recording: None,
            settling: None,
            in_effect: settings.flag_or_origin_fit(),
        }
    }

    /// The fit to draw `recording` with, given its scene `bounds` at `now`; chooses and logs the
    /// recording's fit the first time it can.
    pub fn fit_for(&mut self, recording: &str, bounds: SceneBounds, now: Instant) -> DataFit {
        if self.shown_recording.as_deref() != Some(recording) {
            self.switch_to(recording);
        }
        if self.fitted.contains_key(recording) {
            return self.in_effect;
        }
        if let Some((fit, source)) = self.choose(bounds, now) {
            re_log::info!(
                "headset: recording {recording} placed with {}: {}{}",
                source.describe(),
                describe_fit(fit),
                if source == FitSource::AutoFit {
                    format!(", from scene bounds {} to {}", bounds.min, bounds.max)
                } else {
                    String::new()
                }
            );
            self.fitted.insert(recording.to_owned(), (fit, source));
            self.in_effect = fit;
            self.settling = None;
        }
        self.in_effect
    }

    fn switch_to(&mut self, recording: &str) {
        self.shown_recording = Some(recording.to_owned());
        self.settling = None;
        if let Some((fit, source)) = self.fitted.get(recording).copied() {
            re_log::info!(
                "headset: recording {recording} shown again with its {}: {}",
                source.describe(),
                describe_fit(fit)
            );
            self.in_effect = fit;
        }
    }

    fn choose(&mut self, bounds: SceneBounds, now: Instant) -> Option<(DataFit, FitSource)> {
        if !self.settings.auto_fit {
            let source = if self.settings.flag_fit.is_some() {
                FitSource::Flags
            } else {
                FitSource::Origin
            };
            return Some((self.settings.flag_or_origin_fit(), source));
        }
        if !bounds.is_meaningful() {
            return None;
        }
        if let Some(flag_fit) = self.settings.flag_fit
            && !self.flag_fit_taken
        {
            self.flag_fit_taken = true;
            return Some((flag_fit, FitSource::Flags));
        }
        let settling = self
            .settling
            .get_or_insert_with(|| SettlingBounds::new(bounds, now));
        settling.observe(bounds, now);
        if !settling.has_settled(now) {
            return None;
        }
        DataFit::enclosing(bounds.min, bounds.max, self.settings.volume_size)
            .map(|fit| (fit, FitSource::AutoFit))
    }
}

fn describe_fit(fit: DataFit) -> String {
    format!(
        "center [{}, {}, {}], data scale {:e} m per unit",
        fit.center.x, fit.center.y, fit.center.z, fit.scale
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO_FLAGS: DataFit = DataFit {
        center: Vec3::new(-0.7, 1.3, 1.5),
        scale: 0.18,
    };

    fn air_traffic_bounds() -> SceneBounds {
        let center = Vec3::new(900_000.0, 5_520_000.0, 120_000.0);
        let half_extent = Vec3::new(500_000.0, 500_000.0, 6_000.0);
        SceneBounds {
            min: center - half_extent,
            max: center + half_extent,
        }
    }

    fn robot_arm_bounds() -> SceneBounds {
        SceneBounds {
            min: Vec3::new(-0.4, -0.3, 0.0),
            max: Vec3::new(0.4, 0.25, 0.6),
        }
    }

    fn nothing() -> SceneBounds {
        SceneBounds {
            min: Vec3::splat(f32::INFINITY),
            max: Vec3::splat(f32::NEG_INFINITY),
        }
    }

    fn settings(flag_fit: Option<DataFit>) -> FitSettings {
        FitSettings {
            flag_fit,
            ..FitSettings::default()
        }
    }

    /// Feeds `bounds` every 100 ms from `start` for `duration`, as viewer frames would, and
    /// returns the last fit.
    fn show_for(
        fits: &mut RecordingFits,
        recording: &str,
        bounds: SceneBounds,
        start: Instant,
        duration: Duration,
    ) -> DataFit {
        let step = Duration::from_millis(100);
        let mut elapsed = Duration::ZERO;
        let mut fit = fits.fit_for(recording, bounds, start);
        while elapsed < duration {
            elapsed += step;
            fit = fits.fit_for(recording, bounds, start + elapsed);
        }
        fit
    }

    #[test]
    fn the_start_recording_takes_the_flags_and_a_recording_opened_later_is_auto_fitted() {
        let mut fits = RecordingFits::new(settings(Some(DEMO_FLAGS)));
        let start = Instant::now();
        assert_eq!(
            fits.fit_for("sfm", nothing(), start),
            DEMO_FLAGS,
            "nothing to show yet"
        );
        let sfm_bounds = SceneBounds {
            min: Vec3::new(-4.0, -2.0, -1.0),
            max: Vec3::new(3.0, 4.0, 4.0),
        };
        assert_eq!(fits.fit_for("sfm", sfm_bounds, start), DEMO_FLAGS);

        let opened = start + Duration::from_mins(1);
        let before_settling = fits.fit_for("air_traffic", air_traffic_bounds(), opened);
        assert_eq!(
            before_settling, DEMO_FLAGS,
            "the fit in effect until bounds settle"
        );
        let fitted = show_for(
            &mut fits,
            "air_traffic",
            air_traffic_bounds(),
            opened,
            SETTLE_TIME,
        );
        assert!(
            fitted
                .center
                .abs_diff_eq(Vec3::new(900_000.0, 5_520_000.0, 120_000.0), 1.0),
            "{fitted:?}"
        );
        assert!(
            (fitted.scale - 0.7 * DEFAULT_VOLUME_SIDE_METERS / 1.0e6).abs() < 1e-9,
            "{fitted:?}"
        );
    }

    #[test]
    fn switching_recordings_switches_fits_and_each_keeps_its_own() {
        let mut fits = RecordingFits::new(settings(Some(DEMO_FLAGS)));
        let start = Instant::now();
        let robot_bounds = robot_arm_bounds();
        assert_eq!(fits.fit_for("sfm", robot_bounds, start), DEMO_FLAGS);
        let air_traffic = show_for(
            &mut fits,
            "air_traffic",
            air_traffic_bounds(),
            start,
            SETTLE_TIME,
        );
        let robot_arm = show_for(&mut fits, "robot_arm", robot_bounds, start, SETTLE_TIME);
        assert!(
            (robot_arm.scale - 0.7 * DEFAULT_VOLUME_SIDE_METERS / 0.8).abs() < 1e-5,
            "{robot_arm:?}"
        );
        assert!(
            robot_arm
                .center
                .abs_diff_eq(Vec3::new(0.0, -0.025, 0.3), 1e-6),
            "{robot_arm:?}"
        );

        let later = start + Duration::from_secs(30);
        assert_eq!(
            fits.fit_for("air_traffic", robot_bounds, later),
            air_traffic
        );
        assert_eq!(fits.fit_for("sfm", air_traffic_bounds(), later), DEMO_FLAGS);
        assert_eq!(fits.fit_for("robot_arm", nothing(), later), robot_arm);
    }

    #[test]
    fn growing_bounds_wait_until_they_settle_or_the_deadline() {
        let mut fits = RecordingFits::new(settings(None));
        let start = Instant::now();
        let growing = |step: u16| SceneBounds {
            min: Vec3::ZERO,
            max: Vec3::splat(1.0 + 0.5 * f32::from(step)),
        };
        for step in 0..29_u16 {
            let now = start + Duration::from_millis(100) * u32::from(step);
            assert_eq!(
                fits.fit_for("live", growing(step), now),
                DataFit::default(),
                "still growing at {step}"
            );
        }
        let at_deadline = fits.fit_for("live", growing(30), start + SETTLE_DEADLINE);
        assert!(
            (at_deadline.scale - 0.7 * DEFAULT_VOLUME_SIDE_METERS / 16.0).abs() < 1e-6,
            "fitted with the bounds at the deadline: {at_deadline:?}"
        );
        let much_larger = SceneBounds {
            min: Vec3::ZERO,
            max: Vec3::splat(1000.0),
        };
        assert_eq!(
            show_for(
                &mut fits,
                "live",
                much_larger,
                start + SETTLE_DEADLINE,
                Duration::from_secs(5)
            ),
            at_deadline,
            "fitted once per recording"
        );
    }

    #[test]
    fn point_like_or_empty_bounds_are_not_fitted_and_do_not_take_the_flags() {
        let mut fits = RecordingFits::new(settings(Some(DEMO_FLAGS)));
        let start = Instant::now();
        let one_plane = SceneBounds {
            min: Vec3::new(900_000.0, 5_520_000.0, 11_000.0),
            max: Vec3::new(900_000.0, 5_520_000.0, 11_000.0),
        };
        assert_eq!(
            show_for(&mut fits, "empty", nothing(), start, SETTLE_DEADLINE * 2),
            DEMO_FLAGS
        );
        assert_eq!(
            show_for(
                &mut fits,
                "one_plane",
                one_plane,
                start,
                SETTLE_DEADLINE * 2
            ),
            DEMO_FLAGS
        );
        let robot_arm = show_for(
            &mut fits,
            "robot_arm",
            robot_arm_bounds(),
            start,
            SETTLE_TIME,
        );
        assert_eq!(robot_arm, DEMO_FLAGS, "the first recording with content");
    }

    #[test]
    fn without_auto_fit_every_recording_takes_the_flags_or_the_origin() {
        let flags_only = FitSettings {
            auto_fit: false,
            ..settings(Some(DEMO_FLAGS))
        };
        let mut fits = RecordingFits::new(flags_only);
        let start = Instant::now();
        assert_eq!(fits.fit_for("sfm", nothing(), start), DEMO_FLAGS);
        assert_eq!(
            fits.fit_for("air_traffic", air_traffic_bounds(), start),
            DEMO_FLAGS
        );

        let mut origin_only = RecordingFits::new(FitSettings {
            auto_fit: false,
            ..settings(None)
        });
        assert_eq!(
            origin_only.fit_for("air_traffic", air_traffic_bounds(), start),
            DataFit::default()
        );
    }

    #[test]
    fn without_flags_the_first_recording_is_auto_fitted_too() {
        let mut fits = RecordingFits::new(settings(None));
        let fitted = show_for(
            &mut fits,
            "robot_arm",
            robot_arm_bounds(),
            Instant::now(),
            SETTLE_TIME,
        );
        assert!(
            (fitted.scale - 0.7 * DEFAULT_VOLUME_SIDE_METERS / 0.8).abs() < 1e-5,
            "{fitted:?}"
        );
    }
}
