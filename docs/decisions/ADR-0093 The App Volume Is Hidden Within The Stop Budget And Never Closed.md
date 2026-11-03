---
id: ADR-0093
title: The app volume is hidden within the stop budget, from used-by peers the viewer follows during the run, one removal at a time, and is never closed
status: proposed
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0093: The app volume is hidden within the stop budget, from used-by peers the viewer follows during the run, one removal at a time, and is never closed

> Amends ADR-0089 (how the used-by peers are read and removed at Stop; the volume is never closed; a hide left unfinished is finished by the next start).

## Context

ADR-0089 hides the app volume at Stop by reading its used-by peers (`qar_query_app_volumes`, `qar_app_volume_get_used_by_peers`) and removing them all in one `qar_app_volumes_update_used_by_peers`, and closes the volume when that fails. Seen on METALENSE 2 with QAROS 0.9.0-dev.3184 on 2026-11-02 (viewer logs of the `structure_from_motion` demo, Hub log of session `c19e8322`; viewer times UTC, Hub times local, UTC+1):

- **The read blocks for about 4 s.** At 16:16:36 the Stop of `viewer-20261102T161145Z-3172.log` (a window close, so no watchdog) spent 4000 ms in the hide step. Each of `qar_query_app_volumes_count` and `qar_query_app_volumes` runs `SharedObjectClient::Query`, which sends the mutation and snapshot reconciliation queries and always waits out their 2 s reply window ([QS] `qar-streaming/src/SharedObjectClient.hpp` `Query`, `SharedObjectTypes.hpp` `kSharedObjectReconcileQueryTimeoutMs` = 2000). The Hub log shows the queries (`storage/.../appVolume/mutation/*/*/*`, `requestTimeoutMs=1999`).
- **A Ctrl+C Stop ran into the watchdog.** At 16:56:51.718 the Stop of `viewer-20261102T163233Z-24104.log` had the senders gone after 17 ms, the panels hidden and the panel streams closed by 30 ms; the hide step then queried (Hub, 17:56:51.749 and its resend at .850) and was still waiting at 600 ms, when the watchdog ended the process (`still running 600 ms after the stop request ..., in teardown step hide volume`). No removal and no close reached the Hub: it logged no `AppVolumeProcessor` proposal after the Stop. The owner saw the box and its last content stay in the glasses, and the Hub GUI still listed the headset for the volume. The runs at 16:11:36, 17:05:35 and 17:06:58 UTC ended the same way.
- **Only one removal of several lands.** The 16:16:40 Stop, given the 4 s, removed four peers in one call. QAROS turns each removal into a proposal of its own on the `PeerChange` field ([QS] `qar-streaming-c/src/v0/app_volumes.cpp` `qar_impl_app_volumes_update_used_by_peers`, `qar-streaming/src/AppVolumeImpl.cpp` `RemovePeer`), all on one base. The Hub accepted the first (`peers=4` to `peers=3`, 17:16:40.708) and refused the other three as stale (`Rejected stale shared-object proposal: field 'PeerChange' no longer matches`, [QS] `SharedObjectArbitration.hpp` `CreateNextVersionForFields`). Each newer proposal had dropped the older ones from the outbox, which supersedes a pending proposal whose fields a newer one covers ([QS] `SharedObjectOutbox.hpp` `Submit`, `Supersedes`), and no resend was accepted before the session went 227 ms later. The volume kept three peers.
- **What keeps the frozen content.** The headset's mixer keeps a table and a mixer state, with the last frame it received, for every volume that lists its peer and is not closed, and drops both only when the peer leaves the list or the volume closes (QuaternAR `StereoKitGUI/src/Table/TableControl.hpp` `UpdateTablesFromAppVolumes`, at `4aeffc6`). Destroying the render sender does not clear it: removing the peer, or closing the volume, is the only way.
- **What is cheap.** The volume's update subscription (`qar_app_volumes_subscribe_updates`) replays the current state and then delivers each of the Hub's bakes of the volume, with its used-by peers, without a round trip; the Hub bakes a changed volume 100 ms after its last change ([QS] `AppVolumeProcessor.hpp` `BakeDelay`, `BakeMaxDelay` 250 ms). A removal is published before `qar_app_volumes_update_used_by_peers` returns ([QS] `SharedObjectOutbox.hpp` `Submit`, `PublishOnce`), and the Hub on the same machine accepted one 0.1 ms after it was sent.
- **The budget.** The watchdog ends the process 600 ms after a stop request (ADR-0079, `STOP_DEADLINE`), because QAROS kills an app about 1 s after Stop. On 2026-11-02 the steps before the hide took 5 to 77 ms, and the session and library destroy after it 135 to 388 ms.
- **The owner's rule.** The viewer never closes its app volume, at Stop or on any failure. It only hides it, and the next start finds it with `get_or_create`.

