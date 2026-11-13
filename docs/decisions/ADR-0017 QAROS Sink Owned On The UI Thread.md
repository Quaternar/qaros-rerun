---
id: ADR-0017
title: QAROS sink owned by the viewer app on the UI thread, lent to the renderer per call
status: accepted
date: 2026-10-05
repo: qaros-rerun
---

# ADR-0017: QAROS sink owned by the viewer app on the UI thread, lent to the renderer per call

## Context

The headset renderer sits behind the 3D view observer, which must be `Send + Sync` (ADR-0012). QAROS handles are bound to one thread and are not `Send` in the `qaros` crate. The renderer must also stream when the desktop window is hidden.

## Decision

The viewer app owns the sink on the UI thread and passes it to the renderer per call. The sink is lent to the UI pass through a thread-local, so the 3D view observer shows finished frames and begins the next ones in the same pass; readbacks are collected after the pass. Eyes are drawn and submitted on the viewer's queue directly after the UI pass, independent of window visibility. The viewer repaints continuously while streaming, woken from library threads by QAROS callbacks.

## Alternatives considered

- Begin before the UI pass and show at the end of the viewer frame: each user can then start a frame only every second viewer frame, which halves the stream rate (measured 33.7 against 58.5 fps at 960x1000).
- Rely on egui paint callbacks: work is silently dropped when the window cannot present.

## Consequences

- Positive: thread-bound handles never cross threads; streaming works headless and with an occluded window.
- Positive: all raw Vulkan submits happen on the thread that drives wgpu, which keeps queue access externally synchronized (ADR-0029).
- Negative: QAROS calls run on the UI thread, so any blocking call stalls the viewer (ADR-0039, ADR-0040).

## Evidence and sources

- [APP] `crates/qaros-rerun/src/viewer_app.rs`, `headset.rs`.
- [README] `README.md`, "The hook" (the sink lent to the UI pass, per-frame steps).

[APP]: ../../crates/qaros-rerun/src/viewer_app.rs "Viewer app"
[README]: ../../README.md "qaros-rerun README"
