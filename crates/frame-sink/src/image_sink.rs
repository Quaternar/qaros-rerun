use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::{
    DepthRange, EYE_COUNT, EyeImage, FrameId, FrameKey, FrameRequest, FrameSink, PoseSource,
    RenderPolicy, RosterChange, ShowInfo, SinkError, UserEvent, UserId, UserInfo, UserRoster,
    ViewRequest,
};

#[derive(Clone, Debug)]
pub struct ImageSinkOptions {
    pub output_directory: PathBuf,
    pub resolution_in_pixel: [u32; 2],
    pub depth_range: DepthRange,
    /// Write every n-th shown frame of each user; 1 writes all of them.
    pub write_every_nth_frame: u64,
    /// Each user leaves after this many shown frames.
    pub frames_per_user: u64,
    /// Also write the encoded depth as raw little-endian `f32`, left eye then right eye, each
    /// row by row, for exact comparisons the 8-bit visualization cannot give.
    pub write_raw_depth: bool,
}

/// A simulated headset wearer: appears at a sink tick (one tick per `begin_frames` call) and
/// leaves after `frames_per_user` shown frames.
pub struct SimulatedUser {
    pub label: String,
    pub joins_at_tick: u64,
    /// Poses by the user's own frame id.
    pub pose_source: Box<dyn PoseSource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    NotYetJoined,
    Present,
    Left,
}

struct ScheduledUser {
    id: UserId,
    simulated: SimulatedUser,
    presence: Presence,
    shown_frame_count: u64,
}

struct InFlightFrame {
    request: FrameRequest,
    images: [Option<EyeImage>; EYE_COUNT],
}

#[derive(Default)]
struct UserStream {
    next_frame_id: FrameId,
    in_flight: Option<InFlightFrame>,
}

/// A [`FrameSink`] for simulated users that writes each shown stereo pair as PNGs, per user
/// under `user-<n>/`: color side by side, a depth visualization side by side, and a text file
/// with the user, poses, fields of view and near/far the frame was shown with.
///
/// Users join and leave on a schedule; the [`RenderPolicy`] decides who is rendered, and a
/// waiting user gets its turn when a rendered one leaves.
pub struct ImageSink {
    options: ImageSinkOptions,
    users: Vec<ScheduledUser>,
    roster: UserRoster,
    streams: BTreeMap<UserId, UserStream>,
    tick: u64,
    written_files: Vec<PathBuf>,
}

impl ImageSink {
    pub fn new(
        options: ImageSinkOptions,
        users: Vec<SimulatedUser>,
        policy: Box<dyn RenderPolicy>,
    ) -> Result<Self, SinkError> {
        std::fs::create_dir_all(&options.output_directory)?;
        let users = (1_u32..)
            .zip(users)
            .map(|(number, simulated)| ScheduledUser {
                id: UserId(number),
                simulated,
                presence: Presence::NotYetJoined,
                shown_frame_count: 0,
            })
            .collect();
        Ok(Self {
            options,
            users,
            roster: UserRoster::new(policy),
            streams: BTreeMap::new(),
            tick: 0,
            written_files: Vec::new(),
        })
    }

    pub fn written_files(&self) -> &[PathBuf] {
        &self.written_files
    }

    pub fn shown_frame_count(&self, user: UserId) -> u64 {
        self.users
            .iter()
            .find(|scheduled| scheduled.id == user)
            .map_or(0, |scheduled| scheduled.shown_frame_count)
    }

    pub fn rendered_users(&self) -> &[UserInfo] {
        self.roster.rendered()
    }

    pub fn user_directory(&self, user: UserId) -> PathBuf {
        self.options.output_directory.join(user.to_string())
    }

