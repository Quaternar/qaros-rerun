//! The timeline panel's slider, applied to the viewer: a seek pauses playback and moves
//! the time cursor to that fraction of the active timeline's data, Play resumes. The desktop
//! time panel shows the same cursor, since both drive Rerun's one time control. The other way,
//! the cursor's fraction goes to the panel server each frame for the slider's thumb to follow.

use panel_server::{TimelinePosition, TimelineRequest, TimelineRequests};
use re_viewer_context::external::re_log_types::{AbsoluteTimeRange, StoreId, TimeInt, TimeReal};
use re_viewer_context::{
    CommandSender, SystemCommand, SystemCommandSender as _, TimeControlCommand,
};
use rerun::external::re_sdk_types::blueprint::components::PlayState;
use rerun::external::re_viewer;

/// The recording the viewer shows, the data range of its active timeline and where its time
/// cursor stands.
pub struct ActiveTimeline {
    pub store_id: StoreId,
    pub range: AbsoluteTimeRange,
    pub cursor: TimeInt,
}

/// What a viewer tells the slider about its timeline; the Rerun app, or a stand-in in tests.
pub trait TimelineSource {
    fn active_timeline(&self) -> Option<ActiveTimeline>;
}

impl TimelineSource for re_viewer::App {
    fn active_timeline(&self) -> Option<ActiveTimeline> {
        let store_id = self.active_recording_id()?.clone();
        let query = self.current_query()?;
        let range = self.recording_db()?.time_range_for(&query.timeline()?)?;
        Some(ActiveTimeline {
            store_id,
            range,
            cursor: query.at(),
        })
    }
}

/// Takes the pages' requests once per viewer frame and sends them to the viewer as time
/// control commands, through the command channel the Rerun app was built with, and publishes
/// where the time cursor stands for the pages.
pub struct TimelineScrub {
    requests: TimelineRequests,
    position: TimelinePosition,
    command_sender: CommandSender,
}

impl TimelineScrub {
    pub fn new(
        requests: TimelineRequests,
        position: TimelinePosition,
        command_sender: CommandSender,
    ) -> Self {
        Self {
            requests,
            position,
            command_sender,
        }
    }

    /// Publishes the time cursor as a fraction of the active timeline's data, the range the
    /// seeks use; nothing while the viewer has no timeline.
    pub fn publish_position(&self, viewer: &impl TimelineSource) {
        if let Some(timeline) = viewer.active_timeline() {
            self.position
                .publish(fraction_at(timeline.cursor, timeline.range));
        }
    }

    /// True when commands went out, so the caller repaints for the viewer to apply them.
    /// Requests that arrive before the viewer has a recording with a timeline are dropped: a
    /// seek kept for later would jump the cursor long after the user let go.
    pub fn apply(&self, viewer: &impl TimelineSource) -> bool {
        let requests = self.requests.take();
        if requests.is_empty() {
            return false;
        }
        let Some(timeline) = viewer.active_timeline() else {
            re_log::debug!(
                "panels: no timeline yet for {} slider request(s)",
                requests.len()
            );
            return false;
        };
        let time_commands = requests
            .iter()
            .flat_map(|request| time_commands(*request, timeline.range))
            .collect();
        self.command_sender
            .send_system(SystemCommand::TimeControlCommands {
                store_id: timeline.store_id,
                time_commands,
            });
        true
    }
}

fn time_commands(request: TimelineRequest, range: AbsoluteTimeRange) -> Vec<TimeControlCommand> {
    match request {
        TimelineRequest::SeekAndPause { fraction } => vec![
            TimeControlCommand::SetPlayState(PlayState::Paused),
            TimeControlCommand::SetTimeClamped(time_at(fraction, range)),
        ],
        TimelineRequest::Play => vec![TimeControlCommand::SetPlayState(PlayState::Playing)],
    }
}

/// A cursor outside the data (before its first time, say) is at the nearer end; a timeline with
/// a single time is all at its start.
#[allow(clippy::cast_precision_loss)]
fn fraction_at(cursor: TimeInt, range: AbsoluteTimeRange) -> f64 {
    let first = range.min().as_i64();
    let length = range.max().as_i64().saturating_sub(first);
    if length <= 0 {
        return 0.0;
    }
    (cursor.as_i64().saturating_sub(first) as f64 / length as f64).clamp(0.0, 1.0)
}

fn time_at(fraction: f64, range: AbsoluteTimeRange) -> TimeReal {
    let first = range.min().as_f64();
    let last = range.max().as_f64();
    TimeReal::from(first + fraction.clamp(0.0, 1.0) * (last - first))
}

#[cfg(test)]
mod tests {
    use re_viewer_context::external::re_log_types::{StoreKind, TimeInt};
    use re_viewer_context::{CommandReceiver, command_channel};

    use super::*;

    struct StandInViewer(Option<ActiveTimeline>);

