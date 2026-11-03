//! The app volume is hidden at Stop and shown again at the next start, as the GUI panels are
//! (ADR-0089). QAROS has no hidden state for a volume ([HDR] `QarAppVolumeLifetimeStatus`
//! L1299-1303). What a headset draws is the volume's used-by peers: its mixer keeps a box, a
//! name tag and the app's last content only for a volume that lists the headset's peer, and drops
//! all three once the peer leaves the list (QuaternAR `StereoKitGUI/src/Table/TableControl.hpp`,
//! `UpdateTablesFromAppVolumes`). So every used-by peer is removed and the volume stays open:
//! QAROS keeps it, with its place, and the next run takes it over by common name. The viewer never
//! closes it. Each render sender of the next run lists its target again when it is created, which
//! shows the volume to that headset.
//!
//! The hide at Stop has to fit the stop watchdog's deadline, so it reads nothing from QAROS on
//! the way (ADR-0093). The peers come from the volume's update subscription ([`ReportedUsers`]).
//! Each removal is one proposal on the volume's `PeerChange` field, and the Hub refuses a
//! proposal whose base is older than that field's last commit, so the removals go one at a time,
//! spaced by [`HideTiming::removal_spacing`], each on the base the previous one's commit left.
//! The hide counts once the Hub's next bake of the volume lists none of the removed peers; a peer
//! it still lists is removed again. A hide not confirmed within [`HideTiming::budget`] leaves the
//! volume as it is, with a warning, and the next start finishes it: a volume taken over at start
//! that still lists peers is hidden from them before any sender of the run exists.
//!
//! [HDR]: https://github.com/Quaternar/qaros/blob/e0c0195/qar-streaming-c/include/qar_streaming.h
//! [QS]: https://github.com/Quaternar/qar_streaming/tree/e487a31158caca9a9cd2fcf6073456b316d8b7c6/libs "QAROS streaming library source (private)"

use std::time::{Duration, Instant};

use qaros::qaros_sys::{QarAppVolumeId, QarPeerId};
use qaros::{AppVolumeId, PeerId, Session};

use crate::raw_result::check;
use crate::volume_users::{ReportedUsers, UsersReport};

/// How a hide is paced and bounded.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HideTiming {
    /// Between two removals: the Hub commits a removal and the viewer sees the commit within a
    /// few milliseconds when both run on one machine (the 2026-11-02 Hub log accepted a removal
    /// 0.1 ms after it was sent), so the next removal goes on the fresh base.
    pub(crate) removal_spacing: Duration,
    /// From the first removal until the hide is given up. The Hub bakes a changed volume 100 ms
    /// after its last change ([QS] `qar-streaming/src/AppVolumeProcessor.hpp` `BakeDelay`), so a
    /// hide of up to four peers is confirmed within about 150 ms.
    pub(crate) budget: Duration,
}

/// At Stop. With the 31 ms the Ctrl+C Stop of 2026-11-02 16:56:51 UTC took before this step and
/// the slowest session and library destroy seen that day (388 ms), the teardown ends inside the
/// 600 ms stop deadline.
pub(crate) const STOP_HIDE_TIMING: HideTiming = HideTiming {
    removal_spacing: Duration::from_millis(15),
    budget: Duration::from_millis(180),
};

/// At start, where no watchdog runs and the connect already waits about 4 s for its listing.
pub(crate) const START_HIDE_TIMING: HideTiming = HideTiming {
    removal_spacing: Duration::from_millis(15),
    budget: Duration::from_secs(1),
};