    fn schedule_events(&mut self) -> Vec<UserEvent> {
        let mut events = Vec::new();
        for scheduled in &mut self.users {
            match scheduled.presence {
                Presence::NotYetJoined if scheduled.simulated.joins_at_tick <= self.tick => {
                    scheduled.presence = Presence::Present;
                    events.push(UserEvent::Joined(UserInfo {
                        id: scheduled.id,
                        label: scheduled.simulated.label.clone(),
                    }));
                }
                Presence::Present
                    if scheduled.shown_frame_count >= self.options.frames_per_user =>
                {
                    scheduled.presence = Presence::Left;
                    events.push(UserEvent::Left(scheduled.id));
                }
                _ => {}
            }
        }
        events
    }

    fn in_flight_frame(&mut self, key: FrameKey) -> Result<&mut InFlightFrame, SinkError> {
        self.streams
            .get_mut(&key.user)
            .and_then(|stream| stream.in_flight.as_mut())
            .filter(|frame| frame.request.key == key)
            .ok_or(SinkError::UnknownFrame(key))
    }

    fn should_write(&self, shown_frame_count: u64) -> bool {
        shown_frame_count.is_multiple_of(self.options.write_every_nth_frame.max(1))
    }

    fn write_frame(
        &mut self,
        request: &FrameRequest,
        images: &[EyeImage; EYE_COUNT],
        show: ShowInfo,
    ) -> Result<(), SinkError> {
        let directory = self.user_directory(request.key.user);
        std::fs::create_dir_all(&directory)?;
        let stem = directory.join(format!("frame_{:05}", request.key.frame_id));
        let color_path = path_with_suffix(&stem, "_color.png");
        let depth_path = path_with_suffix(&stem, "_depth.png");
        let metadata_path = path_with_suffix(&stem, ".txt");

        let [width, height] = request.resolution_in_pixel;
        let color = side_by_side(
            &images[0].color_rgba8,
            &images[1].color_rgba8,
            width,
            height,
            4,
        );
        image::save_buffer(
            &color_path,
            &color,
            width * 2,
            height,
            image::ColorType::Rgba8,
        )
        .map_err(|error| SinkError::Backend(error.to_string()))?;

        let depth_range = self.options.depth_range;
        let left_depth = depth_visualization(&images[0].depth, depth_range);
        let right_depth = depth_visualization(&images[1].depth, depth_range);
        let depth = side_by_side(&left_depth, &right_depth, width, height, 1);
        image::save_buffer(&depth_path, &depth, width * 2, height, image::ColorType::L8)
            .map_err(|error| SinkError::Backend(error.to_string()))?;

        let label = self
            .users
            .iter()
            .find(|scheduled| scheduled.id == request.key.user)
            .map_or("", |scheduled| scheduled.simulated.label.as_str());
        std::fs::write(
            &metadata_path,
            frame_metadata(request, label, show, depth_range),
        )?;

        self.written_files
            .extend([color_path, depth_path, metadata_path]);

        if self.options.write_raw_depth {
            let raw_depth_path = path_with_suffix(&stem, "_depth.f32");
            let raw_depth: Vec<u8> = images
                .iter()
                .flat_map(|image| &image.depth)
                .flat_map(|value| value.to_le_bytes())
                .collect();
            std::fs::write(&raw_depth_path, raw_depth)?;
            self.written_files.push(raw_depth_path);
        }
        Ok(())
    }
}

impl FrameSink for ImageSink {
    fn begin_frames(&mut self) -> Result<Vec<FrameRequest>, SinkError> {
        let events = self.schedule_events();
        self.tick += 1;
        for change in self.roster.apply(events) {
            match change {
                RosterChange::Admitted(user) => {
                    self.streams.insert(user.id, UserStream::default());
                }
                RosterChange::Released(user) => {
                    self.streams.remove(&user);
                }
            }
        }

        let mut requests = Vec::new();
        for (user, stream) in &mut self.streams {
            if stream.in_flight.is_some() {
                continue;
            }
            let Some(scheduled) = self
                .users
                .iter_mut()
                .find(|scheduled| scheduled.id == *user)
            else {
                continue;
            };
            let key = FrameKey {
                user: *user,
                frame_id: stream.next_frame_id,
            };
            stream.next_frame_id += 1;
            let request = FrameRequest {
                key,
                views: scheduled.simulated.pose_source.views(key.frame_id),
                resolution_in_pixel: self.options.resolution_in_pixel,
                volume: None,
            };
            stream.in_flight = Some(InFlightFrame {
                request,
                images: [None, None],
            });
            requests.push(request);
        }
        Ok(requests)
    }

