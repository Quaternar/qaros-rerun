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

## Slow first sender on a fresh mixer

- **Symptom.** The first render sender a freshly started mixer accepted took about 21 s to create; once warm, a creation takes about 1 s (1.3 to 1.4 s for the first sender of a run). Without a cancel token, a creation for a listed target that was not receiving did not return for minutes.
- **What the viewer does.** Creations run in the background with a 10 s timeout, and a timeout in the first 30 s of a run is retried after 2 s ([ADR-0040](decisions/ADR-0040%20Render%20Sender%20Creation%20Timeout%20With%20Retry.md)).
- **Seen on.** Build 2916.
- **Status.** Open. The header does not say what `qar_render_sender_create` waits for or how long it may take ([HDR] L3884-3918, `qar_render_sender_create`).

## Panel listing fails while a panel shows a video stream

- **Symptom.** `gui_panels().list()` of the Rust binding fails whenever any panel in the session shows a video stream instead of a URI, and each failed listing blocked the viewer's UI thread for 3 to 4 s.
- **Cause.** The binding reads `qar_gui_panel_get_content_uri` for every listed panel ([QP] L333-337), which fails for a stream panel; the header has `qar_gui_panel_get_content_type` to check first ([HDR] L4561).
- **What the viewer does.** The viewer lists panels through its own raw listing, which reads `qar_gui_panel_get_content_type` before the URI or the stream id (`crates/qaros-sink/src/panel_listing.rs`). A listing that still fails opens no panel and is retried after 0.5 s, then every 30 s ([ADR-0073](decisions/ADR-0073%20Failed%20Panel%20Listings%20Retried%20After%20Half%20A%20Second%20Then%20Every%2030%20s.md)).
- **Seen on.** Build 3137, with the binding at `ebfe3bf`.
- **Status.** Worked around: the viewer reads the content type first. Open in the binding; the raw listing can go once `GuiPanels::list` reads the content type itself.

## A stream panel's mixer loses its Vulkan device on a replaced stream

