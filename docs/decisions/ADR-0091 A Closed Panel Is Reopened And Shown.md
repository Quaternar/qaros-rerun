---
id: ADR-0091
title: A panel closed while held is reopened once a target's head streams, and a panel listed Closed at its claim is created afresh and shown
status: proposed
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0091: A panel closed while held is reopened once a target's head streams, and a panel listed Closed at its claim is created afresh and shown

> Amends ADR-0090 (a panel closed while held is reopened within the run instead of staying closed) and ADR-0075 (a panel listed `Closed` at its claim is shown, at its held place when that place is of use).

## Context

Seen on METALENSE 2 with QAROS 0.9.0-dev.3184 on 2026-11-02, build `combined-1102c` (viewer logs `viewer-20261102T170322Z-26784.log`, `viewer-20261102T170551Z-10672.log` and `viewer-20261102T170703Z-6616.log` of the `structure_from_motion` demo, times UTC; Hub log of session `c19e8322` and the log of mixer `9eac9097`, local time, UTC+1):

- **The mixer closes panels, not only the Hub Display.** Every `validating gui update proposal` line in the Hub log names its proposer in `Update.WhoUpdated`. At 18:01:51 local time, five minutes after the first run's Stop hid its panels (`State: 2` from the viewer `1b9f11e0` at 17:56:51), mixer `9eac9097` (an Internal peer) proposed `State: 3` for all seven panels in the session; its log has `GUI panel ... failed during incoming video destination and will be closed without stopping the mixer: ... MediaStreamingStreamIsClosed` for each, the streams of the viewer that had stopped. At 18:06:21 the same mixer lost its Vulkan device (`vkQueueSubmit lost the Vulkan device. Every stream on it is gone`, then `GuiPanelCannotCreate` and `VulkanDeviceLost` per panel) and proposed `State: 3` for the timeline, reprojection and camera panels while the viewer was streaming to them. The headset left 20 ms later (`device=Metalens2 presence=Leaving`), the mixer went offline at 18:06:27 and came back at 18:06:30, and the headset streamed again from 18:06:31 (user-5). The viewer had released all three panels as closed in the Hub Display and left them closed for the run. This is Quaternar#344: one mixer's failure closes a panel for every viewer.
- **QAROS reports no proposer to an app.** A panel update handle offers the id, display name, pose, size, content, state and visible-to peers (`qar_gui_panel_get_*`, [HDR] L4859-4900); `WhoUpdated` reaches only the Hub's log. Timing does not tell either: the 18:06:21 close came before the headset left, the 18:01:51 one with no run at all, and a mixer failure can close a panel while every headset streams on (#344 with a Display Receiver).
- **A panel listed Closed at its claim was never shown.** The runs at 17:03:22Z and 17:07:03Z found all three panels `Closed` (`reattached (listed as "", Closed, ...)`). `get_or_create` builds a closed panel afresh from its init ([HDR] L4500-4515): the Hub log has a create proposal with every field, `State: 0`, at the config's pose, for each (18:03:56 to 18:04:03 and 18:07:16 to 18:07:23). The claim treated the panel as reattached and, since it was not `Hidden`, sent no show and none with the first frame (`sized 0.450x0.204 m again with its first frame`, without `shown`). Every other claim, a new panel's and a hidden one's, sends the show after the stream is set and again with the first frame (ADR-0090). The owner saw no panels in either run; the mixer did connect the streams and logged hovers on the camera and timeline panels at 18:04:17 to 18:04:58, so whether the panels were drawn where the owner looked is not settled by these logs. `--reset-panels` claims a panel it closed through the same path.

## Decision

- **Nothing in the viewer's life closes a panel.** The viewer hides panels, at Stop and when their view leaves the layout, and every start finds them again by common name. Only `--reset-panels` closes them (ADR-0090).
- **A panel closed while held is reopened.** A close the state watch sees, or a listing that shows a held panel `Closed` or misses it twice, releases the panel as before: no further stream, no hide at Stop. The panel then waits for a target's head that streams after the close was seen, and at the next sync its claim is forgotten and made again with a new listing, which says whether it is still closed and where it is held. A headset that left with the close sees its panels again when it streams again; one that streamed on through the close sees them within a sync. A panel is reopened at most three times a run; a fourth close stands until the next run.
- **A close in the Hub Display is undone too.** The viewer cannot tell it from a mixer's, and keeping a headset's panels is worth more than honouring a close that a mixer may have made. Each release logs that QAROS reports no proposer, and each reopen its count.
- **A panel listed Closed at its claim is created afresh and shown.** `get_or_create` builds it from the config; the claim shows it after the stream is set and again with the first frame, as a new panel. It goes back to its held place with `set_pose` when that place is of use by ADR-0090's rule (centre not nearer than 0.2 m to the placing eyes, at this run's size, and with `--move-far-panels` within 1.5 m of them, ADR-0095), sent again with the first frame; otherwise it stays where new panels open, and a view takes the next slot. A panel `--reset-panels` closed opens where new panels open.

