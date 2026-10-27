---
id: ADR-0070
title: New panels open within reach of a headset user's head; existing panels stay where the Hub keeps them
status: accepted
date: 2026-10-26
repo: qaros-rerun
---

# ADR-0070: New panels open within reach of a headset user's head; existing panels stay where the Hub keeps them

## Context

A GUI panel's pose is the midpoint of its top edge, with panel -Z as the front normal toward the viewer ([HDR] L781-800, [CS] "GUI panels"). ADR-0049 posed panels in a row right of the app volume, facing a viewer assumed on the volume's +Z side. With the default 2 m volume that row starts more than 1 m right of the volume's centre, at its centre depth, beyond the reach of a user standing at the content, and GUI panels are operated by touch in the headset.

Three things QAROS does constrain where a panel can go:

- The Hub owns a panel's place once the panel exists. It keeps the pose the panel was created with, or wherever a user moved it, across runs, and ignores later pose calls for it. `get_or_create` returns an existing panel unchanged ([GP] L18-20).
- Panels are session state with one stored pose ([GP] L14), but the mixer turns every panel to face the head that views it, with a deadzone of about 15 degrees, and takes no touch on a panel the head is more than 60 degrees off. Seen in the glasses as panels tilting toward whoever moved last, while the stored pose stayed still.
- A head is known only once a user streams: the viewer takes it from the two eye poses of that user's frame request, midway between the eyes, turned like the left eye ([HS] `head_between_eyes`).

## Decision

A new panel is created at its final place, within reach of one head, the anchor; an existing panel is never posed again.

- **Anchor.** The first streaming head within 4 m of the app volume's centre on the floor plane (`REACH_ANCHOR_MAX_DISTANCE_METERS`) whose device places panels (ADR-0071) becomes the anchor, once per run. It is levelled: only its heading counts, so a tilted head neither tilts nor raises the panels (`gravity_level`). Later heads change nothing. A head farther away, such as a camera watching from across the room, is logged once and skipped.
- **Row.** The view panels, 0.45 m wide with the view's aspect, stand on an arc 0.8 m from the anchor on the floor plane (`REACH_DISTANCE_METERS`), starting 0.25 m to the right of the gaze along the arc and going right, 0.1 m apart along the arc, top edges 0.1 m below the eyes. Each stands upright and turns about room +Y only, so its -Z front faces the anchor (`panel_pose_within_reach`). Panels created later in the run join the same row. The timeline panel opens in front of the anchor instead (ADR-0072).
- **Waiting.** Until there is an anchor, a view that needs a new panel waits, and the log says so once per view; a view whose panel exists is reattached at once.
- **Existing panels.** A panel found by its common name (ADR-0054) keeps its place: the viewer sets its size from this run's view and no pose.
- **Kept from ADR-0049.** The pose convention (top-edge midpoint, -Z front) and the panel base URL named by the LAN address.

## Alternatives considered

- Beside the app volume (ADR-0049): out of reach for a user at the content, and its premise of one viewer on the volume's +Z side does not hold: the mixer turns each panel toward each head anyway.
- Re-pose every panel on every run: the Hub ignores the pose of an existing panel, and doing it would undo where a user moved it.
- Follow the head while the user moves: a panel that moves with the head cannot be walked up to and touched, and a pose call after creation is ignored.
- Turn the panels in all three axes to face the eyes: the mixer already turns them toward the viewing head; an upright panel stays readable when the mixer's deadzone leaves it as created.

## Consequences

- Positive: on the first run the panels open where the headset user can touch them, beside what they look at rather than in front of it.
- Positive: a panel a user moved stays there across runs; stub tests assert that a restart sets no pose (`a_restart_leaves_a_panel_where_the_user_put_it`) and that new panels wait for a head and then open within reach (`new_panels_wait_for_a_viewer_near_the_volume_and_open_within_reach`); unit tests pin the arc for a head at the origin, a turned and moved head, and a tilted head.
- Negative: the row is placed for one user; another user may find it behind them, although the mixer turns each panel toward them.
- Negative: the row's slots count only the panels this run creates, so a new panel can open where an earlier run's panel still stands.
- Negative: without a head near the app volume a run opens no new panels at all.

## Evidence and sources

- [PN] `crates/qaros-sink/src/panels.rs` (`QarosPanels::consider_head`, `panel_pose_within_reach`, `gravity_level`, `is_near_the_volume`, `REACH_*`, `PANEL_WIDTH_METERS`, `PANEL_GAP_METERS`), `crates/qaros-rerun/src/qaros_setup.rs` (`PanelHook::on_viewer_frame`).
- [HS] `crates/qaros-rerun/src/headset.rs` (`head_between_eyes`, `ViewerHead`).
- [TS] `crates/qaros-sink/tests/stub.rs` (`opens_a_panel_per_view_within_reach_and_hides_them_at_stop`, `new_panels_wait_for_a_viewer_near_the_volume_and_open_within_reach`, `a_restart_leaves_a_panel_where_the_user_put_it`).
- [HDR] QAROS header at `b1338df`, `qar-streaming-c/include/qar_streaming.h` L781-800 (panel space).
- [CS] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/coordinate-systems.md`, "GUI panels".
- [GP] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/gui-panels.mdx` L14 (shared session state), L18-20 (`get_or_create`).

[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[HS]: ../../crates/qaros-rerun/src/headset.rs "Headset renderer"
[TS]: ../../crates/qaros-sink/tests/stub.rs "Sink stub tests"
[HDR]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[CS]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/coordinate-systems.md "Coordinate Systems"
[GP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/gui-panels.mdx "GUI Panels"
