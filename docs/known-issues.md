---
title: Known issues
description: QAROS runtime behaviour the viewer works around, with the build it was seen on and its status.
sidebar_position: 9
---

# Known issues

QAROS behaviour that shapes code in this repository: what was seen, why it happens where the cause is known, what the viewer does about it, the QAROS build it was seen on, and where it stands. "Fixed upstream, not yet in a build" means the fix is in QAROS's source but no installable build carrying it has been run against this viewer. Each entry names the code that exists because of it, so the workaround can go once the minimum supported QAROS build carries the fix.

## Ended target apps listed as running

- **Symptom.** The Hub listed target apps that had ended as running for days. Every sender creation for such a peer ran into its 10 s timeout, at every start of the viewer.
- **Cause.** `QAR_APP_STATE_RUNNING` meant only that the peer had joined, and `QarPeerSpec` carried no liveness signal.
- **What the viewer does.** Senders are created in the background for several candidates at once, requesters first, and a target whose creation timed out is held back for 10 minutes, also across runs, in `qaros-rerun.unanswered-peers.txt` ([ADR-0040](decisions/ADR-0040%20Render%20Sender%20Creation%20Timeout%20With%20Retry.md)). Since the header deprecates the app state for the peer presence, the app state the viewer reads is computed from the presence, so an offline peer reads as shutting down and leaves ([ADR-0036](decisions/ADR-0036%20Target%20Discovery%20From%20The%20Peer%20List.md)).
- **Seen on.** Build 2916.
- **Status.** Fixed from build 3137: peer presence (`qar_peer_spec_get_presence`) tells a gone target apart ([HDR] L552-579). The cross-run memory of unanswered targets can go once the minimum supported build is 3137 or later; it then covers only peers that are online and do not answer.

## Session teardown crash with a begin-frame wait pending

- **Symptom.** Stopping the viewer crashed it in `qar_streaming.dll` at `+0x27A005` in 5 of 8 Stops.
- **Cause.** A render sender destroyed while a `try_begin_frame` wait was pending outlived its handle, because the pending wait held the last reference to it. The session's destructor then destroyed the app volumes while the sender still read their poses.
- **What the viewer does.** A sender goes only once its begin-frame wait has ended: a released user's sender is polled until then, and at Stop the senders with a running wait are polled for at most 500 ms (`BEGIN_WAIT_DRAIN_LIMIT` in `crates/qaros-sink/src/begin_drain.rs`, [ADR-0039](decisions/ADR-0039%20Polled%20try_begin_frame%20By%20Default.md)).
- **Seen on.** Build 2916.
- **Status.** Fixed upstream, not yet in a build: a sender or receiver that outlives its session no longer crashes the application after Stop. Keep the drain until a run on a build with the fix shows clean Stops.

## Cancelled async calls did not call back, and Stops hung in teardown

- **Symptom.** A cancelled `qar_render_sender_begin_frame_async` never called back (observed for 30 s), so waiting for it hung the viewer at exit. A cancelled `qar_render_sender_create_async` dropped its callback the same way. Three of seven Stops from the Hub hung in the QAROS teardown.
- **What the viewer does.** Frames are begun only by polling `try_begin_frame` ([ADR-0039](decisions/ADR-0039%20Polled%20try_begin_frame%20By%20Default.md)), a sender creation is never cancelled to make room for another ([ADR-0040](decisions/ADR-0040%20Render%20Sender%20Creation%20Timeout%20With%20Retry.md)), and every stop request arms a watchdog that ends the process 600 ms later if the teardown has not ([ADR-0035](decisions/ADR-0035%20Launched%20By%20QAROS%20Contract.md)).
- **Seen on.** Build 2916.
- **Status.** Fixed upstream, not yet in a build: cancelling a pending begin-frame or destroying its sender or session ends the call with `QAR_STATUS_TIMEOUT`, and the async callback fires once before the destroy returns. Whether the teardown hang had the same cause is not confirmed.

## Pure virtual call when the process exits through ExitProcess

- **Symptom.** A process ending through `ExitProcess` with `qar-streaming-c.dll` still mapped raised a pure virtual call in that library (`+0x17B5F5`). Windows Error Reporting then held the crashed process suspended for about 3 s, longer than QAROS waits before it kills the app.
- **What the viewer does.** Every Windows exit of a viewer with `--sink qaros` ends with `TerminateProcess` after the QAROS teardown and a bounded flush, so no library unload code runs ([ADR-0035](decisions/ADR-0035%20Launched%20By%20QAROS%20Contract.md), `crates/qaros-sink/src/process_exit.rs`). The QAROS library stays mapped for the life of the process.
- **Seen on.** Build 2916.
- **Status.** Unknown. No QAROS change names an exit or unload crash; the 0.9.0 fix for a pure virtual call concerns cancelling a subscription during delivery, a different trigger ([CL] 0.9.0).

