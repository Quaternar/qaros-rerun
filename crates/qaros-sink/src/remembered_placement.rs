//! Where each app volume was last placed, remembered across runs (ADR-0085).
//!
//! A stopped viewer hides its app volume, and the next run takes it over with its place
//! (ADR-0089). A volume closed in the Hub, which the viewer itself never does, is created afresh
//! by QAROS from the request at the next start, so without this a volume a person moved in the
//! glasses would be back at the requested place. The viewer keeps, per common name, the volume
//! pose, app pose and user zoom it last followed, stored at Stop after the hide (ADR-0093), and
//! asks for the volume there at the next start while the startup placement is still the one the
//! volume was moved from. A startup placement that differs (other `--origin` or `--yaw` flags)
//! wins, and `--reset-placement` forgets the remembered place.
//!
//! The app scale is the user's zoom (ADR-0092): the data scale lives in the viewer's data fit
//! and never reaches QAROS. It is remembered with the poses while it lies in
//! `USER_ZOOM_RANGE`, and a volume created afresh starts with it, as a volume taken over keeps
//! it. Nothing is remembered per recording: each run fits every recording again (ADR-0092).
//!
//! The file is JSON, one entry per common name:
//!
//! ```json
//! {
//!     "volumes": {
//!         "rerun-air_traffic": {
//!             "requested": { "pose": <pose>, "app_pose": <pose> },
//!             "last_known": { "pose": <pose>, "app_pose": <pose> },
//!             "user_zoom": 1.5
//!         }
//!     }
//! }
//! ```
//!
//! where a pose is `{ "position": [x, y, z], "orientation_xyzw": [x, y, z, w] }` in the same
//! spaces as `QarAppVolumeInit`: the volume pose in the room, the app pose in unscaled volume
//! meters. `user_zoom` is absent for a zoom outside the range and in entries written before
//! ADR-0092; the volume then starts at the requested app scale, 1.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use xr_math::{Pose, VolumeTransform};

use crate::state_files::replace_file_contents;
use crate::volume_transform::{USER_ZOOM_RANGE, describe};

/// How often a moving volume's place is written at most. The last place is also written at
/// Stop, so a crash loses at most this much of a move.
pub(crate) const PLACEMENT_SAVE_PERIOD: Duration = Duration::from_secs(1);

/// How far a startup placement may be from the stored request and still count as the same
/// flags; the request is computed from the same flags each run, so only float noise remains.
const SAME_REQUEST_TOLERANCE: f32 = 1e-4;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct StoredPose {
    position: [f32; 3],
    orientation_xyzw: [f32; 4],
}

impl StoredPose {
    fn from_pose(pose: Pose) -> Self {
        Self {
            position: pose.position.to_array(),
            orientation_xyzw: pose.orientation.to_array(),
        }
    }