/// The calls that hide the volume, apart so the sequence is tested without QAROS.
pub(crate) trait VolumeUsers {
    /// The used-by peers QAROS reported last; nothing is asked of QAROS.
    fn reported(&self) -> UsersReport;
    /// The first report after the `seen_count`th, or `None` at `deadline`.
    fn wait_for_report_after(&self, seen_count: u64, deadline: Instant) -> Option<UsersReport>;
    /// Sends the removal of `peer` and returns without waiting for the Hub's answer.
    fn stop_using(&self, peer: PeerId) -> Result<(), String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VolumeHide {
    /// Open, listing none of the removed peers: no headset draws it, and the Hub keeps it with
    /// its place.
    Hidden {
        removed_peers: Vec<PeerId>,
        took: Duration,
    },
    /// Open, and possibly still listing `peers`, as the hide could not be completed.
    Unfinished { peers: Vec<PeerId>, reason: String },
}

/// Hides the volume at Stop and logs the outcome. `served_peers` are the peers this run's
/// senders streamed to, removed when QAROS never reported the used-by peers.
pub(crate) fn hide_at_stop(
    volume: &impl VolumeUsers,
    served_peers: &[PeerId],
    common_name: &str,
) -> VolumeHide {
    let hide = hide_from_every_peer(volume, served_peers, STOP_HIDE_TIMING);
    match &hide {
        VolumeHide::Hidden {
            removed_peers,
            took,
        } if removed_peers.is_empty() => log::info!(
            "qaros: app volume {common_name:?} is used by no peer ({} us); QAROS keeps it with its place, and the next run takes it over and shows it again",
            took.as_micros()
        ),
        VolumeHide::Hidden {
            removed_peers,
            took,
        } => log::info!(
            "qaros: hid app volume {common_name:?} from every peer that used it ({}) in {} ms, as the Hub's bake of it confirms; QAROS keeps it with its place, and the next run takes it over and shows it again",
            describe_peers(removed_peers),
            took.as_millis()
        ),
        VolumeHide::Unfinished { peers, reason } => log::warn!(
            "qaros: app volume {common_name:?} may still show in {} ({reason}); it stays open as it is, and the next start hides it from them before its senders list their headsets again",
            describe_peers_or_unknown(peers)
        ),
    }
    hide
}

/// Hides a volume taken over at start from the peers it still lists, which an earlier run's
/// Stop did not finish removing, before any sender of this run lists its headset. Logs the
/// outcome.
pub(crate) fn hide_left_over_at_start(volume: &impl VolumeUsers, common_name: &str) -> VolumeHide {
    let left_over = volume.reported().peers.unwrap_or_default();
    log::info!(
        "qaros: app volume {common_name:?} still lists {} from an earlier run; hiding it from them before this run's senders list their headsets",
        describe_peers(&left_over)
    );
    let hide = hide_from_every_peer(volume, &[], START_HIDE_TIMING);
    match &hide {
        VolumeHide::Hidden {
            removed_peers,
            took,
        } => log::info!(
            "qaros: hid app volume {common_name:?} from {} in {} ms",
            describe_peers(removed_peers),
            took.as_millis()
        ),
        VolumeHide::Unfinished { peers, reason } => log::warn!(
            "qaros: app volume {common_name:?} may still show in {} ({reason}); this run's Stop tries again",
            describe_peers_or_unknown(peers)
        ),
    }
    hide
}

/// Removes every peer QAROS reported (else every served peer) and waits for the Hub to confirm.
pub(crate) fn hide_from_every_peer(
    volume: &impl VolumeUsers,
    served_peers: &[PeerId],
    timing: HideTiming,
) -> VolumeHide {
    let started = Instant::now();
    let reported = volume.reported();
    let peers = match reported.peers {
        Some(peers) => peers,
        None if served_peers.is_empty() => {
            return VolumeHide::Unfinished {
                peers: Vec::new(),
                reason: "QAROS has not reported which peers use it".to_owned(),
            };
        }
        None => without_repeats(served_peers),
    };
    if peers.is_empty() {
        return VolumeHide::Hidden {
            removed_peers: Vec::new(),
            took: started.elapsed(),
        };
    }
    let deadline = started + timing.budget;
    match remove_until_confirmed(volume, &peers, reported.count, deadline, timing) {
        Ok(()) => VolumeHide::Hidden {
            removed_peers: peers,
            took: started.elapsed(),
        },
        Err(reason) => VolumeHide::Unfinished { peers, reason },
    }
}

/// Removes `peers` one at a time, then waits for a bake. Peers a bake still lists are removed
/// again, until none is listed or `deadline` passes.
fn remove_until_confirmed(
    volume: &impl VolumeUsers,
    peers: &[PeerId],
    reports_seen: u64,
    deadline: Instant,
    timing: HideTiming,
) -> Result<(), String> {
    let mut still_listed = peers.to_vec();
    let mut reports_seen = reports_seen;
    loop {
        for (index, &peer) in still_listed.iter().enumerate() {
            if index > 0 {
                std::thread::sleep(timing.removal_spacing);
            }
            volume.stop_using(peer)?;
        }
        let Some(report) = volume.wait_for_report_after(reports_seen, deadline) else {
            return Err(format!(
                "the Hub did not confirm the removal of {} within {} ms",
                describe_peers(&still_listed),
                timing.budget.as_millis()
            ));
        };
        reports_seen = report.count;
        let listed = report.peers.unwrap_or_default();
        still_listed = peers
            .iter()
            .copied()
            .filter(|peer| listed.contains(peer))
            .collect();
        if still_listed.is_empty() {
            return Ok(());
        }
        log::info!(
            "qaros: the Hub still lists {} on the app volume; removing again",
            describe_peers(&still_listed)
        );
    }
}

fn without_repeats(peers: &[PeerId]) -> Vec<PeerId> {
    let mut unique = Vec::with_capacity(peers.len());
    for &peer in peers {
        if !unique.contains(&peer) {
            unique.push(peer);
        }
    }
    unique
}

fn describe_peers(peers: &[PeerId]) -> String {
    let listed: Vec<String> = peers.iter().map(ToString::to_string).collect();
    listed.join(", ")
}

fn describe_peers_or_unknown(peers: &[PeerId]) -> String {
    if peers.is_empty() {
        return "the headsets that used it".to_owned();
    }
    describe_peers(peers)
}

/// The volume as this session reaches it through the raw table: the binding has no used-by
/// calls (`qar_app_volumes_update_used_by_peers`).
pub(crate) struct SessionVolume<'session> {
    pub(crate) session: &'session Session,
    pub(crate) volume: AppVolumeId,
    pub(crate) reported: &'session ReportedUsers,
}

