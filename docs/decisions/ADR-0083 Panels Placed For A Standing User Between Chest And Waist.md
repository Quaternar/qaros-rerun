---
id: ADR-0083
title: New panels are placed for a standing user, between chest and waist and tilted toward the eyes; a sitting preset keeps the earlier placement
status: proposed
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0083: New panels are placed for a standing user, between chest and waist and tilted toward the eyes; a sitting preset keeps the earlier placement

> Amends ADR-0070 (the row of view panels) and ADR-0072 (the timeline panel's pose): their placement becomes the `--panel-posture sitting` preset. The anchor head (ADR-0070, ADR-0071), the pose convention and the rule that an existing panel keeps its place stand.
>
> Amended by ADR-0090: an existing panel keeps its place while its centre is between 0.2 and 1.5 m from the eyes of the head that places panels and its size is the run's; otherwise it is moved to where a new panel opens. `--reset-panels` closes the viewer's panels so they are created afresh.

## Context

ADR-0070 opens the view panels on an arc 0.8 m from the anchor head with their top edges 0.1 m below the eyes, and ADR-0072 opens the timeline 0.5 m in front and 0.2 m below the eyes. Those numbers suit a user at a desk. Standing in the glasses, they put the panels at face height and at arm's length, so every tap is made with the arm raised and outstretched. The product owner's requirement (2026-11-02): the top of every panel at least 30 cm below eye level, most manipulation between waist and chest, standing use first.

What sets the numbers:

- **The body.** ANSUR II, the 2012 survey of U.S. Army personnel (4082 men, 1986 women), measured standing. Standing eye height is derived per subject as stature minus the seated difference between sitting height and eye height ([ANSUR]). Means below the eyes: chest height 0.35 m for women and men; waist height at the navel 0.54 m (women) and 0.59 m (men); elbow height, as acromial height minus shoulder-elbow length, 0.52 m and 0.57 m. Forearm-hand length 0.44 m and 0.48 m. Thumb-tip reach minus head length, an estimate of the full reach of an outstretched arm measured from the eyes: 0.55 m and 0.61 m on average, 0.49 m and 0.55 m at the 5th percentile. These are averages of one adult population; the placement is a default for an average adult, not a fit to one user.
- **The mixer.** It turns every panel to face the viewing head once the head leaves a 15 degree cone about the panel's front, pivoting halfway between the top edge and the centre, and keeps a panel below the user at least 30 degrees from horizontal, so a front faces at most 60 degrees above the horizon ([BR] `HandTranslationAndFaceUser`, `FacingDeadzoneCos`, `MinimumPanelTiltBelowUserDegrees`). It takes no touch from a head more than 60 degrees off a panel's front ([PF]). An app cannot fix a panel's orientation; D5 lists that as a request to QAROS.
- **The panels.** A panel is 0.45 m wide and, for the 1016x665 views of the measured sessions, 0.35 m tall; the timeline's 960x256 image makes it 0.20 m tall (`stream_panel_size`). Views take no touch; the timeline takes taps and drags (ADR-0078).

## Decision

New panels are placed for a standing user by default. The anchor head is the reference: the head the panels are placed by (ADR-0070, ADR-0071), midway between the eyes, levelled to its heading, at the moment it is taken.

- **Timeline: straight ahead, nearest, lowest.** Its centre stands 0.40 m ahead of the eyes on the floor plane and 0.50 m below them, just above elbow height, where a forearm reaches with the elbow at the side (`STANDING_TIMELINE_DISTANCE_METERS`, `STANDING_TIMELINE_CENTRE_BELOW_EYES_METERS`). It is the panel a user touches most, so it gets the most comfortable place.
- **Views: beside the timeline, alternately right and left.** Each view's centre stands 0.45 m from the eyes on the floor plane (`STANDING_VIEW_DISTANCE_METERS`), within the 5th percentile woman's outstretched reach and a little farther than the timeline. A view hangs from 0.30 m below the eyes as if upright: its centre sits 0.30 m plus half its height below them (`STANDING_TOP_BELOW_EYES_METERS`), 0.47 m for a 1016x665 view. The first view goes right of the timeline, the second left, then one place farther out on each side, 0.05 m apart along the arc (`STANDING_PANEL_GAP_METERS`), so the panels a run opens first stand nearest the gaze. With the timeline 0.45 m wide at 0.40 m, the first two views stand 62 degrees right and left of the gaze, the next two 122 degrees.
- **Every panel faces the eyes.** Each turns about +Y toward the eyes and tilts back about its top edge's axis until its front points at them, top edge level (`pose_facing_the_eyes`). Tilting lowers a top edge further and moves it away, so every top edge stays at least 0.30 m below the eyes: the timeline's sits 0.44 m below them, a 1016x665 view's 0.35 m. The fronts face 51 degrees (timeline) and 46 degrees (view) above the horizon, inside the mixer's 60 degree limit, and a panel created facing the eyes is left as it is by the mixer's facing turn.
- **Straight-line distance.** From the eyes, the timeline's centre is 0.64 m away and a view's 0.65 m, inside D5's 0.5 to 0.65 m for operable panels.
- **A sitting preset.** `--panel-posture sitting` keeps ADR-0070's row and ADR-0072's timeline pose unchanged. Standing numbers at a desk would put the panels in the desk: a seated user's eyes are about 1.2 m above the floor, and 0.5 m below them is desk height. `--panel-posture standing` is the default; the flag follows `--panels-at-stop` in form.
- **Existing panels keep their place.** A panel the session already holds is reattached where the Hub keeps it, as before (ADR-0070). To place panels anew under the new rule, close them in the Hub Display once: a closed panel counts as absent and is created again by its common name at the new place (ADR-0075).
- **The log** says who the panels are placed for: `new panels open within reach of user-N (...), at <head>, placed for a standing user`.

## Alternatives considered

- **Views centred in front, the timeline below them and nearer.** Tilted toward the eyes, a 0.35 m view at chest height reaches down to about 0.55 m below the eyes and toward the user, where a nearer timeline would have to stand; both cannot fit in the chest-to-waist band at forearm reach without overlapping in the line of sight or in space.
- **The timeline at the end of the views' row.** Puts the most touched control farthest from the gaze once there are two views.
- **Re-place existing panels on claim with `qar_gui_panels_update_pose`.** It would undo, on every run, wherever a user moved a panel, which ADR-0070 keeps on purpose; on 0.9.0-dev.3008 a panel's pose update never showed in the listing or its updates, and it is not verified on the current runtime. Closing the panels once in the Hub Display reaches the same result with no new call.
- **Narrower panels to tighten the arc.** At 0.45 m from the eyes a 0.45 m panel spans about 53 degrees of heading. A narrower panel would shrink the spread but also the views, which are already read at 0.65 m; the width stays, and the spread is accepted for more than two views.
- **Constants without a flag.** A seated user would then get panels in the desk with no way out short of a rebuild.

## Consequences

- Positive: standing, the timeline sits just above the elbow and every top edge at least 0.30 m below the eyes; unit tests assert the top edges, the centres between chest (0.35 m) and waist (0.56 m), the distance on the floor plane (0.40 to 0.45 m), the 60 degree tilt limit and the facing for the timeline, one view, four views with the gap between neighbours, a square view and a turned, tilted, moved head.
- Positive: the timeline and the first views open in front of the user, beside each other, instead of a row starting to the right.
- Negative: views spread wide: two views stand 62 degrees right and left of the gaze, so the user turns the head, or the body, to read them; a third and fourth stand at 122 degrees.
- Negative: a portrait view taller than about 0.52 m (content aspect above 1.1) centres below the waist band, since its top edge hangs from 0.30 m.
- Negative: panels placed by an earlier build stay at face height until closed in the Hub Display.
- Negative: the numbers are averages; a much shorter or taller user finds the panels proportionally high or low, and moves them.

## Evidence and sources

- [PN] `crates/qaros-sink/src/panels.rs` (`PanelPosture`, `STANDING_*`, `standing_timeline_panel_pose`, `standing_view_panel_pose`, `pose_facing_the_eyes`, tests `standing_*`), `crates/qaros-rerun/src/options.rs` (`--panel-posture`), `crates/qaros-rerun/src/qaros_setup.rs` (`panel_posture`).
- [ANSUR] Gordon, C.C., et al., 2015, "2012 Anthropometric Survey of U.S. Army Personnel: Methods and Summary Statistics", NATICK/TR-15/007, and its public data files; columns `stature`, `sittingheight`, `eyeheightsitting`, `chestheight`, `waistheightomphalion`, `acromialheight`, `shoulderelbowlength`, `forearmhandlength`, `thumbtipreach`, `headlength`.
- [BR] QuaternAR mixer source at `4aeffc63`, `StereoKitGUI/src/Browser/Browser.hpp` L362-384 (movement mode, facing deadzone and pivot, minimum tilt below the user), L845-929 (the tilt clamp and the facing target).
- [PF] Same source, `StereoKitGUI/src/Browser/PanelFacing.hpp` (60 degree touch limit).
- [HDR] QAROS header at `e0c0195`, `qar-streaming-c/include/qar_streaming.h`, `qar_gui_panels_get_or_create` (init used only when the panel is absent or closed) and `qar_gui_panels_update_pose`.

[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[ANSUR]: https://www.openlab.psu.edu/ansur2/ "ANSUR II public data"
[HDR]: https://github.com/Quaternar/qaros/blob/e0c0195/qar-streaming-c/include/qar_streaming.h "QAROS C header"