    /// As stored, not normalized: a file can hold any numbers, and the caller checks them.
    fn to_pose(self) -> Pose {
        Pose {
            orientation: Quat::from_array(self.orientation_xyzw),
            position: Vec3::from_array(self.position),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct StoredPlacement {
    pose: StoredPose,
    app_pose: StoredPose,
}

impl StoredPlacement {
    fn of_transform(transform: VolumeTransform) -> Self {
        Self {
            pose: StoredPose::from_pose(transform.volume_pose),
            app_pose: StoredPose::from_pose(transform.app_pose),
        }
    }

    /// `base` with this placement's poses; the app scale stays `base`'s.
    fn applied_to(self, base: VolumeTransform) -> VolumeTransform {
        VolumeTransform {
            volume_pose: self.pose.to_pose(),
            app_pose: self.app_pose.to_pose(),
            ..base
        }
    }

    fn is_close_to(self, other: Self) -> bool {
        poses_are_close(self.pose.to_pose(), other.pose.to_pose())
            && poses_are_close(self.app_pose.to_pose(), other.app_pose.to_pose())
    }
}

fn poses_are_close(first: Pose, second: Pose) -> bool {
    let same_turn = first
        .orientation
        .abs_diff_eq(second.orientation, SAME_REQUEST_TOLERANCE)
        || first
            .orientation
            .abs_diff_eq(-second.orientation, SAME_REQUEST_TOLERANCE);
    same_turn
        && first
            .position
            .abs_diff_eq(second.position, SAME_REQUEST_TOLERANCE)
}

/// One volume's entry: the startup placement the volume was moved from, where it was last, and
/// the user's zoom there when it lies in [`USER_ZOOM_RANGE`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct RememberedVolume {
    requested: StoredPlacement,
    last_known: StoredPlacement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_zoom: Option<f32>,
}

impl RememberedVolume {
    fn zoom_to_restore(self) -> Option<f32> {
        self.user_zoom.filter(|zoom| USER_ZOOM_RANGE.contains(zoom))
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PlacementFile {
    volumes: BTreeMap<String, RememberedVolume>,
}

/// The remembered placement of one app volume, kept in step with the followed transform.
#[derive(Debug)]
pub(crate) struct PlacementMemory {
    file: PathBuf,
    common_name: String,
    requested: StoredPlacement,
    /// What the file holds for this volume as far as this run knows; `None` until written or
    /// when nothing usable was read.
    stored: Option<RememberedVolume>,
    next_save: Instant,
    save_failing: bool,
}

impl PlacementMemory {
    /// The memory for `common_name` in `file`, and the transform to ask QAROS for: the
    /// remembered poses over `requested` when they were moved from this same request and
    /// `reset` is false, else `requested`. A file that cannot be read or parsed is logged and
    /// ignored, and rewritten at the next save.
    pub(crate) fn open(
        file: PathBuf,
        common_name: &str,
        requested: VolumeTransform,
        reset: bool,
    ) -> (Self, VolumeTransform) {
        let requested_placement = StoredPlacement::of_transform(requested);
        let remembered = match read_placement_file(&file) {
            Ok(contents) => contents.volumes.get(common_name).copied(),
            Err(error) => {
                log::warn!(
                    "qaros: ignoring the remembered app volume placements: {error}; app volume \
                     {common_name:?} starts at the requested place and the file is rewritten \
                     once it is followed"
                );
                None
            }
        };
        let mut memory = Self {
            file,
            common_name: common_name.to_owned(),
            requested: requested_placement,
            stored: remembered,
            next_save: Instant::now(),
            save_failing: false,
        };
        let Some(remembered) = remembered else {
            return (memory, requested);
        };
        if reset {
            log::info!(
                "qaros: --reset-placement: app volume {common_name:?} starts at the requested \
                 place; its remembered place is forgotten"
            );
            memory.remember_now(requested);
            return (memory, requested);
        }
        let startup = memory.startup_transform(remembered, requested);
        (memory, startup)
    }

    fn startup_transform(
        &self,
        remembered: RememberedVolume,
        requested: VolumeTransform,
    ) -> VolumeTransform {
        let common_name = &self.common_name;
        if !remembered.requested.is_close_to(self.requested) {
            log::info!(
                "qaros: app volume {common_name:?} was moved from another startup placement; \
                 starting at the one requested now (--origin, --yaw)"
            );
            return requested;
        }
        let restored_poses = remembered.last_known.applied_to(requested);
        let restored = VolumeTransform {
            app_scale: remembered.zoom_to_restore().unwrap_or(requested.app_scale),
            ..restored_poses
        };
        let orientations_are_unit = restored.volume_pose.orientation.is_normalized()
            && restored.app_pose.orientation.is_normalized();
        if !restored.is_valid() || !orientations_are_unit {
            log::warn!(
                "qaros: the remembered place of app volume {common_name:?} in {} is unusable; \
                 starting at the requested place",
                self.file.display()
            );
            return requested;
        }
        log::info!(
            "qaros: app volume {common_name:?} starts where it was last placed and zoomed, from \
             {}: {}; --reset-placement starts at the requested place and zoom",
            self.file.display(),
            describe(restored)
        );
        restored
    }

    /// Writes `current` when it differs from what the file holds and the last write lies
    /// [`PLACEMENT_SAVE_PERIOD`] back; for every transform the viewer follows.
    pub(crate) fn remember_soon(&mut self, current: VolumeTransform, now: Instant) {
        if now < self.next_save
            || self
                .entry_for(current)
                .is_some_and(|entry| self.holds(entry))
        {
            return;
        }
        self.next_save = now + PLACEMENT_SAVE_PERIOD;
        self.remember_now(current);
    }

    /// Writes `current` unless the file already holds it; at Stop, before the volume is hidden
    /// or closed.
    pub(crate) fn remember_now(&mut self, current: VolumeTransform) {
        let Some(entry) = self.entry_for(current) else {
            return;
        };
        if self.holds(entry) {
            return;
        }
        match self.write_entry(entry) {
            Ok(()) => {
                if self.save_failing {
                    log::info!(
                        "qaros: the place of app volume {:?} is stored in {} again",
                        self.common_name,
                        self.file.display()
                    );
                }
                self.save_failing = false;
                self.stored = Some(entry);
            }
            Err(error) => {
                if !self.save_failing {
                    log::warn!(
                        "qaros: cannot store the place of app volume {:?}: {error}; the next \
                         run starts at the requested place",
                        self.common_name
                    );
                }
                self.save_failing = true;
            }
        }
    }

    /// `None` for a transform the followed volume never holds (not finite), which is not
    /// worth remembering.
    fn entry_for(&self, current: VolumeTransform) -> Option<RememberedVolume> {
        current.is_valid().then(|| RememberedVolume {
            requested: self.requested,
            last_known: StoredPlacement::of_transform(current),
            user_zoom: USER_ZOOM_RANGE
                .contains(&current.app_scale)
                .then_some(current.app_scale),
        })
    }

    fn holds(&self, entry: RememberedVolume) -> bool {
        self.stored == Some(entry)
    }

    /// Read again before each write, so the entries of viewers with other volume names that
    /// share the file are kept.
    fn write_entry(&self, entry: RememberedVolume) -> Result<(), String> {
        let mut contents = read_placement_file(&self.file).unwrap_or_else(|error| {
            log::warn!("qaros: replacing the remembered app volume placements: {error}");
            PlacementFile::default()
        });
        contents.volumes.insert(self.common_name.clone(), entry);
        let text = placement_file_text(&contents)?;
        replace_file_contents(&self.file, &text)
            .map_err(|error| format!("{}: {error}", self.file.display()))
    }
}

/// A missing file holds no placements.
fn read_placement_file(file: &Path) -> Result<PlacementFile, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(PlacementFile::default()),
        Err(error) => return Err(format!("{}: {error}", file.display())),
    };
    serde_json::from_str(&text).map_err(|error| format!("{}: {error}", file.display()))
}

fn placement_file_text(contents: &PlacementFile) -> Result<String, String> {
    let mut bytes = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut bytes, formatter);
    contents
        .serialize(&mut serializer)
        .map_err(|error| error.to_string())?;
    let mut text = String::from_utf8(bytes).map_err(|error| error.to_string())?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDirectory(PathBuf);

    impl ScratchDirectory {
        fn new(name: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "qaros-sink-placement-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).unwrap();
            Self(directory)
        }

        fn file(&self) -> PathBuf {
            self.0.join("qaros-rerun.volume-placements.json")
        }
    }

    impl Drop for ScratchDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn requested() -> VolumeTransform {
        VolumeTransform {
            volume_pose: Pose::from_translation(Vec3::new(0.0, 0.5, -1.7)),
            app_pose: Pose::new(Quat::from_rotation_y(0.3), Vec3::ZERO),
            app_scale: 1.0,
        }
    }

    /// Where a person grabbed the volume to, with a spread that zoomed in by half.
    fn moved() -> VolumeTransform {
        VolumeTransform {
            volume_pose: Pose::new(Quat::from_rotation_y(0.8), Vec3::new(0.6, 0.9, -2.2)),
            app_pose: Pose::new(Quat::from_rotation_y(-0.4), Vec3::new(0.1, -0.2, 0.05)),
            app_scale: 1.5,
        }
    }

    /// Moved as [`moved`], with the app scale the Hub's clamp leaves, which is no zoom.
    fn moved_to_the_hub_clamp() -> VolumeTransform {
        VolumeTransform {
            app_scale: 0.001,
            ..moved()
        }
    }

    fn assert_poses_equal(actual: Pose, expected: Pose) {
        assert!(
            actual.position.abs_diff_eq(expected.position, 1e-6)
                && actual.orientation.abs_diff_eq(expected.orientation, 1e-6),
            "{actual:?} against {expected:?}"
        );
    }

    fn remember_moved_volume(file: &Path, common_name: &str) {
        let (mut memory, startup) =
            PlacementMemory::open(file.to_owned(), common_name, requested(), false);
        assert_eq!(startup, requested(), "nothing remembered yet");
        memory.remember_now(moved());
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "the zoom is copied through the file")]
    fn the_next_run_starts_at_the_moved_pose_app_pose_and_user_zoom() {
        let scratch = ScratchDirectory::new("restored");
        remember_moved_volume(&scratch.file(), "rerun-air_traffic");

        let (_, startup) =
            PlacementMemory::open(scratch.file(), "rerun-air_traffic", requested(), false);
        assert_poses_equal(startup.volume_pose, moved().volume_pose);
        assert_poses_equal(startup.app_pose, moved().app_pose);
        assert_eq!(startup.app_scale, 1.5, "the user's zoom is restored");

        let (_, other_volume) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        assert_eq!(other_volume, requested(), "remembered per common name");
    }

