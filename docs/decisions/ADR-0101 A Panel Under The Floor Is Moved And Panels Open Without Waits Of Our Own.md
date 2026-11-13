---
id: ADR-0101
title: A reattached panel under the floor is moved, and the next panel opens as soon as the last one's first frame is out
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0101: A reattached panel under the floor is moved, and the next panel opens as soon as the last one's first frame is out

> Amends ADR-0076 (from when a view's settle time counts), ADR-0095 (which held places are of no use without `--move-far-panels`) and ADR-0096 (when the claim after a new panel runs). ADR-0096's rule stands: no listing, claim or replacement while a new sender's first frame is due.

## Context

**A panel under the floor stays there.** In the owner's test of build `test/combined-1103` on METALENSE 2 (2026-11-03, run 5, drone lidar), the timeline panel an earlier run left was reattached and kept: `panel 29:36:BA:... stays where it is held: held at [1.3524945, -0.9751053, -1.4677215] ... its centre 4.37 m from the eyes`, with the eyes of the head that placed panels at y 0.83. Its centre was 1.90 m below the eyes, under the floor, where the user cannot see or reach it. Since ADR-0095 a held place is moved only when nearer than 0.2 m to the eyes or sized for another frame, unless `--move-far-panels` is given. The owner asked for panels under the floor to be moved without the flag.

The tracking origin's y = 0 is no floor: heads that placed panels streamed at y -0.51, 0.01 and 0.99 (ADR-0090). The floor can only be told from the eyes. ANSUR II puts an average standing adult's eyes 1.51 m (women) to 1.64 m (men) above it (ADR-0083).

**Panels open seconds apart.** In run 2 (19:40:32 to 19:40:47 UTC) the three panels opened about 15 s after connect, one by one:

| Time | Step |
| --- | --- |
| 32.24 | connected |
| 33.52 to 37.52 | listing, 4.0 s |
| 37.52 to 39.53 | timeline claim, 2.0 s; first frame 39.70 |
| 42.54 | camera decided ("stays where it is held") |
| 42.54 to 44.55 | camera claim, 2.0 s; first frame 44.59 |
| 45.55 | reprojection decided |
| 45.55 to 47.55 | reprojection claim, 2.0 s |

The listing and the claims are QAROS calls that wait out their reply window (known issues). The two gaps between them are waits of our own:

- **2.8 s after the timeline: the views' settle time, counted from the wrong moment.** A view gets a panel once published for 3 s (`PANEL_SETTLE`, `qaros_setup.rs`). The time was counted from the first sync that saw the view (`ViewActivity::new(now)`). The listing and the timeline claim blocked the syncs from 33.52 to 39.53, while the panel capture, on the UI thread, kept publishing the views (run 1 logged their settled crops 1.7 s after connect). The first sync to see them ran at 39.53, so they settled at 42.53.
- **1.0 s after the camera: the sync tick.** The sync is due 1 s after the last (`PANEL_SYNC_INTERVAL`). The sync right after the camera's claim, at 44.55, found its first frame due and returned (ADR-0096); the frame was out at 44.59, and the next sync came at the next tick, 45.55.

## Decision

- **A held panel under the floor is moved.** A reattached panel whose centre is more than 1.5 m below the eyes of the head that places panels (`HELD_PLACE_LOWEST_BELOW_EYES_METERS`) is moved to where new panels open, with or without `--move-far-panels`, like one nearer than 0.2 m or at another frame's size. 1.5 m below the eyes is at the floor for an average standing woman and 0.14 m above it for a man, and under the floor for a seated user. The log gives the reason: `moved to [x, y, z] with its top edge: held at [...], its centre 1.90 m below the eyes, lower than 1.5 m: at the floor or under it`.
- **A view settles from its first publish.** Each published image carries the time its key was first published since it was last withdrawn (`PanelImage::first_published_at`), and a view's settle time counts from it, so a view the capture published while a claim blocked the syncs can get its panel at the first sync after the claim. A view that leaves the layout is withdrawn and settles afresh when it returns, as before.
- **The sync after a new panel runs as soon as its first frame is out.** When a sync ends with a new sender's first frame due, the frame hook checks the first-frame gate on every frame and syncs as soon as it clears (`sync_due`, `QarosPanels::awaits_a_first_frame`), rather than at the next 1 s tick. The gate is ADR-0096's: no claim starts while a first frame is due.

## Consequences

- Positive: a panel left under the floor by another tracking origin comes back where the user sees it, without `--move-far-panels` or `--reset-panels`.
- Positive: in a run like run 2 the camera's claim starts at the timeline's first frame (39.70) instead of 42.54, and the reprojection claim at the camera's (about 41.8) instead of 45.55: the last panel opens about 4 s sooner. The listing and the 2 s claims remain.
- Negative: a panel a user deliberately put on the floor is moved at the next start.
- Negative: a view published less than 3 s before a sync, and kept, still waits for its settle time; only time already spent published counts.

## Alternatives considered

- **Tell the floor from y = 0.** The tracking origin is where the headset started, not the floor (ADR-0090).
- **Sync on every frame.** A sync also maintains the senders and follows the layout; checking only the first-frame gate on each frame is enough to start the next claim at once.
- **Drop the settle time.** It keeps a replayed recording's short-lived blueprint views from getting panels (ADR-0076, ADR-0082).