## Decision

- **The used-by peers are followed during the run.** A raw subscription on the volume's updates copies the used-by peers of each update into a record shared with the QAROS thread (`volume_users.rs`, `UsedByWatch`, `ReportedUsers`). The listing the connect already makes, to warn about a volume handed back closed, now reads the used-by peers too and seeds the record; the subscription keeps it current. Its cancel token ends with the session, as the binding's subscriptions do.
- **Stop asks QAROS nothing.** The hide takes the peers last reported; when none was ever reported, it takes the peers this run's senders streamed to. A volume reported with no peer gets no call.
- **One removal at a time.** Each peer is removed in a call of its own, 15 ms after the previous one, so each removal goes on the base the previous commit left and none supersedes another.
- **Confirmed by the Hub's bake.** The hide waits for the next report of the volume. A peer it still lists is removed again; the hide is done once a report lists none of the removed peers. The log names the peers and the time: `qaros: hid app volume "rerun-structure_from_motion" from every peer that used it (...) in N ms, as the Hub's bake of it confirms; ...`.
- **Within 180 ms, then left as it is.** A hide not confirmed 180 ms after it started, or whose removal QAROS refuses, leaves the volume open as it is and logs a warning that names the peers and the reason: `qaros: app volume "..." may still show in ... (the Hub did not confirm the removal of ... within 180 ms); it stays open as it is, and the next start hides it from them before its senders list their headsets again`. Four peers fit: the last removal goes at 45 ms and the bake follows at about 145 ms. The 16:56:51 Stop reached the hide at 31 ms; with the slowest session and library destroy seen (388 ms), 31 + 180 + 388 ms stays inside the 600 ms deadline. A Stop that is slow both before and after the hide (77 ms and 388 ms) can still be ended by the watchdog, then in the session destroy, with the hide done.
- **Never closed.** No path of the viewer calls `qar_app_volumes_close_volume`. The teardown step is named `hide volume`.
- **The next start finishes an unfinished hide.** A volume taken over at connect that still lists peers, before any sender of the run exists, is hidden from them the same way, with a 1 s budget, and the log says so: `qaros: app volume "..." still lists ... from an earlier run; hiding it from them before this run's senders list their headsets`. The run's senders then list their headsets again (ADR-0089).
- **The last place is stored after the hide**, not before it, so the hide starts as early as the Stop allows (ADR-0085).

## Alternatives considered

- **Read the peers at Stop with a shorter wait.** The listing's 2 s window is QAROS's constant, not a parameter of the C API, and both list calls pay it. Rejected.
- **Remove all peers in one call, as before.** Only the first proposal lands, as the 16:16:40 Stop shows. Rejected.
- **Send the removals without waiting for a bake.** Fits easily, but a stale or lost removal is then never noticed, and the box stays in that headset until the next start. The bake costs about 100 ms and confirms what the Hub holds; kept within the budget.
- **Close the volume when the hide is not confirmed.** The mixer drops a closed volume too, but the owner's rule is that the viewer never closes its volume: a closed volume loses its place in the Hub and is rebuilt from the init at the next start. Rejected; the next start finishes the hide instead.
- **Raise the watchdog's deadline.** QAROS kills the app about 1 s after Stop, which would cut the log tail and the exit line again (ADR-0079). Rejected.

## Consequences

