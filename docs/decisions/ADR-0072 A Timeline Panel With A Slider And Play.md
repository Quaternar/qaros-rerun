---
id: ADR-0072
title: A timeline panel in front of the user drives the viewer's time cursor through the panel server
status: accepted
date: 2026-10-26
repo: qaros-rerun
---

# ADR-0072: A timeline panel in front of the user drives the viewer's time cursor through the panel server

> The web page, the panel server and the image-load protocol are superseded by ADR-0078 (a timeline drawn by the viewer, driven by panel gestures); the web route and the panel server were removed. The panel's pose, the seek and Play rules and the thumb's hold stand.

## Context

A recording replays over time, and a user in the headset has no desktop time bar to scrub it with. GUI panels show web content and hand a finger on the panel to the page: as mouse input in the mixer's Ultralight panels and as touch input in its CEF panels. The panel pages are served by the viewer's own panel server (ADR-0019), so a page can talk back to the viewer over the same HTTP connection it loads from. Three panel behaviours seen in the glasses shape the page:

- A page with the arrow glyphs U+25C0 and U+25B6 made a panel blink and drop all input on build 3137; the same page with ASCII text was calm and took input.
- A drag that does not move within about 150 ms of contact is committed as a tap.
- The panels run two embedded browsers, Ultralight and CEF. Plain image loads work in both, with no script-side HTTP.

## Decision

- **One panel per run.** With `--panels`, every run opens a timeline panel, common name `timeline.panel.<volume>.rerun`, whether or not any view is published. It is 0.45 m by 0.12 m: the slider's track takes 0.36 m, a Play button 0.09 m. A new one opens with the panels' anchor head (ADR-0070, ADR-0071): its centre 0.5 m in front of the head along the levelled gaze and 0.2 m below the eyes, turned about +Y toward the head and tilted back about its top edge's axis until its front faces the eyes, top edge level (`timeline_panel_pose_facing`). 0.2 m below the eyes keeps it above a desk for a seated user. Like the view panels it is reattached by name where the Hub keeps it and hidden at Stop (ADR-0054); it is left out of the dead-page reload.
- **Page to viewer.** The panel server serves the page at `/timeline-panel`. A press or drag on the track pauses playback and moves the time cursor to that fraction of the active timeline's data; Play resumes. Each request is an image load of `/timeline?at=<fraction>` or `/timeline?play` with a unique `n=` pair so no cache answers it; the moves of a drag are thinned to one request per 80 ms, and the last position of a drag is always sent. The page takes mouse and touch input, and its touch handler cancels the mouse events a browser would make up from the same touch. The page uses ASCII text only.
- **Server to viewer.** The server queues requests; a seek directly after another replaces it, and the oldest go once 32 wait. Once per viewer frame the viewer takes them and sends Rerun time control commands for the active recording: a seek is `SetPlayState(Paused)` then `SetTimeClamped` at that fraction of the timeline's data range, Play is `SetPlayState(Playing)`. Requests that arrive before the viewer has a recording with a timeline are dropped. The desktop time panel shows the same cursor, since both drive Rerun's one time control.
- **Viewer to page.** The viewer publishes its cursor as a fraction of the active timeline's data every frame. The page polls `/timeline/position` every 250 ms, a PNG whose width is 1 plus the fraction in thousandths (no content until there is a timeline), and moves the thumb to it, so the thumb follows playback and the desktop time bar. Under the finger, and for 1 s after the last seek while the viewer catches up, polled positions are ignored.

## Alternatives considered

- A slider on every view's page: one more control on each 2D view, several sliders for one time cursor, and view pages that no longer show only the view.
- Rerun's web viewer in a panel: the route ADR-0019 rejects for views, with a second time cursor to keep in step.
- `fetch` or WebSocket from the page: one more capability to depend on in two embedded browsers, where an image load already reaches the server and a loaded image's width carries the answer back.
- Step buttons with arrow glyphs: the glyphs broke the panel on build 3137.

## Consequences

- Positive: a user in the headset scrubs and plays the recording; the desktop and the headset show and drive one time cursor. Stub tests open the panel without views in front of and below the head and reclaim it on restart; server tests check the routes, the queue and the position image; viewer tests check that a seek pauses at its fraction and Play resumes.
- Negative: every panel page polls the server, the timeline page four times a second.
- Negative: any client on the LAN can send seeks; the queue keeps that bounded but not authenticated.
- Negative: a drag that starts slowly is committed by QAROS as a tap, which seeks to the touched point and ends there.

## Evidence and sources

- [PN] `crates/qaros-sink/src/panels.rs` (`QarosPanels::with_timeline_panel`, `timeline_panel_pose_facing`, `TIMELINE_*`).
- [PG] `crates/qaros-rerun/src/timeline_requests.rs` (`TimelineRequests`, `TimelinePosition`), the request queue and the published cursor that stand.
- [TL] `crates/qaros-rerun/src/timeline_scrub.rs` (`TimelineScrub`, `time_commands`).
- [TS] `crates/qaros-sink/src/panels.rs` tests (`the_timeline_panel_stands_half_a_metre_ahead_and_a_fifth_below_the_eyes_facing_them`).

[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[PG]: ../../crates/qaros-rerun/src/timeline_requests.rs "Timeline requests"
[TL]: ../../crates/qaros-rerun/src/timeline_scrub.rs "Timeline scrub"
[TS]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
