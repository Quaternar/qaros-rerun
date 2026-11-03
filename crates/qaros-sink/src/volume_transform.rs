//! The app volume's transform as QAROS holds it, which may differ from the one the viewer
//! asked for: `get_or_create` hands back an existing volume as it is, and a person can move,
//! turn and scale the volume in the Hub while the viewer runs. The sinks map every frame's
//! poses, and the renderer places Rerun's world, with the transform followed here (ADR-0066).

use std::ops::RangeInclusive;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use qaros::{AppVolumeId, AppVolumeInfo, Session};
use xr_math::{Pose, VolumeTransform};

use crate::connection::from_qaros_pose;
use crate::remembered_placement::PlacementMemory;

/// How often the latest values are read again when no update arrived: the header calls the
/// pose and app scale setters "low-latency fast-path" updates and does not say whether they
/// reach the update subscription.
pub(crate) const LATEST_VALUES_POLL_PERIOD: Duration = Duration::from_millis(100);

/// The transform the frames are mapped with, logged once at every change.
#[derive(Debug)]
pub(crate) struct FollowedVolume {
    common_name: String,
    current: VolumeTransform,
}

impl FollowedVolume {
    /// Starts from the live transform when it can be read and is valid, else from the
    /// requested one; a live transform that differs from the requested one is logged with both.
    pub(crate) fn new(
        common_name: &str,
        requested: VolumeTransform,
        live: Result<VolumeTransform, String>,
    ) -> Self {
        let current = match live {
            Ok(live) if !live.is_valid() => {
                log::warn!(
                    "qaros: app volume {common_name:?} reports an unusable transform ({}); mapping with the requested one ({})",
                    describe(live),
                    describe(requested)
                );
                requested
            }
            Ok(live) => {
                if live != requested {
                    log::info!(
                        "qaros: app volume {common_name:?} is held by QAROS with another transform than requested; mapping with QAROS's: requested {}, live {}",
                        describe(requested),
                        describe(live)
                    );
                }
                live
            }
            Err(error) => {
                log::warn!(
                    "qaros: cannot read app volume {common_name:?}'s transform ({error}); mapping with the requested one ({})",
                    describe(requested)
                );
                requested
            }
        };
        Self {
            common_name: common_name.to_owned(),
            current,
        }
    }

    pub(crate) fn current(&self) -> VolumeTransform {
        self.current
    }

    /// Takes `live` when it is valid and differs from the current transform.
    pub(crate) fn follow(&mut self, live: VolumeTransform) {
        if live == self.current {
            return;
        }
        if !live.is_valid() {
            log::warn!(
                "qaros: app volume {:?} reports an unusable transform ({}); keeping {}",
                self.common_name,
                describe(live),
                describe(self.current)
            );
            return;
        }
        log::info!(
            "qaros: app volume {:?} changed from {} to {}; frames begun from now on map with it",
            self.common_name,
            describe(self.current),
            describe(live)
        );
        self.current = live;
    }
}

/// The followed transform with what feeds it: the latest values, read again on an update for
/// this volume or once the poll period passed. An update only triggers the read; its own values
/// are never applied, because the Hub can deliver updates out of order and the last one can
/// carry an older transform than the latest values. Each followed transform is remembered for
/// the next run when a memory is given (ADR-0085).
#[derive(Debug)]
pub(crate) struct LiveVolume {
    followed: FollowedVolume,
    updates: Receiver<AppVolumeInfo>,
    next_read: Instant,
    read_failing: bool,
    placement_memory: Option<PlacementMemory>,
}

impl LiveVolume {
    pub(crate) fn new(
        followed: FollowedVolume,
        updates: Receiver<AppVolumeInfo>,
        mut placement_memory: Option<PlacementMemory>,
    ) -> Self {
        let now = Instant::now();
        if let Some(memory) = &mut placement_memory {
            memory.remember_soon(followed.current(), now);
        }
        Self {
            followed,
            updates,
            next_read: now + LATEST_VALUES_POLL_PERIOD,
            read_failing: false,
            placement_memory,
        }
    }