    #[test]
    #[allow(clippy::float_cmp, reason = "the app scale is copied from the request")]
    fn an_app_scale_outside_the_user_zoom_range_is_not_restored() {
        let scratch = ScratchDirectory::new("clamped");
        let (mut memory, _) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        memory.remember_now(moved_to_the_hub_clamp());

        let (_, startup) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        assert_poses_equal(startup.volume_pose, moved().volume_pose);
        assert_eq!(startup.app_scale, 1.0, "the requested zoom");
    }

    #[test]
    fn the_file_holds_the_request_the_last_known_poses_and_the_user_zoom() {
        let scratch = ScratchDirectory::new("format");
        remember_moved_volume(&scratch.file(), "rerun");
        let text = std::fs::read_to_string(scratch.file()).unwrap();
        assert!(text.ends_with("}\n"), "{text}");
        assert!(
            text.contains("\n    \"volumes\": {\n        \"rerun\": {"),
            "{text}"
        );
        assert!(text.contains("\"user_zoom\": 1.5"), "{text}");

        let contents: PlacementFile = serde_json::from_str(&text).unwrap();
        let entry = contents.volumes["rerun"];
        assert_eq!(entry.requested, StoredPlacement::of_transform(requested()));
        assert_poses_equal(entry.last_known.pose.to_pose(), moved().volume_pose);
        assert_poses_equal(entry.last_known.app_pose.to_pose(), moved().app_pose);
        assert_eq!(entry.user_zoom, Some(1.5));

        let mut memory = PlacementMemory::open(scratch.file(), "rerun", requested(), false).0;
        memory.remember_now(moved_to_the_hub_clamp());
        let clamped_text = std::fs::read_to_string(scratch.file()).unwrap();
        assert!(!clamped_text.contains("zoom"), "{clamped_text}");
    }