## Mixer crash when panels close or navigate

- **Symptom.** The mixer crashed in `WebCore.dll` (Ultralight) 0.35 to 1 s after the viewer closed GUI panels or set new URLs on reused ones, and streams came back only after a Hub restart. The same crash showed again when a demo stopped with its panels hidden.
- **Cause.** Ultralight allows one renderer per process; the mixer created one per website panel, and creating or destroying one beside a live one tore down shared WebCore state.
- **What the viewer does.** Panels are never closed: they are hidden at Stop and claimed again by common name, and a panel is navigated only when its URL differs ([ADR-0054](decisions/ADR-0054%20Panels%20Claimed%20By%20Common%20Name%20With%20Run%20Stable%20View%20Names.md)). `--panels-at-stop keep` makes no panel call at Stop, and `--dead-panels leave` makes no navigation, so either call can be ruled out as a trigger.
- **Seen on.** Builds 2916 and 3137.
- **Status.** Fixed upstream, not yet in a build: QuaternAR commit `ff91001b` shares one Ultralight renderer across website panels, so an app that shows a website panel can restart and create its panels again without crashing the mixer. Hiding at Stop stays for its own reason: a user's panel placement survives the restart.

## White panel after a Hub restart

- **Symptom.** A persistent panel whose document failed to load stays white, runs no script, and recovers only by a navigation. It happens when the mixer loads the panel's URL while no viewer runs (a Hub restart restoring the panel), and after a headset's identity changes.
- **What the viewer does.** A reattached panel whose page asks the panel server for nothing 10 s after the browser is seen running is navigated once to the URL it already shows ([ADR-0054](decisions/ADR-0054%20Panels%20Claimed%20By%20Common%20Name%20With%20Run%20Stable%20View%20Names.md)). A Hub "New session" also clears it.
- **Seen on.** Builds 2916 and 3137.
- **Status.** Open. The QAROS docs do not say what a panel does when its page fails to load ([GP] "Manipulating panels").

## Slow first sender on a fresh mixer

- **Symptom.** The first render sender a freshly started mixer accepted took about 21 s to create; once warm, a creation takes about 1 s (1.3 to 1.4 s for the first sender of a run). Without a cancel token, a creation for a listed target that was not receiving did not return for minutes.
- **What the viewer does.** Creations run in the background with a 10 s timeout, and a timeout in the first 30 s of a run is retried after 2 s ([ADR-0040](decisions/ADR-0040%20Render%20Sender%20Creation%20Timeout%20With%20Retry.md)).
- **Seen on.** Build 2916.
- **Status.** Open. The header does not say what `qar_render_sender_create` waits for or how long it may take ([HDR] L3884-3918, `qar_render_sender_create`).

## Panel listing fails while a panel shows a video stream

- **Symptom.** `gui_panels().list()` of the Rust binding fails whenever any panel in the session shows a video stream instead of a URI, and each failed listing blocked the viewer's UI thread for 3 to 4 s.
- **Cause.** The binding reads `qar_gui_panel_get_content_uri` for every listed panel ([QP] L333-337), which fails for a stream panel; the header has `qar_gui_panel_get_content_type` to check first ([HDR] L4561).
- **What the viewer does.** A failed listing opens no panel and is retried after 0.5 s, then every 30 s ([ADR-0073](decisions/ADR-0073%20Failed%20Panel%20Listings%20Retried%20After%20Half%20A%20Second%20Then%20Every%2030%20s.md)).
- **Seen on.** Build 3137, with the binding at `ebfe3bf`.
- **Status.** Open in the binding. Relax the retry once the binding checks the content type first.

## Sources

- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h`.
- [GP] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/gui-panels.mdx`.
- [CL] QAROS `CHANGELOG.md` at `ebfe3bf`, 0.9.0.
- [QP] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/crates/qaros/src/panels.rs` (`GuiPanels::list`).

[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[GP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/gui-panels.mdx "GUI Panels"
[CL]: https://github.com/Quaternar/qaros/blob/ebfe3bf/CHANGELOG.md "QAROS changelog"
[QP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/crates/qaros/src/panels.rs#L333-L337 "qaros GUI panels"