    fn submit_view(
        &mut self,
        key: FrameKey,
        view_index: usize,
        image: EyeImage,
    ) -> Result<(), SinkError> {
        image.validate()?;
        let frame = self.in_flight_frame(key)?;
        if image.resolution_in_pixel != frame.request.resolution_in_pixel {
            return Err(SinkError::InvalidImage(format!(
                "eye is {:?}, the frame asked for {:?}",
                image.resolution_in_pixel, frame.request.resolution_in_pixel
            )));
        }
        let slot = frame
            .images
            .get_mut(view_index)
            .ok_or(SinkError::UnknownView(view_index))?;
        *slot = Some(image);
        Ok(())
    }

    fn show_frame(&mut self, key: FrameKey, show: ShowInfo) -> Result<(), SinkError> {
        self.in_flight_frame(key)?;
        let Some(InFlightFrame { request, images }) = self
            .streams
            .get_mut(&key.user)
            .and_then(|stream| stream.in_flight.take())
        else {
            return Err(SinkError::UnknownFrame(key));
        };
        let [Some(left), Some(right)] = images else {
            return Err(SinkError::InvalidImage(format!(
                "{key} shown before both eyes were submitted"
            )));
        };
        let Some(scheduled) = self
            .users
            .iter_mut()
            .find(|scheduled| scheduled.id == key.user)
        else {
            return Err(SinkError::UnknownFrame(key));
        };
        let shown_before = scheduled.shown_frame_count;
        scheduled.shown_frame_count += 1;
        if self.should_write(shown_before) {
            self.write_frame(&request, &[left, right], show)
        } else {
            Ok(())
        }
    }

    fn abandon_frame(&mut self, key: FrameKey) {
        if let Some(stream) = self.streams.get_mut(&key.user) {
            stream.in_flight.take_if(|frame| frame.request.key == key);
        }
    }

    /// Until every scheduled user has joined, been rendered and left.
    fn is_streaming(&self) -> bool {
        self.users
            .iter()
            .any(|scheduled| scheduled.presence != Presence::Left)
    }

    fn depth_range(&self) -> DepthRange {
        self.options.depth_range
    }
}