- **Symptom.** When a stream panel connected the stream it had just been pointed at, a mixer failed with `VulkanDeviceLost` and closed the panel. It was intermittent, and was seen in the glasses' mixer and in a Display Receiver's mixer after the headset had left the session ([Quaternar#343]).
- **What the viewer does.** The panel stays closed until the next run claims it again by its common name ([ADR-0075](decisions/ADR-0075%20A%20Closed%20Panel%20Is%20Revived%20By%20Its%20Common%20Name.md)). Every failure seen came with NVIDIA driver Event 153 as a mixer connected a stream of 1016x665 or 960x256 px, so stream panel images are padded on the right and at the bottom to a width that is a multiple of 64 and a height that is a multiple of 8, and the log names the padded size (a view's fixed stream frame). A desktop resize no longer replaces a view's stream: each view streams one frame per run ([ADR-0087](decisions/ADR-0087%20Each%20Panel%20Streams%20One%20Fixed%20Frame%20Per%20Run.md)). The mixer's panel copy has no size-dependent arithmetic, so this is a test that rules out a size mismatch between the exported destination and its alias rather than an expected fix: `MIXER_WIDTH_ALIGNMENT` and `MIXER_HEIGHT_ALIGNMENT` in `crates/qaros-rerun/src/panel_capture.rs` set to 1 turn it off.
- **Seen on.** 0.9.0-dev.3177.
- **Status.** Open in QAROS.

## GUI panel queries block for about 2 s

- **Symptom.** On 0.9.0-dev.3177, `qar_query_gui_panels_count`, `qar_query_gui_panels` and a panel claim each block the calling thread for about 2 s ([Quaternar#345]), read from 2.0 s steps between the library's `guiPanel` log lines and 4.0 s gaps in the viewer's frame log. A panel listing every 5 s stopped the viewer's frames for 4 s in every 5, so the glasses' 3D stream stalled and its receiver was treated as lost, and a Stop during a listing overran the 600 ms watchdog.
- **What the viewer does.** It lists panels only to claim them, and one listing serves every claim for 30 s, so the views and the timeline that settle at the start share one, and a view that settles later lists again at most once in 30 s. The check for panels the mixer closed (see below) reads those listings and makes none of its own. Each listing is timed under the perf event `panel_listing`, and one over 100 ms is logged. Claims still block the frames while they run, because QAROS handles stay on the thread that created them ([ADR-0017](decisions/ADR-0017%20QAROS%20Sink%20Owned%20On%20The%20UI%20Thread.md)). Hiding and showing the panels of views that leave and return to the layout makes no listing either, so a panel a user minimised comes back visible with its view ([ADR-0082](decisions/ADR-0082%20Held%20Panels%20Follow%20The%20Viewer%20Layout.md)).
- **Seen on.** 0.9.0-dev.3177.
- **Status.** Open in QAROS. `qar_gui_panels_subscribe_updates` would report closed panels without a listing.

## A panel failure in one viewer's mixer closes the panel for everyone

- **Symptom.** A GUI panel that fails in one viewer's mixer, such as a Display Receiver's, is closed for every viewer in the session, the headsets included. The panel list then shows it closed, exactly as if a user had closed it in the Hub Display ([Quaternar#344]).
- **What the viewer does.** It cannot tell such a close from a user's, so it does not reopen the panel during a run; the next run revives it by its common name ([ADR-0075](decisions/ADR-0075%20A%20Closed%20Panel%20Is%20Revived%20By%20Its%20Common%20Name.md)). With a Display Receiver connected, panels are more exposed to this through the issue above.
- **Seen on.** 0.9.0-dev.3177.
- **Status.** Open in QAROS.

## A panel's show right after its new stream is refused as stale

- **Symptom.** After a Stop and a Start in the same Hub session, the timeline panel came back, but the two view panels stayed out of sight until the owner showed them in the Hub, although the viewer logged each one "shown again after the earlier run hid it" and streamed its first frame.
- **Cause.** A claim sends two sparse proposals back to back: `set_stream` (field `Content`), then `set_state(VISIBLE)` (field `State`), both on the same base revision. The authority commits the first and publishes a snapshot with it, and that snapshot stamps every field at the new revision (`PublishAcceptedSnapshot` calls `MarkAllFieldsAtRevision`, `GuiPanelProcessor.hpp` L975-985 at [QS]). The show then fails the per-field check (`SharedObjectArbitration.hpp` L815-830 at [QS]), against the stated intent that proposals on disjoint fields both succeed (L777-787). The Hub's log shows it for every claimed panel of the run, the timeline's included: `refused guiPanel proposal ... field 'State' no longer matches` within 1 ms of the `Content` commit, then a resend 100 to 200 ms later with no further refusal. Whether that resend lands is logged only at trace level, so the log cannot say why the view panels stayed out of sight while the timeline did not.
- **What the viewer does.** A panel the claim showed is shown once more with its first frame, which the mixer can show only after it took the claim's stream, so that show follows the stream's commit instead of racing it (`show_again_with_its_first_frame` in `crates/qaros-sink/src/stream_panels.rs`). A minimised panel is not shown, nor one whose view left the layout before its first frame.
- **Seen on.** 0.9.0-dev.3184, Hub session log of 2026-11-02, three claims at 13:52:17, 13:52:56 and 13:52:58 local time, and the next run's at 13:59:51 and 13:59:55.
- **Status.** Open, to raise with QuaternAR. The second show can go once a snapshot no longer stamps the fields its commit did not touch.

## A two-hand spread ends before it scales the scene

- **Symptom.** With hand tracking on METALENSE 2, a one-hand grab moves the scene smoothly, but a two-hand spread barely scales it. QAROS starts the spread and ends it 50 to 80 ms later; the one-hand grab then passes from one hand to the other every 60 to 100 ms while both hands pinch. In a two-minute run the app scale changed once, by about 3 %.
- **What the viewer does.** Nothing to work around: QAROS's default rules apply both gestures, and the viewer only follows the volume's live transform. Scaling stays with `--scale` at start.
- **Seen on.** 0.9.0-dev.3184, METALENSE 2.
- **Status.** Under investigation in the QAROS gesture detector.

## An infinite far plane is not in the header

- **Symptom.** None seen. The header says only that near > far declares reverse-Z ([HDR] `QarNearFar`); that a reverse-Z `near_plane` above 1e20 declares an infinite far plane is in qar-streaming's source alone (`IsInfiniteFar`, `PixelShaderHelper.hpp` L397-404 at `e487a31`).
- **What the viewer does.** By default it sends Rerun's infinite reverse-Z depth as it is and declares it with `near_plane` 1e30 ([ADR-0084](decisions/ADR-0084%20Rerun%20Reverse-Z%20Depth%20Declared%20With%20An%20Infinite%20Far%20Plane.md)). `--depth forward` and `--depth reverse` convert to a finite far plane instead, should a QAROS build read the declaration differently.
- **Seen on.** qar-streaming source at `e487a31`.
- **Status.** Open, to raise with QuaternAR: document the infinite far plane in `qar_streaming.h`. The finite-far encodings can go once the header carries it and a headset run confirms the default.

## History: why panels need 0.9.0-dev.3177

Mixers up to build 3137 gave every GUI panel a browser with its own Ultralight renderer, stream panels included, while Ultralight allows one per process. Closing or reviving a panel next to live ones crashed the mixer in `WebCore.dll`, and a stream panel is closed by the mixer once its sender is gone, so restarting an app with stream panels crashed it. 0.9.0-dev.3177 shares one renderer across the panels, and restarts there are clean. That build is the viewer's minimum, and the viewer logs a warning on an older runtime.

## Sources

- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h`.
- [GP] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/gui-panels.mdx`.
- [CL] QAROS `CHANGELOG.md` at `ebfe3bf`, 0.9.0.
- [QP] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/crates/qaros/src/panels.rs` (`GuiPanels::list`).
- [QS] qar-streaming source at `e487a311`, `libs/qar-streaming/src/`.
- [Quaternar#343]: Mixer: VulkanDeviceLost when a GUI stream panel connects its replaced stream (3177).
- [Quaternar#344]: Mixer: a GUI panel failure in one viewer's mixer closes the panel for every viewer.
- [Quaternar#345]: SDK: GUI panel queries block the calling thread for about 2 s each (3177).

[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[GP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/gui-panels.mdx "GUI Panels"
[CL]: https://github.com/Quaternar/qaros/blob/ebfe3bf/CHANGELOG.md "QAROS changelog"
[QP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/crates/qaros/src/panels.rs#L333-L337 "qaros GUI panels"
[Quaternar#343]: https://github.com/Quaternar/Quaternar/issues/343 "Mixer: VulkanDeviceLost when a GUI stream panel connects its replaced stream"
[Quaternar#344]: https://github.com/Quaternar/Quaternar/issues/344 "A GUI panel failure in one viewer's mixer closes the panel for every viewer"
[Quaternar#345]: https://github.com/Quaternar/Quaternar/issues/345
