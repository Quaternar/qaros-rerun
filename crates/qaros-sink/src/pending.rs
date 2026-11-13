//! Render senders being created without blocking the UI thread. Creating a sender for a target
//! that never answers takes the whole creation timeout; a blocking creation would hold the
//! viewer for every user, again for each such target after its back-off.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use frame_sink::{UserId, UserInfo};
use qaros::{PeerId, PendingRenderSender, RenderSender, StatusCode};

use crate::users::ReleaseCause;

/// When a creation that has not called back is given up. The runtime's own timeout
/// ([`SENDER_CREATION_TIMEOUT`](crate::connection::SENDER_CREATION_TIMEOUT)) should end it first;
/// this deadline is a backstop, since a creation that never called back and that nobody gave
/// up would keep its user from being tried again for good. It is set well past the time a
/// slow first sender takes, so a creation that would still succeed is not dropped.
pub(crate) const SENDER_CREATION_DEADLINE: Duration = Duration::from_secs(30);

/// A creation in flight for an admitted user.
struct PendingCreation {
    user: UserInfo,
    peer: PeerId,
    creation: PendingRenderSender,
    started: Instant,
}

/// A creation that finished, successfully or not.
pub(crate) struct FinishedCreation {
    pub user: UserInfo,
    pub peer: PeerId,
    pub sender: Result<RenderSender, CreationFailure>,
}

pub(crate) struct CreationFailure {
    pub message: String,
    /// [`ReleaseCause::Unanswered`] when the runtime timed the creation out or it passed the
    /// deadline.
    pub cause: ReleaseCause,
}

impl CreationFailure {
    fn of(error: &qaros::Error) -> Self {
        Self {
            message: error.to_string(),
            cause: if error.has_code(StatusCode::TIMEOUT) {
                ReleaseCause::Unanswered
            } else {
                ReleaseCause::Failed
            },
        }
    }
}

/// The sink's creations in flight, by user. Dropping one cancels it.
pub(crate) struct PendingSenders {
    creations: BTreeMap<UserId, PendingCreation>,
    deadline: Duration,
}

impl Default for PendingSenders {
    fn default() -> Self {
        Self {
            creations: BTreeMap::new(),
            deadline: SENDER_CREATION_DEADLINE,
        }
    }
}

impl PendingSenders {
    pub(crate) fn set_deadline(&mut self, deadline: Duration) {
        self.deadline = deadline;
    }

    pub(crate) fn insert(&mut self, user: UserInfo, peer: PeerId, creation: PendingRenderSender) {
        self.creations.insert(
            user.id,
            PendingCreation {
                user,
                peer,
                creation,
                started: Instant::now(),
            },
        );
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.creations.is_empty()
    }

    /// Takes every creation that finished since the last call, and every one past the
    /// deadline as failed (dropping it cancels it), oldest user first.
    pub(crate) fn take_finished(&mut self) -> Vec<FinishedCreation> {
        let deadline = self.deadline;
        let mut outcomes = Vec::new();
        for (user, pending) in &mut self.creations {
            if let Some(sender) = pending.creation.poll() {
                outcomes.push((*user, sender.map_err(|error| CreationFailure::of(&error))));
            } else if pending.started.elapsed() >= deadline {
                outcomes.push((
                    *user,
                    Err(CreationFailure {
                        message: format!("no answer within {deadline:?}; given up"),
                        cause: ReleaseCause::Unanswered,
                    }),
                ));
            }
        }
        outcomes
            .into_iter()
            .filter_map(|(user, sender)| {
                let pending = self.creations.remove(&user)?;
                Some(FinishedCreation {
                    user: pending.user,
                    peer: pending.peer,
                    sender,
                })
            })
            .collect()
    }
}