impl VolumeUsers for SessionVolume<'_> {
    fn reported(&self) -> UsersReport {
        self.reported.latest()
    }

    fn wait_for_report_after(&self, seen_count: u64, deadline: Instant) -> Option<UsersReport> {
        self.reported.wait_for_report_after(seen_count, deadline)
    }

    /// One removal per call: QAROS turns each removal into a proposal of its own
    /// ([QS] `qar-streaming-c/src/v0/app_volumes.cpp` `qar_impl_app_volumes_update_used_by_peers`),
    /// which its outbox publishes before the call returns
    /// ([QS] `qar-streaming/src/SharedObjectOutbox.hpp` `Submit`).
    fn stop_using(&self, peer: PeerId) -> Result<(), String> {
        let library = self.session.library();
        let api = library.raw_api();
        let volume_id = QarAppVolumeId {
            data: self.volume.0,
        };
        let removal = QarPeerId { data: peer.0 };
        // SAFETY: the session is alive and used on this thread; the id and the removal live for
        // the call, and no additions are passed.
        let updated = unsafe {
            (api.qar_impl_app_volumes_update_used_by_peers)(
                self.session.as_raw(),
                &raw const volume_id,
                std::ptr::null(),
                0,
                &raw const removal,
                1,
            )
        };
        check(api, "qar_app_volumes_update_used_by_peers", updated)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// The Hub's bake delay, shortened so the tests run fast; the pacing is what is asserted.
    const BAKE_DELAY: Duration = Duration::from_millis(40);
    /// How long the fake client takes to see a commit; a removal sent sooner is stale.
    const COMMIT_SEEN_AFTER: Duration = Duration::from_millis(3);

    const TEST_TIMING: HideTiming = HideTiming {
        removal_spacing: Duration::from_millis(10),
        budget: Duration::from_millis(150),
    };

    /// A Hub and the viewer's view of it: a removal is committed unless it was sent before the
    /// previous commit could be seen, as the Hub refuses a stale `PeerChange`; a bake follows
    /// the last change after [`BAKE_DELAY`]. Every call is recorded with its time.
    struct FakeHub {
        started: Instant,
        listed: RefCell<Vec<PeerId>>,
        reports: RefCell<UsersReport>,
        last_commit: RefCell<Option<Instant>>,
        last_change: RefCell<Option<Instant>>,
        bakes: bool,
        refuse_first_removal_of: Option<PeerId>,
        stop_using_fails: bool,
        calls: RefCell<Vec<(String, Duration)>>,
    }

    impl FakeHub {
        fn listing(peers: &[PeerId]) -> Self {
            Self {
                started: Instant::now(),
                listed: RefCell::new(peers.to_vec()),
                reports: RefCell::new(UsersReport {
                    peers: Some(peers.to_vec()),
                    count: 1,
                }),
                last_commit: RefCell::new(None),
                last_change: RefCell::new(None),
                bakes: true,
                refuse_first_removal_of: None,
                stop_using_fails: false,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn never_reported() -> Self {
            let hub = Self::listing(&[]);
            *hub.reports.borrow_mut() = UsersReport::default();
            hub
        }

        fn calls(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .map(|(call, _)| call.clone())
                .collect()
        }

        fn times_of(&self, call: &str) -> Vec<Duration> {
            self.calls
                .borrow()
                .iter()
                .filter(|(recorded, _)| recorded.starts_with(call))
                .map(|(_, at)| *at)
                .collect()
        }
    }

    impl VolumeUsers for FakeHub {
        fn reported(&self) -> UsersReport {
            self.reports.borrow().clone()
        }

        fn wait_for_report_after(&self, seen_count: u64, deadline: Instant) -> Option<UsersReport> {
            let bake_at = (*self.last_change.borrow())
                .filter(|_| self.bakes)
                .map(|changed| changed + BAKE_DELAY);
            let Some(bake_at) = bake_at.filter(|bake_at| *bake_at <= deadline) else {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                return None;
            };
            std::thread::sleep(bake_at.saturating_duration_since(Instant::now()));
            let mut reports = self.reports.borrow_mut();
            assert!(reports.count >= seen_count);
            *reports = UsersReport {
                peers: Some(self.listed.borrow().clone()),
                count: reports.count + 1,
            };
            Some(reports.clone())
        }

        fn stop_using(&self, peer: PeerId) -> Result<(), String> {
            let call = format!("stop_using {}", peer.0[0]);
            let earlier_removals_of_peer = self.times_of(&call).len();
            self.calls.borrow_mut().push((call, self.started.elapsed()));
            if self.stop_using_fails {
                return Err("refused".to_owned());
            }
            let now = Instant::now();
            *self.last_change.borrow_mut() = Some(now);
            let stale = (*self.last_commit.borrow())
                .is_some_and(|committed| now < committed + COMMIT_SEEN_AFTER);
            let refused_once =
                self.refuse_first_removal_of == Some(peer) && earlier_removals_of_peer == 0;
            if !(stale || refused_once) {
                self.listed.borrow_mut().retain(|listed| *listed != peer);
                *self.last_commit.borrow_mut() = Some(now);
            }
            Ok(())
        }
    }

    fn peer(first_byte: u8) -> PeerId {
        let mut bytes = [0; 16];
        bytes[0] = first_byte;
        PeerId(bytes)
    }

    fn removed_peers(hide: &VolumeHide) -> Option<&[PeerId]> {
        match hide {
            VolumeHide::Hidden { removed_peers, .. } => Some(removed_peers),
            VolumeHide::Unfinished { .. } => None,
        }
    }

    #[test]
    fn every_reported_peer_is_removed_one_at_a_time_and_the_volume_stays_open() {
        let hub = FakeHub::listing(&[peer(7), peer(9), peer(11)]);

        let started = Instant::now();
        let hide = hide_from_every_peer(&hub, &[], TEST_TIMING);
        let took = started.elapsed();

        assert_eq!(
            removed_peers(&hide),
            Some([peer(7), peer(9), peer(11)].as_slice())
        );
        assert_eq!(
            hub.calls(),
            ["stop_using 7", "stop_using 9", "stop_using 11"]
        );
        assert!(hub.listed.borrow().is_empty());
        let sent = hub.times_of("stop_using");
        for pair in sent.windows(2) {
            assert!(
                pair[1].saturating_sub(pair[0]) >= TEST_TIMING.removal_spacing,
                "removals sent {sent:?}"
            );
        }
        assert!(
            took >= 2 * TEST_TIMING.removal_spacing + BAKE_DELAY && took < TEST_TIMING.budget,
            "the hide took {took:?}"
        );
    }

    #[test]
    fn a_peer_the_bake_still_lists_is_removed_again() {
        let hub = FakeHub {
            refuse_first_removal_of: Some(peer(9)),
            ..FakeHub::listing(&[peer(7), peer(9)])
        };

        let hide = hide_from_every_peer(&hub, &[], TEST_TIMING);

        assert_eq!(removed_peers(&hide), Some([peer(7), peer(9)].as_slice()));
        assert_eq!(
            hub.calls(),
            ["stop_using 7", "stop_using 9", "stop_using 9"]
        );
        let resent = hub.times_of("stop_using 9");
        assert!(
            resent[1].saturating_sub(resent[0]) >= BAKE_DELAY,
            "{resent:?}"
        );
        assert!(hub.listed.borrow().is_empty());
    }

    #[test]
    fn a_hide_the_hub_does_not_confirm_within_the_budget_is_left_unfinished_at_its_end() {
        let hub = FakeHub {
            bakes: false,
            ..FakeHub::listing(&[peer(3)])
        };

        let started = Instant::now();
        let hide = hide_from_every_peer(&hub, &[], TEST_TIMING);
        let took = started.elapsed();

        assert_eq!(
            hide,
            VolumeHide::Unfinished {
                peers: vec![peer(3)],
                reason: format!(
                    "the Hub did not confirm the removal of {} within 150 ms",
                    peer(3)
                ),
            }
        );
        assert_eq!(hub.calls(), ["stop_using 3"]);
        assert!(
            took >= TEST_TIMING.budget && took < TEST_TIMING.budget + Duration::from_millis(100),
            "the hide gave up {took:?} after it started"
        );
    }

    #[test]
    fn a_volume_no_peer_uses_is_left_open_without_a_call() {
        let hub = FakeHub::listing(&[]);

        let hide = hide_from_every_peer(&hub, &[peer(5)], TEST_TIMING);

        assert_eq!(removed_peers(&hide), Some([].as_slice()));
        assert!(hub.calls().is_empty(), "{:?}", hub.calls());
    }

    #[test]
    fn without_a_report_the_served_peers_are_removed() {
        let hub = FakeHub::never_reported();
        hub.listed.borrow_mut().extend([peer(4), peer(6)]);

        let hide = hide_from_every_peer(&hub, &[peer(4), peer(6), peer(4)], TEST_TIMING);

        assert_eq!(removed_peers(&hide), Some([peer(4), peer(6)].as_slice()));
        assert_eq!(hub.calls(), ["stop_using 4", "stop_using 6"]);
    }

    #[test]
    fn without_a_report_or_a_served_peer_the_volume_is_left_as_it_is_at_once() {
        let hub = FakeHub::never_reported();

        let started = Instant::now();
        let hide = hide_from_every_peer(&hub, &[], TEST_TIMING);

        assert_eq!(
            hide,
            VolumeHide::Unfinished {
                peers: Vec::new(),
                reason: "QAROS has not reported which peers use it".to_owned(),
            }
        );
        assert!(hub.calls().is_empty());
        assert!(started.elapsed() < Duration::from_millis(20));
    }

    #[test]
    fn a_volume_whose_peers_cannot_be_removed_is_left_as_it_is() {
        let hub = FakeHub {
            stop_using_fails: true,
            ..FakeHub::listing(&[peer(3), peer(8)])
        };

        let hide = hide_from_every_peer(&hub, &[], TEST_TIMING);

        assert_eq!(
            hide,
            VolumeHide::Unfinished {
                peers: vec![peer(3), peer(8)],
                reason: "refused".to_owned(),
            }
        );
        assert_eq!(hub.calls(), ["stop_using 3"]);
    }

    #[test]
    fn the_start_hides_the_peers_an_unfinished_stop_left() {
        let hub = FakeHub::listing(&[peer(2), peer(5)]);

        let hide = hide_left_over_at_start(&hub, "rerun-structure_from_motion");

        assert_eq!(removed_peers(&hide), Some([peer(2), peer(5)].as_slice()));
        assert_eq!(hub.calls(), ["stop_using 2", "stop_using 5"]);
        assert!(hub.listed.borrow().is_empty());
    }

    #[test]
    fn a_hide_of_four_peers_is_confirmed_by_the_hubs_bake_within_the_stop_budget() {
        let hub_bake_delay = Duration::from_millis(100);
        let last_removal_sent = 3 * STOP_HIDE_TIMING.removal_spacing;
        assert!(last_removal_sent + hub_bake_delay < STOP_HIDE_TIMING.budget);
    }

    #[test]
    fn the_stop_hide_budget_leaves_the_session_its_slowest_seen_destroy_before_the_watchdog() {
        let before_the_hide_on_2026_11_02 = Duration::from_millis(31);
        let slowest_session_and_library_destroy = Duration::from_millis(388);
        assert!(
            before_the_hide_on_2026_11_02
                + STOP_HIDE_TIMING.budget
                + slowest_session_and_library_destroy
                <= crate::STOP_DEADLINE
        );
    }
}
