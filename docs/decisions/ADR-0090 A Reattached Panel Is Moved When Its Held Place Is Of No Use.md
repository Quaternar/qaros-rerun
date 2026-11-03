---
id: ADR-0090
title: A reattached panel is moved when its held place is of no use, a panel closed while held stays closed, and --reset-panels closes the viewer's panels before their claims
status: proposed
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0090: A reattached panel is moved when its held place is of no use, a panel closed while held stays closed, and --reset-panels closes the viewer's panels before their claims

> Amended by ADR-0091: a panel closed while held is reopened once a target's head streams after the close, and a panel listed `Closed` at its claim is created afresh and shown, at its held place when that place is of use.

> Amends ADR-0083 (an existing panel keeps its place only while that place is of use; every claim waits for the head that places panels), ADR-0075 (a panel closed while held is seen at once and not hidden at Stop) and ADR-0087 (the claim's size is sent again with the first frame).

## Context

Seen on METALENSE 2 with QAROS 0.9.0-dev.3184 on 2026-11-02 (viewer logs `viewer-20261102T160522Z-6852.log` and `viewer-20261102T161145Z-3172.log` of the `structure_from_motion` demo, Hub log of session `c19e8322`, times UTC):

- **Panels held where nobody could use them.** At 16:06 all three panels were reattached (`reattached (listed as "", Hidden, ...)`), none created, so the standing placement of ADR-0083 never applied. QAROS carries an app's panels into the Hub's next session, so a new Hub session does not reset them. Their places came from earlier runs: the timeline and camera panels were created at 11:02 by a METALENSE head at (-1.77, 0.42, 0.33), the reprojection panel at 12:59 by a HoloLens remoting head at (-1.49, 0.99, 1.23), each in its own tracking origin; the head placing panels at 16:05 was at (0.08, 0.01, 0.06), and at 16:11 at (-0.15, -0.51, 0.49). The owner saw one panel at the head and two as tiny rectangles at one spot.
- **The Hub's close was undone by the viewer's Stop.** At 17:10:57 local time the Hub Display's peer proposed `State: 3` (`QAR_GUI_PANEL_STATE_CLOSED`) for seven panels, the three of this viewer among them, and the Hub accepted them. The first run was still streaming; it never listed the panels again, and at its Stop (17:11:36) it proposed `State: 2` (hidden) for the three, on base revisions after the close, which the Hub accepted. The next run found them `Hidden` and reattached them at their old places. The Hub's close is a real close; the viewer's Stop turned it into a hide.
- **The size is no stale proposal.** `set_size` and `set_pose` are fast updates (`GuiPanelImpl::ChangeSize`, `UpdatePose` → `PublishFastUpdated`, [QS] `GuiPanelImpl.cpp` L411-472). The authority applies them without the per-field check that refuses a stale proposal and answers an update it rejects only with a correction on the size or pose correction topic ([QS] `GuiPanelProcessor.hpp` `HandleFastProposal` L539-640). They never appear in the Hub's `validating gui update proposal` lines; the refusals at 17:12:00 to 17:12:07 local time were `Content` (the timeline, resent and accepted 90 ms later) and `State` (all three panels). A fast update is sent once, and one that reaches the Hub before a new panel's create is dropped there (`Missing gui panel for fast update`, L556-566). Neither the Hub nor the viewer logged the size the Hub held for each panel.
- **The aspect is kept.** A 960x472 frame gets a 0.450 x 0.287 m panel: the content area left by the mixer's chrome is 0.370 x 0.182 m, the frame's 2.03:1 (`stream_panel_size`, ADR-0076). The mixer draws a stream at the content width and letterboxes or scrolls it, never stretching it ([MX] `GuiPanelStreamViewport.hpp`), and ignores `content_scale` for a stream panel ([HDR] `QarGuiPanelSize`).
- **No code path makes a panel 1 x 2 cm.** The viewer's panels are 0.45 m wide and at least 0.14 m tall; the mixer's own resize keeps a window at least 0.3 x 0.3 m ([MX] `Browser.hpp` L126). A 0.45 m panel held several metres away, as the panels above were, is what looks that small.

## Decision

- **Every claim waits for the head that places panels.** A reattached panel is checked against that head, so it is claimed only once one is taken (ADR-0070, ADR-0071), as a new panel always was. The listing finds a panel by the id QAROS derives from its common name, a version 5 UUID in the QAR namespace ([QS] `CommonImpl.cpp` `CreateUUIDFromName`), because a panel carried into a new Hub session is listed with an empty display name (`panel_ids.rs`).
- **A held place of no use is replaced.** A reattached panel is moved to where a new panel would open, with `set_pose` and `set_size`, when its centre is farther than 1.5 m from the eyes of the placing head, nearer than 0.2 m, or its width or height is more than 5 mm off this run's size (`unusable_held_place`). 1.5 m is one step (about 0.7 m) beyond the outstretched reach of an arm measured from the eyes (0.61 m for the average man, ADR-0083), and more than twice the 0.65 m at which ADR-0083 places a panel: a panel the user moved within the space they work in stays, one placed in another tracking origin or another part of the room is moved. Below 0.2 m the panel cuts through the head. A size other than this run's means the place was chosen for another frame, and a standing view hangs from its top edge by its height. A moved view takes the next slot of the row, as a new one does. The log names the held place and the reason: `stream panel ... moved to [x, y, z] with its top edge: held at [...], 0.450x0.287 m, its centre D m from the eyes, farther than 1.5 m from the eyes of the head that places panels`, or `panel ... stays where it is held: ...`.
- **The first frame sends the claim's fields again.** With the claim's first frame the panel gets its size, the pose the claim moved it to, and the show (as before, ADR-0087's frame size or a replacement's size), so each follows the stream's commit and reaches a panel the Hub knows by then.
- **A panel closed while held stays closed.** Each claimed panel's state is watched (`qar_gui_panels_subscribe_panel_updates`, reading only `qar_gui_panel_get_state`, `panel_state_watch.rs`). A close seen after the panel was seen open releases it at the next sync: no further stream, no hide at Stop. The next run's `get_or_create` builds it afresh from the init, at that run's placement and size. A close between the last sync and Stop is skipped by the hide too.
- **`--reset-panels`.** Each panel this viewer claims is closed first (`qar_gui_panels_close_panel`, truly `QAR_GUI_PANEL_STATE_CLOSED`), and its claim waits for a listing made at least 1 s after the close; once a listing shows it closed or absent, the claim creates it afresh. A panel still listed open after three such listings, or one whose close fails, is claimed as it is, with the place check above. The close comes at a panel's first claim: a view's common name is known only once the view settles. A website panel under the name is never closed. The flag needs `--panels`.

## Alternatives considered

- **Move every reattached panel on every claim.** ADR-0083 rejected it: it undoes, every run, wherever a user moved a panel. The distance and size checks move only panels whose place is of no use.
- **Also move a panel held by another device category.** The METALENSE and the HoloLens remoting both report `QAR_DEVICE_CATEGORY_HEADSET` (`headset "Remoting Xr System"` in the 12:52 log), and two METALENSE starts at different spots have different origins too, so the category would not have caught any case above; the Hub keeps no record of who placed a panel. The distance check catches a changed origin whatever device placed the panel.
- **Close a reattached panel instead of moving it.** A close must be seen by the library before `get_or_create` rebuilds the panel, which costs a listing of about 4 s on the QAROS thread (Quaternar#345), and a panel closed while another proposal is in flight can come back closed. Moving is two fast updates. The close stays for `--reset-panels`, where the owner asks for it.
- **List the panels at Stop and skip the closed ones.** A listing blocks for seconds; Stop has 600 ms (ADR-0079).
- **Re-send the size only when the claim's proposal was refused.** The library reports neither a refusal nor a correction to the app; sending the fields once more with the first frame costs two fast updates.

## Consequences

- Positive: panels left in another tracking origin, at the head, or sized for another layout come back within reach at this run's size, without a trip to the Hub Display; panels a user moved within reach stay.
- Positive: a close in the Hub Display while the viewer runs is final for that run, and the next run creates the panel at its placement, as ADR-0083 told users to expect.
- Positive: unit tests assert the moved, kept and resized cases with the measured heads, the call order of a moved claim and its first frame, the release of a panel closed in the Hub Display and its absence from the Stop's hide, the reset's close, wait and give-up, the ids of the demo's panels, and the camera frame's aspect on the content area.
- Negative: a reattached panel waits for a head as a new one does, up to `HEADSET_WAIT` without a headset.
- Negative: a panel a user put farther than 1.5 m on purpose, or kept through a layout change, is moved back at the next run.
- Negative: `--reset-panels` holds every claim for one or two listings at start, about 5 s each on 0.9.0-dev.3184.
- Open: not yet tried in the glasses. The owner's check: start with `--reset-panels` once and see the panels open at the standing placement; start again without it and see them reattached and kept; move one beyond two metres, restart, and see it back within reach; close one in the Hub Display while the viewer runs, stop, start, and see it created afresh.

## Evidence and sources

- [SP] `crates/qaros-sink/src/stream_panels.rs` (`PanelPlace`, `claim`, `reassert_with_the_first_frame`, `release_panels_closed_elsewhere`, `hide_all`; tests `a_panel_moved_by_its_claim_is_sized_moved_and_shown_then_all_three_again_with_its_first_frame`, `a_panel_closed_in_the_hub_display_is_released_at_the_next_sync_and_not_hidden_at_stop`, `a_panel_closed_after_the_last_sync_is_not_hidden_at_stop`).
- [PN] `crates/qaros-sink/src/panels.rs` (`HELD_PLACE_FARTHEST_METERS`, `unusable_held_place`, `place_for_claim`, `PanelReset`; tests `a_panel_held_beyond_a_step_and_a_reach_or_at_the_head_is_moved`, `a_reset_closes_a_held_panel_then_claims_once_a_later_listing_shows_it_closed`), `crates/qaros-sink/src/panel_ids.rs`, `crates/qaros-sink/src/panel_state_watch.rs`, `crates/qaros-rerun/src/options.rs` (`--reset-panels`).
- [QS] qar-streaming source at `e487a311`, `libs/qar-streaming/src/` (`GuiPanelImpl.cpp`, `GuiPanelProcessor.hpp`, `CommonImpl.cpp`) and `libs/qar-streaming-c/src/v0/gui_panels.cpp` (`qar_impl_gui_panels_set_state`, `qar_impl_gui_panels_close_panel`).
- [MX] QuaternAR mixer source at `4aeffc63`, `StereoKitGUI/src/Browser/Browser.hpp` and `qar-common-lib/include/qar_common/gui_panel_gestures/GuiPanelStreamViewport.hpp`.
- [HDR] QAROS header at `e0c0195`, `qar-streaming-c/include/qar_streaming.h` (`QarGuiPanelSize`, `QarGuiPanelState`, `qar_gui_panels_get_or_create`).

[SP]: ../../crates/qaros-sink/src/stream_panels.rs "Stream panels"
[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[HDR]: https://github.com/Quaternar/qaros/blob/e0c0195/qar-streaming-c/include/qar_streaming.h "QAROS C header"