    /// Entries written before ADR-0092 hold no zoom; their poses still count.
    #[test]
    #[allow(clippy::float_cmp, reason = "the app scale is copied from the request")]
    fn an_entry_without_a_zoom_restores_the_poses_at_the_requested_zoom() {
        let scratch = ScratchDirectory::new("before-zoom");
        remember_moved_volume(&scratch.file(), "rerun");
        let text = std::fs::read_to_string(scratch.file()).unwrap();
        let without_zoom = text.replace(",\n            \"user_zoom\": 1.5", "");
        assert!(!without_zoom.contains("zoom"), "{without_zoom}");
        std::fs::write(scratch.file(), without_zoom).unwrap();

        let (_, startup) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        assert_poses_equal(startup.volume_pose, moved().volume_pose);
        assert_eq!(startup.app_scale, 1.0);
    }

    #[test]
    fn other_startup_flags_win_over_the_remembered_place() {
        let scratch = ScratchDirectory::new("other-flags");
        remember_moved_volume(&scratch.file(), "rerun");
        let other_origin = VolumeTransform {
            volume_pose: Pose::from_translation(Vec3::new(0.0, 1.5, -1.7)),
            ..requested()
        };

        let (mut memory, startup) =
            PlacementMemory::open(scratch.file(), "rerun", other_origin, false);
        assert_eq!(startup, other_origin);

        memory.remember_now(startup);
        let (_, next_run) = PlacementMemory::open(scratch.file(), "rerun", other_origin, false);
        assert_eq!(next_run, other_origin, "the new flags are the new baseline");
        let (_, old_flags) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        assert_eq!(old_flags, requested());
    }

