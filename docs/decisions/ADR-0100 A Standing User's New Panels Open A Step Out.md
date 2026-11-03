---
id: ADR-0100
title: A standing user's new panels open a step out, where AR glasses see them at once
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0100: A standing user's new panels open a step out, where AR glasses see them at once

> Amends ADR-0083 (how far from the eyes a standing user's new panels open). The heights, the tilt toward the eyes, the order of the views and the sitting preset stand.

## Context

ADR-0083 opens a standing user's new panels within arm's reach: the timeline's centre 0.40 m ahead of the eyes on the floor plane, the views' 0.45 m, both well below the eyes. In the owner's test of build `test/combined-1103` on METALENSE 2 (2026-11-03, run 5), fresh panels at that distance sit too close to the body to be seen: AR glasses show a narrow field of view, and a panel 0.40 m ahead and 0.50 m down is below its lower edge. The first two views stand 62 degrees right and left of the gaze, outside it too. The owner asked for new panels about 0.5 m farther out, accepting that the user steps forward to touch them.

## Decision

- **New panels open half a metre farther out.** Standing, the timeline's centre stands 0.90 m ahead of the eyes on the floor plane (`STANDING_TIMELINE_DISTANCE_METERS`) and each view's centre 0.95 m from them (`STANDING_VIEW_DISTANCE_METERS`). From the eyes, the timeline's centre is about 1.03 m away and a view's about 1.06 m.
- **The rest of the layout stands.** The timeline's centre stays 0.50 m below the eyes, every view hangs from 0.30 m below them, each panel faces the eyes with its top edge level, and the views alternate right and left of the timeline 0.05 m apart along their arc. At the larger distance the timeline tilts back about 29 degrees instead of 51, and the first two views stand about 30 degrees right and left of the gaze instead of 62.
- **Only new panels move.** A reattached panel stays where it is held (ADR-0090, ADR-0095). `--move-far-panels` keeps its 1.5 m threshold, now about 0.45 m beyond where new panels open.
- **The sitting preset is unchanged** (`--panel-posture sitting`, ADR-0070, ADR-0072): its views stand 0.8 m from the head already.

## Consequences

- Positive: a standing user sees the timeline and the first views as soon as they open, within a narrow field of view and without looking down at the chest.
- Positive: the views stand nearer the gaze, so more of them fit the glasses' view at once.
- Negative: the panels are out of reach where they open; the user steps forward to scrub the timeline or grab a title bar.
- Negative: with `--move-far-panels`, a panel the user moved about half a metre farther away than where it opened is moved back at the next start. Without the flag (the default) it stays.

## Alternatives considered

- **Move the panels farther out and also raise them.** Raising them toward the gaze would put the hands in front of the face again, which ADR-0083 avoids; the larger distance alone already brings them within the view.
- **Raise the `--move-far-panels` threshold with the distance.** The owner set 1.5 m on 2026-11-03 (ADR-0095); the flag is off by default, so it stays.
