---
id: ADR-0044
title: The headless viewer starts its next frame at once while a headset streams
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0044: The headless viewer starts its next frame at once while a headset streams

## Context

A headless loop that runs one egui pass and then sleeps 4 ms (`HeadlessOptions::step_interval`) idles the UI thread for part of every viewer frame. Measured on an RTX laptop at 1832x1920 with two targets, the viewer frame was 12.6 ms, of which the sleep was 4.4 ms. Every user begins a frame at best every second viewer frame, because `try_begin_frame` returns nothing in the viewer frame right after a show, so the rate per target is one over two viewer frame periods. A third of each period was idle.

## Decision

While a headset streams (`ViewerApp::headset_is_streaming`), the headless loop starts the next viewer frame at once. Without a stream it waits for the next pass egui schedules, through its repaint callback, so requests from any thread (a peer update, a render request, Stop, Rerun data) and the next panel capture wake it; the wait is at least the 4 ms step and at most `IDLE_REPAINT_INTERVAL` (0.5 s) after the last pass, and the harness's predicted pass time is the 4 ms step, so egui does not turn delayed repaints into immediate ones (`RepaintSchedule`).

## Alternatives considered

- A shorter fixed sleep: still idles the UI thread during every frame.
- Beginning frames again at the end of the viewer frame, so a user starts every viewer frame (the "lost viewer frame"): gains at most part of the 3.1 ms request-to-begin, and needs eye views after re_renderer's frame-global encoder is gone, a frame-lifecycle change in re_renderer. Not done.

## Consequences

- Positive: on the Vulkan path with a CPU receiver, 960x1000 one target went from 35.6 to 58.1 frames/s, two targets from 35 to 43.6, and 1832x1920 two targets from 21.7 to 35.8, with every pixel check passing.
- Negative: a streaming headless viewer keeps one CPU core busy on the UI thread.
- Positive: an idle headless viewer with nobody to render for runs a pass about every 0.5 s instead of every 4 ms, unless egui or a wake asks for one sooner.
- Neutral: windowed viewers are paced by the window's repaint and are unaffected; an occluded or minimized window starves the headsets (open).

## Evidence and sources

- [CODE] `crates/qaros-rerun/src/headless.rs` (`RepaintSchedule`), `crates/qaros-rerun/src/viewer_app.rs` (`IDLE_REPAINT_INTERVAL`, `headset_is_streaming`), `crates/qaros-rerun/src/main.rs` (the 4 ms `step_interval`).

[CODE]: ../../crates/qaros-rerun/src/headless.rs "Headless loop"