fn path_with_suffix(stem: &Path, suffix: &str) -> PathBuf {
    let mut path = stem.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

fn side_by_side<T: Copy>(
    left: &[T],
    right: &[T],
    width: u32,
    height: u32,
    channels: usize,
) -> Vec<T> {
    let row_length = width as usize * channels;
    let mut combined = Vec::with_capacity(row_length * 2 * height as usize);
    for (left_row, right_row) in left
        .chunks_exact(row_length)
        .zip(right.chunks_exact(row_length))
    {
        combined.extend_from_slice(left_row);
        combined.extend_from_slice(right_row);
    }
    combined
}

/// Grayscale view of encoded depth: near is bright, far fades on a log scale, and anything at
/// the far plane (Rerun's background) is black.
pub fn depth_visualization(encoded_depth: &[f32], depth_range: DepthRange) -> Vec<u8> {
    let near = depth_range.near();
    let far = depth_range.far();
    let log_span = (far / near).ln();
    encoded_depth
        .iter()
        .map(|&encoded| {
            let distance = depth_range.decode_distance(encoded);
            if !distance.is_finite() || distance >= far * 0.999 {
                return 0;
            }
            let fraction = ((distance.max(near) / near).ln() / log_span).clamp(0.0, 1.0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let brightness = (32.0 + 223.0 * (1.0 - fraction)).round() as u8;
            brightness
        })
        .collect()
}

fn frame_metadata(
    request: &FrameRequest,
    label: &str,
    show: ShowInfo,
    depth_range: DepthRange,
) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "user: {} ({label})", request.key.user);
    let _ = writeln!(text, "frame_id: {}", request.key.frame_id);
    let _ = writeln!(
        text,
        "resolution_per_eye: {:?}",
        request.resolution_in_pixel
    );
    let _ = writeln!(text, "near_plane: {}", show.near_far.near_plane);
    let _ = writeln!(text, "far_plane: {}", show.near_far.far_plane);
    let _ = writeln!(text, "depth_scale: {}", show.depth_scale);
    let _ = writeln!(text, "depth_encoding: {:?}", depth_range.encoding);
    for (eye_name, ViewRequest { pose, fov }) in ["left", "right"].iter().zip(request.views) {
        let _ = writeln!(
            text,
            "{eye_name}_position: [{:.4}, {:.4}, {:.4}]",
            pose.position.x, pose.position.y, pose.position.z
        );
        let _ = writeln!(
            text,
            "{eye_name}_orientation_xyzw: [{:.4}, {:.4}, {:.4}, {:.4}]",
            pose.orientation.x, pose.orientation.y, pose.orientation.z, pose.orientation.w
        );
        let _ = writeln!(
            text,
            "{eye_name}_fov_left_right_up_down: [{:.4}, {:.4}, {:.4}, {:.4}]",
            fov.angle_left, fov.angle_right, fov.angle_up, fov.angle_down
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use glam::Vec3;

    use super::*;
    use crate::{DepthEncoding, HeadMotion, MaxUsers, ScriptedHeadMotion};

    fn unique_directory(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("frame-sink-{name}-{}-{nanos}", std::process::id()))
    }

    fn depth_range() -> DepthRange {
        DepthRange::new(0.1, 10.0, DepthEncoding::ForwardNormalized).unwrap()
    }

    fn standing_user(label: &str, joins_at_tick: u64, x: f32) -> SimulatedUser {
        SimulatedUser {
            label: label.to_owned(),
            joins_at_tick,
            pose_source: Box::new(ScriptedHeadMotion::new(HeadMotion::Still {
                position: Vec3::new(x, 1.6, 0.0),
                yaw: 0.0,
            })),
        }
    }

    fn options(directory: &Path, frames_per_user: u64) -> ImageSinkOptions {
        ImageSinkOptions {
            output_directory: directory.to_owned(),
            resolution_in_pixel: [4, 2],
            depth_range: depth_range(),
            write_every_nth_frame: 1,
            frames_per_user,
            write_raw_depth: false,
        }
    }

    fn sink(directory: &Path) -> ImageSink {
        sink_with_options(options(directory, 2))
    }

    fn sink_with_options(options: ImageSinkOptions) -> ImageSink {
        ImageSink::new(
            options,
            vec![standing_user("only", 0, 0.0)],
            Box::new(MaxUsers { maximum: 2 }),
        )
        .unwrap()
    }

    fn eye_image(red: u8, depth_value: f32) -> EyeImage {
        EyeImage {
            resolution_in_pixel: [4, 2],
            color_rgba8: [red, 0, 0, 255].repeat(8),
            depth: vec![depth_value; 8],
        }
    }

    fn show_both_eyes(sink: &mut ImageSink, key: FrameKey) {
        sink.submit_view(key, 0, eye_image(1, 0.5)).unwrap();
        sink.submit_view(key, 1, eye_image(1, 0.5)).unwrap();
        sink.show_frame(key, ShowInfo::from_depth_range(depth_range()))
            .unwrap();
    }

    #[test]
    fn writes_side_by_side_pngs_with_metadata_per_user() {
        let directory = unique_directory("write");
        let mut sink = sink(&directory);
        let [request] = sink.begin_frames().unwrap()[..] else {
            panic!("one user, one frame");
        };
        assert!(
            sink.begin_frames().unwrap().is_empty(),
            "one frame in flight"
        );

        let one_meter = depth_range().encode_distance(1.0);
        sink.submit_view(request.key, 0, eye_image(200, one_meter))
            .unwrap();
        sink.submit_view(request.key, 1, eye_image(50, 1.0))
            .unwrap();
        sink.show_frame(request.key, ShowInfo::from_depth_range(depth_range()))
            .unwrap();

        let user_directory = directory.join("user-1");
        let color = image::open(user_directory.join("frame_00000_color.png"))
            .unwrap()
            .to_rgba8();
        assert_eq!(color.dimensions(), (8, 2));
        assert_eq!(color.get_pixel(0, 0).0, [200, 0, 0, 255]);
        assert_eq!(color.get_pixel(7, 1).0, [50, 0, 0, 255]);

        let depth = image::open(user_directory.join("frame_00000_depth.png"))
            .unwrap()
            .to_luma8();
        assert_eq!(depth.get_pixel(0, 0).0, [144]);
        assert_eq!(depth.get_pixel(7, 0).0, [0], "far plane is background");

        let metadata = std::fs::read_to_string(user_directory.join("frame_00000.txt")).unwrap();
        assert!(metadata.contains("user: user-1 (only)"));
        assert!(metadata.contains("near_plane: 0.1"));
        assert!(metadata.contains("far_plane: 10"));
        assert!(metadata.contains("depth_encoding: ForwardNormalized"));
        assert_eq!(sink.written_files().len(), 3);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn writes_raw_depth_left_eye_first_when_asked() {
        let directory = unique_directory("raw-depth");
        let mut sink = sink_with_options(ImageSinkOptions {
            write_raw_depth: true,
            ..options(&directory, 2)
        });
        let key = sink.begin_frames().unwrap()[0].key;
        sink.submit_view(key, 0, eye_image(1, 0.25)).unwrap();
        sink.submit_view(key, 1, eye_image(1, 0.75)).unwrap();
        sink.show_frame(key, ShowInfo::from_depth_range(depth_range()))
            .unwrap();

        let bytes = std::fs::read(directory.join("user-1").join("frame_00000_depth.f32")).unwrap();
        let values: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        let mut expected = vec![0.25; 8];
        expected.extend([0.75; 8]);
        assert_eq!(values, expected);
        assert_eq!(sink.written_files().len(), 4);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_user_leaves_after_its_frames_and_streaming_ends_with_the_last_user() {
        let directory = unique_directory("limit");
        let mut sink = sink(&directory);
        for _ in 0..2 {
            let requests = sink.begin_frames().unwrap();
            show_both_eyes(&mut sink, requests[0].key);
        }
        assert!(
            sink.is_streaming(),
            "the leave is applied at the next begin"
        );
        assert!(sink.begin_frames().unwrap().is_empty());
        assert!(!sink.is_streaming());
        assert!(sink.rendered_users().is_empty());
        assert_eq!(sink.shown_frame_count(UserId(1)), 2);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn users_join_at_their_tick_with_their_own_poses_and_frame_ids() {
        let directory = unique_directory("two-users");
        let mut sink = ImageSink::new(
            options(&directory, 3),
            vec![
                standing_user("first", 0, 0.0),
                standing_user("second", 2, 1.0),
            ],
            Box::new(MaxUsers { maximum: 2 }),
        )
        .unwrap();

        let mut shown: Vec<(FrameKey, f32)> = Vec::new();
        while sink.is_streaming() {
            for request in sink.begin_frames().unwrap() {
                show_both_eyes(&mut sink, request.key);
                shown.push((request.key, request.views[0].pose.position.x));
            }
        }
        let frames_of = |user: u32| -> Vec<(FrameId, f32)> {
            shown
                .iter()
                .filter(|(key, _)| key.user == UserId(user))
                .map(|(key, x)| (key.frame_id, *x))
                .collect()
        };
        let left_eye = -0.063 / 2.0;
        assert_eq!(frames_of(1), [(0, left_eye), (1, left_eye), (2, left_eye)]);
        assert_eq!(
            frames_of(2),
            [
                (0, 1.0 + left_eye),
                (1, 1.0 + left_eye),
                (2, 1.0 + left_eye)
            ]
        );
        let first_frame_of_second = shown
            .iter()
            .position(|(key, _)| key.user == UserId(2))
            .unwrap();
        assert_eq!(
            first_frame_of_second, 3,
            "the second user joins at tick 2, after the first user's frames 0, 1 and 2"
        );
        assert!(
            directory
                .join("user-2")
                .join("frame_00002_color.png")
                .exists()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_user_over_the_maximum_waits_for_a_free_slot() {
        let directory = unique_directory("waiting");
        let mut sink = ImageSink::new(
            options(&directory, 2),
            vec![
                standing_user("first", 0, 0.0),
                standing_user("second", 0, 1.0),
            ],
            Box::new(MaxUsers { maximum: 1 }),
        )
        .unwrap();
        let mut order = Vec::new();
        while sink.is_streaming() {
            for request in sink.begin_frames().unwrap() {
                show_both_eyes(&mut sink, request.key);
                order.push(request.key);
            }
        }
        let users: Vec<u32> = order.iter().map(|key| key.user.0).collect();
        assert_eq!(users, [1, 1, 2, 2]);
        assert_eq!(order[2].frame_id, 0, "frame ids start at 0 per user");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_mismatched_and_incomplete_frames() {
        let directory = unique_directory("reject");
        let mut sink = sink(&directory);
        let key = sink.begin_frames().unwrap()[0].key;

        let wrong_size = EyeImage {
            resolution_in_pixel: [2, 2],
            color_rgba8: vec![0; 16],
            depth: vec![0.0; 4],
        };
        assert!(matches!(
            sink.submit_view(key, 0, wrong_size),
            Err(SinkError::InvalidImage(_))
        ));
        let next_frame = FrameKey {
            frame_id: key.frame_id + 1,
            ..key
        };
        assert!(matches!(
            sink.submit_view(next_frame, 0, eye_image(0, 0.0)),
            Err(SinkError::UnknownFrame(_))
        ));
        let other_user = FrameKey {
            user: UserId(7),
            ..key
        };
        assert!(matches!(
            sink.submit_view(other_user, 0, eye_image(0, 0.0)),
            Err(SinkError::UnknownFrame(_))
        ));
        assert!(matches!(
            sink.submit_view(key, 2, eye_image(0, 0.0)),
            Err(SinkError::UnknownView(2))
        ));

        sink.submit_view(key, 0, eye_image(0, 0.0)).unwrap();
        assert!(matches!(
            sink.show_frame(key, ShowInfo::from_depth_range(depth_range())),
            Err(SinkError::InvalidImage(_))
        ));
        assert_eq!(
            sink.begin_frames().unwrap().len(),
            1,
            "failed show releases the frame"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn abandoned_frame_frees_the_slot() {
        let directory = unique_directory("abandon");
        let mut sink = sink(&directory);
        let key = sink.begin_frames().unwrap()[0].key;
        sink.abandon_frame(key);
        let next = sink.begin_frames().unwrap()[0].key;
        assert_eq!(next.frame_id, key.frame_id + 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn depth_visualization_is_bright_near_and_dark_far() {
        let range = depth_range();
        let encoded = [0.1, 1.0, 9.0, 10.0].map(|distance| range.encode_distance(distance));
        let shades = depth_visualization(&encoded, range);
        assert_eq!(shades[0], 255);
        assert!(shades[1] > shades[2]);
        assert!(shades[2] >= 32);
        assert_eq!(shades[3], 0);
    }
}
