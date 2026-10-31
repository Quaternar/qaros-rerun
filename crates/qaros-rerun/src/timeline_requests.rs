//! What the timeline panel asks of the viewer's timeline, and where the viewer's time cursor
//! stands for the panel's thumb. The panel's gestures push requests; the viewer takes them once
//! per frame ([`crate::timeline_scrub`]) and publishes its cursor back.

use std::sync::{Arc, Mutex, PoisonError};

/// What the timeline panel asked of the viewer's timeline, in the order it asked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TimelineRequest {
    /// Pause and put the time cursor at this fraction of the active timeline, 0 at its first
    /// time and 1 at its last.
    SeekAndPause {
        fraction: f64,
    },
    Play,
    /// Stop playback where the cursor stands; the timeline panel sends it from its Play/Pause
    /// button while the viewer plays.
    Pause,
}

/// Requests the panel sent that the viewer has not taken yet. A drag sends many seeks; a seek
/// directly after another replaces it, so the queue holds what a frame needs to apply and
/// stays short however fast the panel sends.
#[derive(Clone, Default)]
pub struct TimelineRequests {
    pending: Arc<Mutex<Vec<TimelineRequest>>>,
}

/// The viewer may not take requests for a while (a window that is not repainting), so the
/// oldest go once this many wait.
const LONGEST_QUEUE: usize = 32;

impl TimelineRequests {
    pub fn push(&self, request: TimelineRequest) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let replaces_last_seek = matches!(request, TimelineRequest::SeekAndPause { .. })
            && matches!(pending.last(), Some(TimelineRequest::SeekAndPause { .. }));
        if replaces_last_seek {
            pending.pop();
        }
        if pending.len() == LONGEST_QUEUE {
            pending.remove(0);
        }
        pending.push(request);
    }

    pub fn take(&self) -> Vec<TimelineRequest> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Where the viewer's time cursor stands, as a fraction of the active timeline (the range the
/// seeks use), for the timeline panel's thumb to follow; nothing until the viewer shows a
/// recording with a timeline.
#[derive(Clone, Default)]
pub struct TimelinePosition {
    fraction: Arc<Mutex<Option<f64>>>,
}

impl TimelinePosition {
    pub fn publish(&self, fraction: f64) {
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        *self.fraction.lock().unwrap_or_else(PoisonError::into_inner) = Some(fraction);
    }

    pub fn latest(&self) -> Option<f64> {
        *self.fraction.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seek_replaces_the_seek_before_it_and_keeps_a_play_between() {
        let requests = TimelineRequests::default();
        requests.push(TimelineRequest::SeekAndPause { fraction: 0.1 });
        requests.push(TimelineRequest::SeekAndPause { fraction: 0.2 });
        requests.push(TimelineRequest::Play);
        requests.push(TimelineRequest::SeekAndPause { fraction: 0.3 });
        requests.push(TimelineRequest::SeekAndPause { fraction: 0.4 });

        assert_eq!(
            requests.take(),
            [
                TimelineRequest::SeekAndPause { fraction: 0.2 },
                TimelineRequest::Play,
                TimelineRequest::SeekAndPause { fraction: 0.4 },
            ]
        );
        assert_eq!(requests.take(), [], "taken requests are gone");
    }

    #[test]
    fn the_queue_drops_the_oldest_once_full() {
        let requests = TimelineRequests::default();
        let mut last_seek = TimelineRequest::Play;
        let mut fraction = 0.0;
        for _ in 0..LONGEST_QUEUE {
            requests.push(TimelineRequest::Play);
            last_seek = TimelineRequest::SeekAndPause { fraction };
            requests.push(last_seek);
            fraction += 0.01;
        }

        let taken = requests.take();
        assert_eq!(taken.len(), LONGEST_QUEUE);
        assert_eq!(taken.first(), Some(&TimelineRequest::Play));
        assert_eq!(taken.last(), Some(&last_seek));
    }

    #[test]
    fn the_position_keeps_the_latest_fraction_within_the_timeline() {
        let position = TimelinePosition::default();
        assert_eq!(position.latest(), None);
        position.publish(0.375);
        assert_eq!(position.latest(), Some(0.375));
        position.publish(1.5);
        assert_eq!(position.latest(), Some(1.0));
        position.publish(f64::NAN);
        assert_eq!(position.latest(), Some(0.0));
    }
}
