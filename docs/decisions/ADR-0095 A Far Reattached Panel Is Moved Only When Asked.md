---
id: ADR-0095
title: A reattached panel held far from the eyes is moved only with --move-far-panels
status: proposed
date: 2026-11-03
repo: qaros-rerun
---

# ADR-0095: A reattached panel held far from the eyes is moved only with --move-far-panels

> Amends ADR-0090 (the distance rule of a held place) and ADR-0091 (a panel listed `Closed` goes back to its held place by the same rule).

## Context

Seen on METALENSE 2 with QAROS 0.9.0-dev.3184 on 2026-11-03, run 2 of the 1102d test (viewer log of the `structure_from_motion` demo written 12:47:05Z, times UTC):

- **A panel the user could still reach was moved.** At 12:46:39 the camera panel was listed `Closed` (the headset mixer closes hidden panels about five minutes after their streams end) and created afresh. Its held place was 1.53 m from the eyes of the head that placed panels, so ADR-0090's rule moved it to where new panels open: `moved to [1.215, -0.864, -1.082] with its top edge: held at [1.0931597, -0.39052358, 0.20302479] ... its centre 1.53 m from the eyes, farther than 1.5 m`. The timeline and reprojection panels, at 1.11 m and 0.91 m, stayed where they were held.
- **The distance depends on where the user stands at start, not on where the panel is.** A standing user walks about the room between runs; 1.5 m from the eyes at one start is 0.9 m at another. The owner saw panels at new places after the five minutes and asked that this move be opt-in.
- **The other reasons to move still hold.** A held place nearer than 0.2 m cuts through the head, and a size other than this run's means the place was chosen for another frame (ADR-0087). A panel listed `Closed` with no place left (`held at [0, 0, 0] with its top edge, 0.000x0.000 m`, run 1 at 12:29:42) is moved by the size rule, as it was on 2026-11-03.

## Decision

- **`--move-far-panels` turns the distance rule on; it is off by default.** Without the flag, a reattached panel, or one listed `Closed` and created afresh (ADR-0091), stays at its held place however far its centre is from the eyes of the placing head. With it, a centre farther than 1.5 m moves the panel to where new panels open, as ADR-0090 decided.
- **The near and size rules stay on.** A held centre nearer than 0.2 m, or a width or height more than 5 mm off this run's size, moves the panel with or without the flag.
- **The log keeps the distance.** `panel ... stays where it is held: held at [...] with its top edge, W x H m, its centre D m from the eyes`, so a far panel left in place is visible in the log. The flag needs `--panels`.

## Alternatives considered

- **Raise the limit to 2.5 m.** Still moves a panel the user left on the other side of a room they walk in, and still catches a panel placed in another tracking origin only when that origin is far off. The owner chose the flag.
- **Drop the distance rule.** A panel placed by another headset's tracking origin has no other way back within reach short of `--reset-panels` or a close in the Hub Display; the flag keeps that way for whoever needs it.

## Consequences

- Positive: panels stay where the user left them across runs and across the mixer's close of hidden panels, wherever the user stands at start.
- Negative: a panel placed in another headset's tracking origin, or in another part of the room, stays there; the user grabs it, starts once with `--move-far-panels` or `--reset-panels`, or closes it in the Hub Display.
- Open: not yet tried in the glasses. The owner's check: leave a panel more than 1.5 m away, stop, start, and see it stay; start with `--move-far-panels` and see it moved within reach.

## Evidence and sources

- [PN] `crates/qaros-sink/src/panels.rs` (`HELD_PLACE_FARTHEST_METERS`, `unusable_held_place`, `place_for_claim`, `QarosPanels::with_move_far_panels`; tests `with_move_far_panels_a_panel_held_beyond_a_step_and_a_reach_is_moved`, `without_move_far_panels_a_panel_held_beyond_a_step_and_a_reach_stays`, `a_panel_held_at_the_head_is_moved_with_or_without_move_far_panels`, `a_claim_moves_only_a_held_panel_whose_place_is_of_no_use`).
- [OP] `crates/qaros-rerun/src/options.rs` (`--move-far-panels`; test `far_panels_are_moved_only_when_asked_and_only_with_panels`) and `crates/qaros-rerun/src/qaros_setup.rs`.

[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[OP]: ../../crates/qaros-rerun/src/options.rs "Command-line options"
