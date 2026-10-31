//! The app volume's transform as QAROS holds it, which may differ from the one the viewer
//! asked for: `get_or_create` hands back an existing volume as it is, and a person can move,
//! turn and scale the volume in the Hub while the viewer runs. The sinks map every frame's
//! poses, and the renderer places Rerun's world, with the transform followed here (ADR-0066).

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use qaros::{AppVolumeId, AppVolumeInfo, Session};
use xr_math::{Pose, VolumeTransform};

use crate::connection::from_qaros_pose;

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
/// carry an older transform than the latest values.
#[derive(Debug)]
pub(crate) struct LiveVolume {
    followed: FollowedVolume,
    updates: Receiver<AppVolumeInfo>,
    next_read: Instant,
    read_failing: bool,
}

impl LiveVolume {
    pub(crate) fn new(followed: FollowedVolume, updates: Receiver<AppVolumeInfo>) -> Self {
        Self {
            followed,
            updates,
            next_read: Instant::now() + LATEST_VALUES_POLL_PERIOD,
            read_failing: false,
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

fn describe(transform: VolumeTransform) -> String {
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
}
