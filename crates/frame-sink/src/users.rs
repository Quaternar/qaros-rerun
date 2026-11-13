//! Who is rendered for: users appear and leave through [`UserEvent`]s, a [`RenderPolicy`]
//! decides whether a new user gets frames, and the [`UserRoster`] keeps the rendered and the
//! waiting users in step with both.

use std::fmt;

/// A user the renderer may render for: one QAROS peer that asked for a stream, or one
/// simulated head in the image sink. Assigned by the sink, stable while the user is present.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub u32);

impl fmt::Display for UserId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "user-{}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserInfo {
    pub id: UserId,
    /// For logs: a display name or a peer id.
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserEvent {
    Joined(UserInfo),
    Left(UserId),
}

/// Decides whether a user gets frames. Asked when the user appears, and again for every
/// waiting user, oldest first, whenever a rendered user leaves.
pub trait RenderPolicy {
    fn should_render(&mut self, candidate: &UserInfo, rendered: &[UserInfo]) -> bool;
}

/// The default policy: render for every user, up to `maximum` at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaxUsers {
    pub maximum: usize,
}

impl RenderPolicy for MaxUsers {
    fn should_render(&mut self, _candidate: &UserInfo, rendered: &[UserInfo]) -> bool {
        rendered.len() < self.maximum
    }
}

/// One change to the set of rendered users, in the order the sink must apply them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosterChange {
    /// Start rendering: create the user's stream.
    Admitted(UserInfo),
    /// Stop rendering and release everything held for the user.
    Released(UserId),
}

pub struct UserRoster {
    policy: Box<dyn RenderPolicy>,
    rendered: Vec<UserInfo>,
    waiting: Vec<UserInfo>,
}

impl fmt::Debug for UserRoster {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserRoster")
            .field("rendered", &self.rendered)
            .field("waiting", &self.waiting)
            .finish_non_exhaustive()
    }
}

impl UserRoster {
    pub fn new(policy: Box<dyn RenderPolicy>) -> Self {
        Self {
            policy,
            rendered: Vec::new(),
            waiting: Vec::new(),
        }
    }

    pub fn rendered(&self) -> &[UserInfo] {
        &self.rendered
    }

    pub fn waiting(&self) -> &[UserInfo] {
        &self.waiting
    }

    pub fn is_rendered(&self, user: UserId) -> bool {
        self.rendered.iter().any(|info| info.id == user)
    }

    /// Asks the policy whether `candidate` would be rendered now, beside the rendered users,
    /// without offering it.
    pub fn would_render(&mut self, candidate: &UserInfo) -> bool {
        self.policy.should_render(candidate, &self.rendered)
    }

    /// Asks the policy whether `candidate` would be rendered now if every user in `ahead` were
    /// rendered first, without offering anyone.
    pub fn would_render_behind(&mut self, candidate: &UserInfo, ahead: &[UserInfo]) -> bool {
        let mut rendered = self.rendered.clone();
        rendered.extend_from_slice(ahead);
        self.policy.should_render(candidate, &rendered)
    }

    /// Applies the events in order. A user already known is not offered twice; a waiting
    /// user who leaves is dropped without a change.
    pub fn apply(&mut self, events: impl IntoIterator<Item = UserEvent>) -> Vec<RosterChange> {
        let mut changes = Vec::new();
        for event in events {
            match event {
                UserEvent::Joined(user) => self.offer(user, &mut changes),
                UserEvent::Left(user) => self.remove(user, &mut changes),
            }
        }
        changes
    }

    fn knows(&self, user: UserId) -> bool {
        self.rendered
            .iter()
            .chain(&self.waiting)
            .any(|info| info.id == user)
    }

    fn offer(&mut self, user: UserInfo, changes: &mut Vec<RosterChange>) {
        if self.knows(user.id) {
            return;
        }
        if self.policy.should_render(&user, &self.rendered) {
            self.rendered.push(user.clone());
            changes.push(RosterChange::Admitted(user));
        } else {
            self.waiting.push(user);
        }
    }

    fn remove(&mut self, user: UserId, changes: &mut Vec<RosterChange>) {
        self.waiting.retain(|info| info.id != user);
        let before = self.rendered.len();
        self.rendered.retain(|info| info.id != user);
        if self.rendered.len() == before {
            return;
        }
        changes.push(RosterChange::Released(user));
        for candidate in std::mem::take(&mut self.waiting) {
            if self.policy.should_render(&candidate, &self.rendered) {
                self.rendered.push(candidate.clone());
                changes.push(RosterChange::Admitted(candidate));
            } else {
                self.waiting.push(candidate);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: u32) -> UserInfo {
        UserInfo {
            id: UserId(id),
            label: format!("headset {id}"),
        }
    }

    fn roster(maximum: usize) -> UserRoster {
        UserRoster::new(Box::new(MaxUsers { maximum }))
    }

    #[test]
    fn renders_up_to_the_maximum_and_lets_a_waiting_user_in_when_one_leaves() {
        let mut roster = roster(2);
        let changes = roster.apply([
            UserEvent::Joined(user(1)),
            UserEvent::Joined(user(2)),
            UserEvent::Joined(user(3)),
        ]);
        assert_eq!(
            changes,
            [
                RosterChange::Admitted(user(1)),
                RosterChange::Admitted(user(2))
            ]
        );
        assert_eq!(roster.waiting(), [user(3)]);

        let changes = roster.apply([UserEvent::Left(UserId(1))]);
        assert_eq!(
            changes,
            [
                RosterChange::Released(UserId(1)),
                RosterChange::Admitted(user(3))
            ]
        );
        assert_eq!(roster.rendered(), [user(2), user(3)]);
        assert!(roster.waiting().is_empty());
    }

    #[test]
    fn repeated_joins_and_unknown_leaves_change_nothing() {
        let mut roster = roster(2);
        roster.apply([UserEvent::Joined(user(1))]);
        assert!(
            roster
                .apply([UserEvent::Joined(user(1)), UserEvent::Left(UserId(9))])
                .is_empty()
        );
        assert!(roster.is_rendered(UserId(1)));
    }

    #[test]
    fn asking_whether_a_user_would_be_rendered_offers_nothing() {
        let mut roster = roster(1);
        assert!(roster.would_render(&user(1)));
        assert!(roster.rendered().is_empty() && roster.waiting().is_empty());
        roster.apply([UserEvent::Joined(user(1))]);
        assert!(!roster.would_render(&user(2)), "the one slot is taken");
    }

    #[test]
    fn a_waiting_user_who_leaves_is_forgotten() {
        let mut roster = roster(1);
        roster.apply([UserEvent::Joined(user(1)), UserEvent::Joined(user(2))]);
        assert!(roster.apply([UserEvent::Left(UserId(2))]).is_empty());
        assert_eq!(
            roster.apply([UserEvent::Left(UserId(1))]),
            [RosterChange::Released(UserId(1))]
        );
        assert!(roster.rendered().is_empty());
    }

    #[test]
    fn a_custom_policy_decides_per_user() {
        struct OddUsersOnly;
        impl RenderPolicy for OddUsersOnly {
            fn should_render(&mut self, candidate: &UserInfo, _rendered: &[UserInfo]) -> bool {
                candidate.id.0 % 2 == 1
            }
        }
        let mut roster = UserRoster::new(Box::new(OddUsersOnly));
        let changes = roster.apply((1..=4).map(|id| UserEvent::Joined(user(id))));
        assert_eq!(
            changes,
            [
                RosterChange::Admitted(user(1)),
                RosterChange::Admitted(user(3))
            ]
        );
        assert_eq!(roster.waiting(), [user(2), user(4)]);
    }
}