    #[test]
    fn reset_placement_starts_at_the_request_and_forgets_the_remembered_place() {
        let scratch = ScratchDirectory::new("reset");
        remember_moved_volume(&scratch.file(), "rerun");

        let (_, startup) = PlacementMemory::open(scratch.file(), "rerun", requested(), true);
        assert_eq!(startup, requested());

        let (_, next_run) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        assert_eq!(next_run, requested(), "forgotten, not only skipped");
    }

    #[test]
    fn a_corrupt_file_is_reported_ignored_and_replaced_at_the_next_save() {
        let scratch = ScratchDirectory::new("corrupt");
        std::fs::write(
            scratch.file(),
            "{\"volumes\": {\"rerun\": {\"requested\": 3}}}\n",
        )
        .unwrap();

        let error = read_placement_file(&scratch.file()).unwrap_err();
        assert!(
            error.starts_with(&scratch.file().display().to_string()) && error.contains("line 1"),
            "{error}"
        );
        let (mut memory, startup) =
            PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        assert_eq!(startup, requested());

        memory.remember_now(moved());
        let (_, next_run) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        assert_poses_equal(next_run.volume_pose, moved().volume_pose);
    }

    #[test]
    fn a_remembered_place_that_is_not_a_pose_is_ignored() {
        let scratch = ScratchDirectory::new("unusable");
        remember_moved_volume(&scratch.file(), "rerun");
        let text = std::fs::read_to_string(scratch.file()).unwrap();
        let mut contents: PlacementFile = serde_json::from_str(&text).unwrap();
        let entry = contents.volumes.get_mut("rerun").unwrap();
        entry.last_known.pose.orientation_xyzw = [0.0, 0.0, 0.0, 0.0];
        std::fs::write(scratch.file(), placement_file_text(&contents).unwrap()).unwrap();

        let (_, startup) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        assert_eq!(startup, requested());
    }

    #[test]
    fn a_moving_volume_is_written_at_most_once_a_save_period() {
        let scratch = ScratchDirectory::new("throttled");
        let (mut memory, _) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        let start = Instant::now();
        memory.remember_soon(requested(), start);
        let first_step = VolumeTransform {
            volume_pose: Pose::from_translation(Vec3::new(0.2, 0.5, -1.7)),
            ..requested()
        };
        memory.remember_soon(first_step, start + Duration::from_millis(300));
        let stored_now = |file: &Path| {
            let (_, startup) = PlacementMemory::open(file.to_owned(), "rerun", requested(), false);
            startup.volume_pose.position
        };
        assert_eq!(
            stored_now(&scratch.file()),
            requested().volume_pose.position
        );

        memory.remember_soon(first_step, start + PLACEMENT_SAVE_PERIOD);
        assert_eq!(stored_now(&scratch.file()), first_step.volume_pose.position);

        memory.remember_now(moved());
        assert_eq!(stored_now(&scratch.file()), moved().volume_pose.position);
    }

    #[test]
    fn other_volume_names_in_the_file_are_kept() {
        let scratch = ScratchDirectory::new("shared");
        remember_moved_volume(&scratch.file(), "rerun-robot_arm");
        let (mut memory, _) = PlacementMemory::open(scratch.file(), "rerun", requested(), false);
        memory.remember_now(requested());

        let (_, robot_arm) =
            PlacementMemory::open(scratch.file(), "rerun-robot_arm", requested(), false);
        assert_poses_equal(robot_arm.volume_pose, moved().volume_pose);
    }
}