    /// Reads the latest transform once more and stores it for the next run; at Stop, after
    /// the volume is hidden, which it never closes (ADR-0093).
    pub(crate) fn remember_before_close(&mut self, session: &Session, volume: AppVolumeId) {
        let last = self.refresh(session, volume, true);
        if let Some(memory) = &mut self.placement_memory {
            memory.remember_now(last);
        }
    }

    /// The transform for the frames begun now. Runs on the thread that owns the session.
    /// `read_now` forces a read, for frames begun while a gesture moves the volume.
    pub(crate) fn refresh(
        &mut self,
        session: &Session,
        volume: AppVolumeId,
        read_now: bool,
    ) -> VolumeTransform {
        let mut volume_updated = read_now;
        for update in self.updates.try_iter() {
            volume_updated |= update.id == volume;
        }
        let now = Instant::now();
        if !volume_updated && now < self.next_read {
            return self.followed.current();
        }
        self.next_read = now + LATEST_VALUES_POLL_PERIOD;
        match read_latest(session, volume) {
            Ok(latest) => {
                self.read_failing = false;
                self.followed.follow(latest);
                if let Some(memory) = &mut self.placement_memory {
                    memory.remember_soon(self.followed.current(), now);
                }
            }
            Err(error) => {
                if !self.read_failing {
                    log::warn!(
                        "qaros: cannot read the app volume's latest transform ({error}); keeping the current one until a read succeeds"
                    );
                }
                self.read_failing = true;
            }
        }
        self.followed.current()
    }
}

/// The latest locally known values (`qar_app_volume_get_latest_*`), which include changes by
/// other peers and gestures.
pub(crate) fn read_latest(
    session: &Session,
    volume: AppVolumeId,
) -> qaros::Result<VolumeTransform> {
    let volumes = session.app_volumes();
    Ok(VolumeTransform {
        volume_pose: from_qaros_pose(volumes.latest_pose(volume)?),
        app_pose: from_qaros_pose(volumes.latest_app_pose(volume)?),
        app_scale: volumes.latest_app_scale(volume)?,
    })
}

/// The app scales a person sets as a zoom (ADR-0092): the viewer asks for 1, and a two-hand
/// spread or the Hub changes it from there. A taken-over or remembered app scale outside this
/// range is not a zoom worth keeping: the Hub clamps at 0.001, and builds before ADR-0092 set
/// the data scale (1.8e-6 for air traffic) as the app scale.
pub(crate) const USER_ZOOM_RANGE: RangeInclusive<f32> = 0.1..=10.0;

/// Whether `app_scale`, held by a volume this run takes over, is a zoom a person set. Not when
/// it lies outside [`USER_ZOOM_RANGE`], or when it is exactly `--scale` (other than 1): builds
/// before ADR-0092 handed `--scale` to QAROS as the app scale, and a spread never lands on it
/// to the bit.
#[allow(
    clippy::float_cmp,
    reason = "an app scale set from the flag is the flag's value to the bit"
)]
pub(crate) fn is_user_zoom(app_scale: f32, flag_data_scale: f32) -> bool {
    let set_from_the_flag = flag_data_scale != 1.0 && app_scale == flag_data_scale;
    USER_ZOOM_RANGE.contains(&app_scale) && !set_from_the_flag
}

/// The parts of a taken-over volume's transform to set so it stands at the startup transform:
/// each one that differs. A volume created by this start already holds the startup transform,
/// so nothing differs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StartupCorrection {
    pub(crate) volume_pose: Option<Pose>,
    pub(crate) app_pose: Option<Pose>,
    pub(crate) app_scale: Option<f32>,
}

impl StartupCorrection {
    #[allow(
        clippy::float_cmp,
        reason = "any change of the scale QAROS holds is to be undone"
    )]
    pub(crate) fn between(live: VolumeTransform, startup: VolumeTransform) -> Self {
        Self {
            volume_pose: (live.volume_pose != startup.volume_pose).then_some(startup.volume_pose),
            app_pose: (live.app_pose != startup.app_pose).then_some(startup.app_pose),
            app_scale: (live.app_scale != startup.app_scale).then_some(startup.app_scale),
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self == Self::default()
    }
}

