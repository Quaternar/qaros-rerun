---
id: ADR-0046
title: The headless viewer hides the top, blueprint, selection and time panels
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0046: The headless viewer hides the top, blueprint, selection and time panels

## Context

With the headless loop of ADR-0044 the Rerun UI pass is the largest part of a viewer frame (4.7 ms of 12.6 ms), though nobody sees a headless viewer's panels. The headset frames begin and are prepared inside that pass, so every millisecond of UI delays them.

## Decision

Headless, the viewer starts with `StartupOptions::panel_state_overrides` set to hidden for the top, blueprint, selection and time panels, so the pass lays out only the views. A window keeps Rerun's defaults.

## Alternatives considered

- A smaller headless window: shrinks the views, not the panels' layout cost.
- Hiding the panels in a window too: the desktop user needs them.

## Consequences

- Positive: UI pass 1.9 ms and viewer frame 3.4 ms at 1832x1920; with ADR-0044 and ADR-0045, the Vulkan path with Vulkan receivers reached 115.5 frames/s at 960x1000 one target and 90.3 at 1832x1920 one target, 59.5 with two. A repeat of the same matrix gave 117.0, 87.8 and 70.1 in runs that streamed smoothly, leaving out runs that stayed choppy for their whole length (see ADR-0057).
- Negative: relies on Rerun's `panel_state_overrides`, which a Rerun upgrade may rename.

## Evidence and sources

- [CODE] `crates/qaros-rerun/src/main.rs` (`startup_options`).

[CODE]: ../../crates/qaros-rerun/src/main.rs "Viewer startup options"