- Positive: a Ctrl+C Stop hides the volume in about 100 to 150 ms instead of running into the watchdog, and the teardown keeps its exit line.
- Positive: every peer is removed, not only the first; peers left by earlier Stops are removed by the next start.
- Positive: unit tests drive the hide against a fake Hub that refuses a removal sent before the previous commit could be seen and bakes 40 ms after the last change: removals one at a time and spaced, a refused removal sent again after the bake, a hide given up at the end of its budget, no call for a volume no peer uses, the served peers removed when nothing was reported, the start's hide of left-over peers, and the budget arithmetic against the stop deadline ([VV] tests). Stub tests assert that Stop lists nothing and never closes the volume, that the removal comes after the senders and before the session, and that a hide the Hub never confirms ends within its budget and the teardown inside the stop deadline ([ST]).
- Negative: a volume whose hide was not confirmed may stay in a headset, with its last content, until the viewer starts again under the same `--volume-name`.
- Negative: a start that takes over a volume listing peers hides it from them, including a headset whose user chose "Unhide Volume for Me" while the viewer was stopped, or one served by another viewer under the same volume name; the senders of this run list their own headsets again.
- Open: whether the 15 ms spacing holds when the Hub runs on another machine than the viewer. A refused removal is then sent again after the bake, which may push a hide of several peers past the budget; the next start finishes it.
- Open: the owner's check in the glasses: after a Ctrl+C Stop, the box and content disappear from the glasses and the Hub GUI shows no peer for the volume; the viewer log has the `hid app volume` line and no watchdog line.

## Evidence and sources

- [VU] `crates/qaros-sink/src/volume_users.rs` (`ReportedUsers`, `UsedByWatch`, `list_volume`, tests).
- [VV] `crates/qaros-sink/src/volume_visibility.rs` (`hide_at_stop`, `hide_left_over_at_start`, `hide_from_every_peer`, `STOP_HIDE_TIMING`, tests).
- [CN] `crates/qaros-sink/src/connection.rs` (`Drop for QarosConnection`, `list_and_warn_if_closed`, `seed_reported_users`, `watch_used_by`).
- [ST] `crates/qaros-sink/tests/stub.rs` (`a_hide_the_hub_never_confirms_ends_within_its_budget_and_leaves_the_volume_open`, `a_volume_whose_peers_were_never_reported_is_left_open_at_stop_without_a_listing`, `a_volume_no_peer_uses_stays_open_at_stop`, `a_sink_holding_the_last_connection_hides_the_volume_after_its_senders`).
- [QS] QAROS source, `Quaternar/qar_streaming` at `e487a31`, `libs/`: `qar-streaming/src/SharedObjectClient.hpp` (`Query`, `ReceiveUpdates`, `OverlayLocalReads`), `SharedObjectOutbox.hpp` (`Submit`, `Supersedes`), `SharedObjectArbitration.hpp` (`CreateNextVersionForFields`), `SharedObjectTypes.hpp` (L27), `AppVolumeImpl.cpp` (`RemovePeer`, `SerializeAndPublish`), `AppVolumeProcessor.hpp` (L78-79); `qar-streaming-c/src/v0/app_volumes.cpp` (`qar_impl_app_volumes_update_used_by_peers`, `qar_impl_app_volumes_subscribe_updates`).
- Viewer logs on the Legion machine, `demos/.cache/structure_from_motion/logs/`: `viewer-20261102T160522Z-6852.log`, `viewer-20261102T161145Z-3172.log`, `viewer-20261102T163233Z-24104.log`, `viewer-20261102T170322Z-26784.log`, `viewer-20261102T170551Z-10672.log`; Hub logs of session `c19e8322` (`launcher_stdout.log` and `previous-runs/20261102T162858Z/launcher_stdout.log`).

[VU]: ../../crates/qaros-sink/src/volume_users.rs "App volume used-by peers"
[VV]: ../../crates/qaros-sink/src/volume_visibility.rs "App volume visibility"
[CN]: ../../crates/qaros-sink/src/connection.rs "QAROS connection"
[ST]: ../../crates/qaros-sink/tests/stub.rs "Stub tests"
[QS]: https://github.com/Quaternar/qar_streaming/tree/e487a31158caca9a9cd2fcf6073456b316d8b7c6/libs "QAROS streaming library source (private)"