pub(crate) fn describe(transform: VolumeTransform) -> String {
    format!(
        "pose {}, app pose {}, app scale {}",
        describe_pose(transform.volume_pose),
        describe_pose(transform.app_pose),
        transform.app_scale
    )
}

fn describe_pose(pose: Pose) -> String {
    let position = pose.position;
    let orientation = pose.orientation;
    format!(
        "[{:.3}, {:.3}, {:.3}] xyzw [{:.3}, {:.3}, {:.3}, {:.3}]",
        position.x,
        position.y,
        position.z,
        orientation.x,
        orientation.y,
        orientation.z,
        orientation.w
    )
}

#[cfg(test)]
mod tests {
    use glam::{Quat, Vec3};

    use super::*;

    fn requested() -> VolumeTransform {
        VolumeTransform {
            volume_pose: Pose::from_translation(Vec3::new(0.0, 1.0, -1.5)),
            app_pose: Pose::IDENTITY,
            app_scale: 1.0,
        }
    }

    fn dragged() -> VolumeTransform {
        VolumeTransform {
            volume_pose: Pose::new(Quat::from_rotation_y(0.5), Vec3::new(0.4, 0.9, -2.0)),
            app_pose: Pose::from_translation(Vec3::new(0.0, -0.2, 0.0)),
            app_scale: 0.001,
        }
    }

    #[test]
    fn starts_from_the_live_transform_and_falls_back_to_the_requested_one() {
        let live = FollowedVolume::new("rerun", requested(), Ok(dragged()));
        assert_eq!(live.current(), dragged());
        let unreadable = FollowedVolume::new("rerun", requested(), Err("no".to_owned()));
        assert_eq!(unreadable.current(), requested());
        let zero_scale = VolumeTransform {
            app_scale: 0.0,
            ..dragged()
        };
        let unusable = FollowedVolume::new("rerun", requested(), Ok(zero_scale));
        assert_eq!(unusable.current(), requested());
    }

    #[test]
    fn follows_valid_changes_and_keeps_the_last_valid_transform() {
        let mut followed = FollowedVolume::new("rerun", requested(), Ok(requested()));
        followed.follow(dragged());
        assert_eq!(followed.current(), dragged());
        followed.follow(VolumeTransform {
            app_scale: f32::INFINITY,
            ..requested()
        });
        assert_eq!(followed.current(), dragged());
    }

    #[test]
    fn a_taken_over_volume_is_corrected_only_where_it_differs_from_the_startup_transform() {
        let unchanged = StartupCorrection::between(requested(), requested());
        assert!(unchanged.is_empty());

        let spread_only = VolumeTransform {
            app_scale: 0.001,
            ..requested()
        };
        assert_eq!(
            StartupCorrection::between(spread_only, requested()),
            StartupCorrection {
                volume_pose: None,
                app_pose: None,
                app_scale: Some(1.0),
            }
        );

        assert_eq!(
            StartupCorrection::between(dragged(), requested()),
            StartupCorrection {
                volume_pose: Some(requested().volume_pose),
                app_pose: Some(Pose::IDENTITY),
                app_scale: Some(1.0),
            }
        );
    }

    #[test]
    fn only_an_app_scale_a_person_can_have_set_counts_as_the_user_zoom() {
        for zoom in [0.1, 0.5, 1.0, 2.0, 10.0] {
            assert!(is_user_zoom(zoom, 1.0), "{zoom}");
        }
        for not_a_zoom in [0.001, 1.8e-6, 0.09, 10.5] {
            assert!(!is_user_zoom(not_a_zoom, 1.0), "{not_a_zoom}");
        }
        assert!(
            !is_user_zoom(0.18, 0.18),
            "--scale set as the app scale by an older build"
        );
        assert!(is_user_zoom(0.18, 0.024));
        assert!(is_user_zoom(1.0, 1.0), "the scale this viewer asks for");
    }
}