## Alternatives considered

- **Tell the Hub Display's close by its proposer.** The Hub knows it (`WhoUpdated`, the Display's peer `06:46:8F...` with `device=VisualizerControl`), but the C API exposes no proposer on a panel update or listing. Worth asking QAROS for; the viewer would then keep a Display close and reopen every other.
- **Reopen only when a target peer left around the close.** The 18:06:21 close came 20 ms before the headset left, the 18:01:51 one between runs, and #344 closes panels while headsets stay. Any window would miss real mixer closes.
- **Reopen at once, without waiting for a head.** A mixer that just failed, or is restarting, would take the panel again before it can show it; a head streaming after the close shows a receiver is back.
- **Keep the sender and only show the panel again.** The header documents `get_or_create` as what builds a closed panel again ([HDR] L4500-4515); what `set_state` does on a closed panel is not documented or tried. A claim builds the panel and a new sender through the path every start takes.

## Consequences

- Positive: a headset that drops and comes back, or a mixer that fails and restarts, gets its panels back within the run, at the place they had.
- Positive: a run that finds its panels closed shows them, at their held place when that place is of use.
- Positive: unit tests assert the calls: a panel listed `Closed` is created, put back at its held place and shown, then sized, moved and shown again with its first frame; one whose held place is of no use opens at the config's pose; a panel closed as its headset left is not reopened before a head streams after the close, then claimed again with get_or_create, set_stream, set_size, set_pose and the show, and hidden at Stop; a close while a head streams on is reopened at the next head; a fourth close in a run stands.
- Negative: a close in the Hub Display while the viewer runs lasts until the next head streams, at most a sync; to remove a panel, the viewer has to stop.
- Negative: a reopen makes a new listing, which blocks the QAROS thread for about 4 s on 0.9.0-dev.3184 (Quaternar#345), right after a headset came back.
- Open: not yet tried in the glasses. The owner's check: start a demo, take the headset off until it leaves the session and put it back on, and see the panels again; stop the viewer, wait five minutes, start again, and see the panels at the place they had.

## Evidence and sources

- [SP] `crates/qaros-sink/src/stream_panels.rs` (`claim`, `show_after_claim`, `restore_held_place`, `release_closed`, `panels_to_reopen`, `REOPEN_LIMIT`; tests `a_panel_listed_closed_is_created_afresh_back_at_its_held_place_and_shown_twice`, `a_panel_listed_closed_whose_held_place_is_of_no_use_opens_where_new_panels_open`, `a_panel_closed_as_its_headset_left_is_reopened_once_a_head_streams_after_the_close`, `a_panel_closed_while_a_head_streams_on_is_reopened_at_the_next_head`, `a_panel_closed_again_after_three_reopens_stays_closed_for_the_run`).
- [PN] `crates/qaros-sink/src/panels.rs` (`existing_panel`, `release_panels_to_reopen`, `closed_by_reset`, `consider_head`).
- [HDR] QAROS header at `e0c0195`, `qar-streaming-c/include/qar_streaming.h` (`qar_gui_panels_get_or_create` L4500-4515, `qar_gui_panel_get_*` L4859-4900).
- Hub log `C:\Users\jleho\AppData\Local\Temp\quaternar\logs\qar-launcher-default\c19e8322-465a-4e57-be89-bf96d6aa0136\launcher_stdout.log` and `mixer_9eac9097-3aa2-40c3-ac5c-96a060280679_stdout.log` on the Legion test machine, 2026-11-02 17:50 to 18:09 local time.

[SP]: ../../crates/qaros-sink/src/stream_panels.rs "Stream panels"
[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[HDR]: https://github.com/Quaternar/qaros/blob/e0c0195/qar-streaming-c/include/qar_streaming.h "QAROS C header"
