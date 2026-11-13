//! Letting begin-frame waits finish before their senders go at Stop.
//!
//! `qar_render_sender_try_begin_frame` starts a begin-frame wait in the library that holds the
//! sender. So that no sender outlives its handle into the session's teardown, at Stop the
//! senders with a wait are polled until it ends, within a bound, and the frames it yields are
//! dropped unshown; a released sender is dropped only after its begin-frame wait has ended.

use std::time::{Duration, Instant};

use frame_sink::UserId;
use qaros::{OwnedFrame, RenderSender};

/// How long a Stop polls for begin-frame waits to end. A live receiver's wait ends within a
/// frame interval or two; a paused receiver's does not end at all and costs the whole bound,
/// inside the stop watchdog's 1 s.
pub const BEGIN_WAIT_DRAIN_LIMIT: Duration = Duration::from_millis(500);

const DRAIN_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Polls each sender's begin-frame wait until it has ended, or `limit` has passed. Returns the
/// frames the waits yielded, for the caller to drop unshown before their senders.
pub(crate) fn drain_begin_waits(
    mut waiting: Vec<(UserId, &mut RenderSender)>,
    limit: Duration,
) -> Vec<(UserId, OwnedFrame)> {
    if waiting.is_empty() {
        return Vec::new();
    }
    let started = Instant::now();
    let mut ended = Vec::new();
    let mut frames = Vec::new();
    loop {
        waiting.retain_mut(|(user, sender)| match sender.try_begin_owned_frame() {
            Ok(Some(frame)) => {
                ended.push(*user);
                frames.push((*user, frame));
                false
            }
            Ok(None) => true,
            Err(error) => {
                log::debug!("qaros: the begin-frame wait of {user} ended in {error}");
                ended.push(*user);
                false
            }
        });
        if waiting.is_empty() || started.elapsed() >= limit {
            break;
        }
        std::thread::sleep(DRAIN_POLL_INTERVAL);
    }
    let still_waiting: Vec<UserId> = waiting.iter().map(|(user, _)| *user).collect();
    let consequence = if still_waiting.is_empty() {
        ""
    } else {
        " (their senders outlive their handles)"
    };
    log::info!(
        "qaros: begin-frame waits at stop: ended for {} in {} ms; still running for {}{consequence}",
        describe_users(&ended),
        started.elapsed().as_millis(),
        describe_users(&still_waiting),
    );
    frames
}

fn describe_users(users: &[UserId]) -> String {
    if users.is_empty() {
        return "nobody".to_owned();
    }
    users
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
