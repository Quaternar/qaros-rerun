---
id: ADR-0082
title: Held panels follow the viewer's layout, hidden when their view leaves it and shown when it returns
status: accepted
date: 2026-11-02
repo: qaros-rerun
---

# ADR-0082: Held panels follow the viewer's layout, hidden when their view leaves it and shown when it returns

## Context

A 2D view becomes a stream panel once it has been on screen for 3 s (`PANEL_SETTLE`), and a run holds that panel until Stop (ADR-0054, ADR-0076, ADR-0077). Opening another recording in the desktop viewer (the command palette, a URL such as `https://app.rerun.io/version/0.40.0/examples/arkit_scenes.rrd`) makes it the active recording with its own blueprint. Seen in the glasses on QAROS 0.9.0-dev.3184: the new recording's views got panels, and the previous recording's panels stayed open beside them, frozen on their last image.

Two things kept them. Panel capture published only the views on screen, but `PanelImages` kept every image it was ever given, so the sink counted a view that had left the layout as still published. And `QarosPanels::sync` only ever claimed panels: nothing hid a held panel whose view had gone, and the leftover check of ADR-0054 skips held panels by design.

The embedding app sees which views are on screen in each capture (Rerun's `ViewRectPublisher`, refreshed every frame), and the active recording through `active_recording_id`. Rerun stays unpatched for this.

## Decision

- **Withdrawn with the layout.** Each capture publishes the views on screen and withdraws from `PanelImages` every view not on screen in it (`PanelImages::retain_views`). A capture that never happens, as while the desktop window is minimised, withdraws nothing, so panels do not vanish with the window.
- **Hidden, not closed.** At each sync, a held view panel whose view is no longer published is set `QAR_GUI_PANEL_STATE_HIDDEN`. It keeps its sender and its place, as at Stop (ADR-0054, ADR-0079).
- **Shown again when the view returns.** A returning view settles afresh, like a new one, so a blueprint that is active only while a recording loads does not flash its panels. The panel is set `VISIBLE` once the view has settled and its current image has been streamed to the panel's sender, so it does not reappear with the image it showed before it left. A view of the same name in another recording (a `plot` in both) keeps its panel, which shows the new view's pixels.
- **The timeline panel stays.** It is no view: it draws and drives the active recording's time control, read every frame, so it follows a recording switch by itself (ADR-0078).
- **Stop.** A panel hidden with its view gets no second hide at Stop.

| Candidate | Fits | Why |
| --- | --- | --- |
| Hide panels whose view left the capture's layout | yes, chosen | Uses only what the app sees; covers a recording switch, a blueprint that drops a view, and a view hidden in the blueprint, with one rule. |
| Hide on a change of the active recording id | no | Misses a blueprint change within one recording, and a recording switch whose new blueprint keeps a view of the same name would still need the layout to decide. |
| Hide views not published for 1.5 s (`PANEL_LIVE`) | no | A minimised window publishes nothing, which would hide every panel. |
| Close the panels of views that left | no | A held panel listed closed reads as closed by a user or a failing mixer and is released for the rest of the run (ADR-0075), so a returning view could not get it back before the next run. |
| Key panels by recording as well as view name | no | Changes every panel's common name, so no run would reattach the panels of earlier runs (ADR-0054, ADR-0077). |

## Consequences

- Positive: after opening another recording, the previous recording's panels are hidden within about a second (a 250 ms capture, then the once-a-second sync) and the new recording's 2D views get panels 3 s after they appear; opening the first recording again shows its panels where they stood.
- Positive: a view hidden or removed in the blueprint takes its panel with it.
- Negative: a view in a background tab of the desktop layout is not on screen, so its panel is hidden until the tab is brought forward (3 s after). A view maximised in the desktop layout hides the panels of the others the same way.
- Negative: a panel a user minimised whose view leaves and returns comes back visible. Knowing its state takes a panel listing, which blocks the QAROS thread for seconds on 0.9.0-dev.3177 ([KI] "GUI panel queries block for about 2 s").
- Negative: a hidden panel stays in the session until the end of the run and is hidden at Stop like the others; its sender lives until then.

## Evidence and sources

- [PI] `crates/qaros-sink/src/panel_images.rs` (`retain_views`).
- [SP] `crates/qaros-sink/src/stream_panels.rs` (`follow_published_views`, `hide_all`; tests `after_a_recording_switch_only_the_panels_of_views_that_left_are_hidden`, `a_view_back_in_the_layout_is_shown_again_once_settled_and_its_image_streamed`, `stop_hides_only_the_panels_not_already_hidden_with_their_views`).
- [PN] `crates/qaros-sink/src/panels.rs` (`QarosPanels::sync`).
- [PC] `crates/qaros-rerun/src/panel_capture.rs` (`publish_on_screen_views`; tests `a_recording_switch_withdraws_the_views_that_left_and_publishes_the_new_ones`, `a_capture_with_no_view_on_screen_withdraws_every_view`).
- [QS] `crates/qaros-rerun/src/qaros_setup.rs` (`observe_views`; test `a_view_that_left_the_layout_settles_afresh_when_it_returns`).
- [KI] [Known issues](../known-issues.md).

[PI]: ../../crates/qaros-sink/src/panel_images.rs "Panel images"
[SP]: ../../crates/qaros-sink/src/stream_panels.rs "Stream panels"
[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[PC]: ../../crates/qaros-rerun/src/panel_capture.rs "Panel capture"
[QS]: ../../crates/qaros-rerun/src/qaros_setup.rs "Panel hook"
[KI]: ../known-issues.md "Known issues"
