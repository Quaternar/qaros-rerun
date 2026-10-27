---
id: ADR-0075
title: A panel closed in the Hub is claimed again by its common name, never replaced under a new one
status: accepted
date: 2026-10-26
repo: qaros-rerun
---

# ADR-0075: A panel closed in the Hub is claimed again by its common name, never replaced under a new one

## Context

A user can close a GUI panel in the Hub Display. The panel stays in the session's panel list with the state `QAR_GUI_PANEL_STATE_CLOSED` ([HDR] L1142-1147); the docs list the states and say nothing of what a later `get_or_create` with that common name does ([GP] L136). The panel's id is derived from its common name ([GP] L101). Seen with the glasses: a `get_or_create` by the closed panel's common name brings that panel back in the Visualizer, and a panel created under a fresh common name appeared beside it, a duplicate.

## Decision

- A listed panel whose state is closed counts as absent: it is neither reattached nor taken for a leftover of an earlier run ([PN] `existing_panel`, `is_left_by_earlier_run`).
- Its view claims it again with `get_or_create` and the same common name once there is a head to place new panels by (ADR-0070), which revives the closed panel; the viewer navigates it to the view's URL and sets its size, as for a new panel.
- No common name is ever changed to get a fresh panel.

## Alternatives considered

- A fresh common name for a view whose panel was closed (a suffix): the closed panel came back as well, so the user saw two panels for one view, and the next run would find both.
- Reattach a closed panel without navigating it, like a hidden one: its page is not known to be loaded, and the user closed it, so it comes back as a new panel would.

## Consequences

- Positive: closing a panel in the Hub and starting the viewer again brings back one panel per view, never a duplicate.
- Negative: what a revived panel keeps (its last pose, or the pose the claim passes) is not documented, and earlier on the same build, calls on a closed panel timed out instead of reviving it; the state's behaviour is a question for the QAROS docs.

## Evidence and sources

- [PN] `crates/qaros-sink/src/panels.rs` (`existing_panel`, `is_left_by_earlier_run`, `QarosPanels::open_panel`).
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L1142-1147 (`QarGuiPanelState`).
- [GP] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/gui-panels.mdx` L101 (id derived from the common name), L136 (panel states).

[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[GP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/gui-panels.mdx "GUI Panels"