    impl TimelineSource for StandInViewer {
        fn active_timeline(&self) -> Option<ActiveTimeline> {
            self.0.as_ref().map(|timeline| ActiveTimeline {
                store_id: timeline.store_id.clone(),
                range: timeline.range,
                cursor: timeline.cursor,
            })
        }
    }

    fn viewer_with_frames(first: i64, last: i64) -> StandInViewer {
        viewer_at(first, first, last)
    }

    fn viewer_at(cursor: i64, first: i64, last: i64) -> StandInViewer {
        StandInViewer(Some(ActiveTimeline {
            store_id: StoreId::random(StoreKind::Recording, "demo"),
            range: AbsoluteTimeRange::new(
                TimeInt::new_temporal(first),
                TimeInt::new_temporal(last),
            ),
            cursor: TimeInt::new_temporal(cursor),
        }))
    }

    fn scrub_with(requests: TimelineRequests, sender: CommandSender) -> TimelineScrub {
        TimelineScrub::new(requests, TimelinePosition::default(), sender)
    }

    #[test]
    fn the_viewer_publishes_its_cursor_as_a_fraction_of_the_timeline() {
        let (sender, _receiver) = command_channel();
        let position = TimelinePosition::default();
        let scrub = TimelineScrub::new(TimelineRequests::default(), position.clone(), sender);

        scrub.publish_position(&StandInViewer(None));
        assert_eq!(position.latest(), None, "no timeline, no position");

        scrub.publish_position(&viewer_at(150, 100, 300));
        assert_eq!(position.latest(), Some(0.25));
        scrub.publish_position(&viewer_at(300, 100, 300));
        assert_eq!(position.latest(), Some(1.0));
        scrub.publish_position(&viewer_at(50, 100, 300));
        assert_eq!(position.latest(), Some(0.0), "before the data is its start");
        scrub.publish_position(&viewer_at(7, 7, 7));
        assert_eq!(position.latest(), Some(0.0), "a single time is the start");
        scrub.publish_position(&StandInViewer(None));
        assert_eq!(position.latest(), Some(0.0), "the last position stays");
    }

    #[test]
    fn a_seek_and_the_published_position_agree() {
        let range = AbsoluteTimeRange::new(TimeInt::new_temporal(-40), TimeInt::new_temporal(60));
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let cursor = time_at(fraction, range).floor();
            let published = fraction_at(cursor, range);
            assert!(
                (published - fraction).abs() < 1e-12,
                "{published} for {fraction}"
            );
        }
    }

    fn sent_commands(receiver: &CommandReceiver) -> Vec<String> {
        let mut sent = Vec::new();
        while let Some((_, command)) = receiver.recv_system() {
            let SystemCommand::TimeControlCommands { time_commands, .. } = command else {
                panic!("not a time control command");
            };
            sent.extend(time_commands.iter().map(|command| format!("{command:?}")));
        }
        sent
    }

    #[test]
    fn a_seek_pauses_at_its_fraction_of_the_timeline_and_play_resumes() {
        let (sender, receiver) = command_channel();
        let requests = TimelineRequests::default();
        let scrub = scrub_with(requests.clone(), sender);
        let viewer = viewer_with_frames(100, 300);

        requests.push(TimelineRequest::SeekAndPause { fraction: 0.25 });
        requests.push(TimelineRequest::Play);
        assert!(scrub.apply(&viewer));

        assert_eq!(
            sent_commands(&receiver),
            [
                format!("{:?}", TimeControlCommand::SetPlayState(PlayState::Paused)),
                format!(
                    "{:?}",
                    TimeControlCommand::SetTimeClamped(TimeReal::from(150.0))
                ),
                format!("{:?}", TimeControlCommand::SetPlayState(PlayState::Playing)),
            ]
        );
        assert!(!scrub.apply(&viewer), "nothing new to send");
    }

    #[test]
    fn the_ends_of_the_slider_are_the_first_and_last_time_of_the_data() {
        let range = AbsoluteTimeRange::new(TimeInt::new_temporal(-50), TimeInt::new_temporal(50));
        assert_eq!(time_at(0.0, range), TimeReal::from(-50.0));
        assert_eq!(time_at(1.0, range), TimeReal::from(50.0));
        assert_eq!(time_at(0.5, range), TimeReal::from(0.0));
    }

    #[test]
    fn requests_before_a_timeline_exists_are_dropped() {
        let (sender, receiver) = command_channel();
        let requests = TimelineRequests::default();
        let scrub = scrub_with(requests.clone(), sender);

        requests.push(TimelineRequest::SeekAndPause { fraction: 0.5 });
        assert!(!scrub.apply(&StandInViewer(None)));
        assert!(sent_commands(&receiver).is_empty());

        assert!(
            !scrub.apply(&viewer_with_frames(0, 10)),
            "the dropped seek does not come back once the recording arrives"
        );
    }
}
