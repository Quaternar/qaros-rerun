//! A deadline of the viewer's own for every stop request.
//!
//! QAROS kills an app it stopped about 1.3 s after the Stop (`onboarding-and-sessions.mdx`
//! L278 at `b1338df`). A teardown ended by that kill loses the log tail and the exit line, so
//! the watchdog ends the process itself a little earlier, after one line naming the teardown
//! step still in progress.

use std::sync::Once;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::process_exit::end_process_after_bounded_flush;

/// How long the process may run after a stop request before the watchdog ends it: inside
/// QAROS's 1.3 s, with room for the bounded flush.
pub const STOP_DEADLINE: Duration = Duration::from_secs(1);

/// Exit code of a process the watchdog ended before its teardown was done.
pub const STOP_DEADLINE_CODE: i32 = 1;

/// The teardown step in progress, in the order the teardown takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TeardownStep {
    NotStarted = 0,
    /// The headset's sink drops with its render senders.
    LeaveSink = 1,
    /// The GUI panels are hidden for the next run.
    HidePanels = 2,
    /// The app volume is closed.
    CloseVolume = 3,
    /// The subscriptions are cancelled and the session and its runtime destroyed.
    SessionDestroy = 4,
    /// The library is destroyed (and stays mapped).
    LibraryDestroy = 5,
    /// The teardown is done; the logs flush and the process ends.
    EndProcess = 6,
}

impl TeardownStep {
    const ALL: [Self; 7] = [
        Self::NotStarted,
        Self::LeaveSink,
        Self::HidePanels,
        Self::CloseVolume,
        Self::SessionDestroy,
        Self::LibraryDestroy,
        Self::EndProcess,
    ];

    fn from_code(code: u8) -> Self {
        Self::ALL
            .get(usize::from(code))
            .copied()
            .unwrap_or(Self::NotStarted)
    }

    fn describe(self) -> &'static str {
        match self {
            Self::NotStarted => "not started (onboarding, or the viewer frame before the teardown)",
            Self::LeaveSink => "leave sink (render senders)",
            Self::HidePanels => "hide panels",
            Self::CloseVolume => "close volume",
            Self::SessionDestroy => "session destroy",
            Self::LibraryDestroy => "library destroy",
            Self::EndProcess => "end process (flush)",
        }
    }
}

static CURRENT_STEP: AtomicU8 = AtomicU8::new(TeardownStep::NotStarted as u8);

/// Marks `step` as the teardown step in progress, for the watchdog's line.
pub fn enter_teardown_step(step: TeardownStep) {
    CURRENT_STEP.store(step as u8, Ordering::Release);
}

pub fn current_teardown_step() -> TeardownStep {
    TeardownStep::from_code(CURRENT_STEP.load(Ordering::Acquire))
}

static WATCHDOG: Once = Once::new();

/// Starts the watchdog on the first stop request of the process; later calls do nothing.
/// [`STOP_DEADLINE`] later, if the process still runs, it logs the teardown step in progress,
/// flushes for at most [`BOUNDED_FLUSH_WAIT`](crate::BOUNDED_FLUSH_WAIT) and terminates.
pub fn start_stop_watchdog() {
    WATCHDOG.call_once(|| {
        watch(STOP_DEADLINE, |line| {
            end_process_after_bounded_flush(STOP_DEADLINE_CODE, line)
        });
    });
}

/// Calls `end` with the line naming the step in progress once `deadline` has passed. The line
/// gives the time since the stop request as measured when the watchdog wakes: a process held
/// suspended past its deadline shows up as such.
fn watch(deadline: Duration, end: impl FnOnce(String) + Send + 'static) {
    let stop_requested = Instant::now();
    let spawned = std::thread::Builder::new()
        .name("qaros stop watchdog".to_owned())
        .spawn(move || {
            std::thread::sleep(deadline);
            end(deadline_line(
                stop_requested.elapsed(),
                deadline,
                current_teardown_step(),
            ));
        });
    if let Err(error) = spawned {
        log::warn!("qaros: no stop watchdog ({error}); a hung teardown ends by QAROS's kill");
    }
}

fn deadline_line(elapsed: Duration, deadline: Duration, step: TeardownStep) -> String {
    format!(
        "qaros: still running {} ms after the stop request (deadline {deadline:?}), in teardown \
         step {}; ending the process",
        elapsed.as_millis(),
        step.describe()
    )
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn the_watchdog_names_the_teardown_step_in_progress_once_the_deadline_passes() {
        enter_teardown_step(TeardownStep::SessionDestroy);
        let (ended, end_seen) = mpsc::channel();
        let started = Instant::now();

        watch(Duration::from_millis(30), move |line| {
            ended.send(line).unwrap();
        });

        let line = end_seen.recv_timeout(Duration::from_secs(5)).unwrap();
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(30));
        let elapsed_millis: u128 = line
            .strip_prefix("qaros: still running ")
            .and_then(|rest| rest.split(' ').next())
            .and_then(|millis| millis.parse().ok())
            .unwrap_or_else(|| panic!("no elapsed time in {line:?}"));
        assert!(
            (30..=waited.as_millis()).contains(&elapsed_millis),
            "{elapsed_millis} ms, waited {waited:?}"
        );
        assert!(
            line.ends_with(
                " ms after the stop request (deadline 30ms), in teardown step session destroy; \
                 ending the process"
            ),
            "{line}"
        );
        enter_teardown_step(TeardownStep::NotStarted);
    }

    #[test]
    fn a_process_suspended_past_its_deadline_reports_the_time_it_really_ran() {
        assert_eq!(
            deadline_line(
                Duration::from_millis(3157),
                STOP_DEADLINE,
                TeardownStep::SessionDestroy
            ),
            "qaros: still running 3157 ms after the stop request (deadline 1s), in teardown \
             step session destroy; ending the process"
        );
    }

    #[test]
    fn every_step_survives_the_round_trip_through_its_code() {
        for step in TeardownStep::ALL {
            assert_eq!(TeardownStep::from_code(step as u8), step);
        }
        assert_eq!(TeardownStep::from_code(200), TeardownStep::NotStarted);
    }

    #[test]
    fn the_deadline_and_its_flush_end_the_process_inside_qaros_kill_window() {
        let qaros_kill_after_stop = Duration::from_millis(1300);
        assert!(STOP_DEADLINE + crate::BOUNDED_FLUSH_WAIT < qaros_kill_after_stop);
    }
}
