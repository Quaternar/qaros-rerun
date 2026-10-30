---
id: ADR-0019
title: 2D views as URI panels through a local panel server (route B)
status: superseded
superseded_by: ADR-0076
date: 2026-10-05
repo: qaros-rerun
---

# ADR-0019: 2D views as URI panels through a local panel server (route B)

> Superseded by ADR-0076: panels are video streams of the viewer; the web route and the panel server were removed.

## Context

Rerun's 2D views (images, time series, bar charts, text logs, dataframes) must appear as QAROS GUI panels that follow the desktop time cursor. A QAROS GUI panel's content is a URI (web content). Rerun paints many 2D views with egui, and offers no per-view off-screen rendering. Two routes exist: (A) Rerun's web viewer in the panel, (B) our viewer renders each view and a small web page shows it.

## Decision

Route B. `panel-server` (tiny_http) keeps the latest PNG per view and serves a minimal page per view that polls for new frames. Views are cropped from the viewer's own UI (window or headless-harness screenshot). `QarosPanels` opens one 0.45 m wide panel per published view, within reach of a headset user's head (ADR-0070, ADR-0071), and one timeline panel whose slider sends seeks and Play back to the viewer through the panel server (ADR-0072). Panels are claimed by common name; at Stop they are hidden and kept in the session for the next run to reattach (ADR-0054).

## Alternatives considered

- Route A, Rerun's web viewer in the panel: depends on the panel's browser running Rerun's WebAssembly viewer with WebGL or WebGPU, on a URL selecting one view, and on a second viewer staying in step with the desktop time cursor.
- A frame-fed panel: the header has video-stream panels (`QarVideoSender` with `QarGuiPanelStreamIdExt`); not used yet, and the `qaros` crate reaches them only through its raw table.

## Consequences

- Positive: every view type works, from one data store and one time cursor.
- Negative: an encoder, a local web server and a polling hop of latency. What travels back is limited to what a page can send as image loads: the timeline panel's seeks and Play (ADR-0072); view pages only display.
- Negative: moving to video-stream panels is open.

## Evidence and sources

- [PS] `crates/qaros-sink/src/panels.rs` (`QarosPanels`); `crates/qaros-rerun/src/panel_capture.rs`. The panel server crate is removed.
- [README] `README.md`, "The QAROS sink", panels paragraph.
- [GP] QAROS GUI panels documentation at `b1338df`.
- [KI] [Known issues](../known-issues.md#history-why-panels-need-090-dev3177): the mixer's Ultralight renderers on builds up to 3137, behind closing and navigating panels as crash triggers.

[PS]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[README]: ../../README.md "qaros-rerun README"
[GP]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/gui-panels.mdx "QAROS GUI panels"
[KI]: ../known-issues.md#history-why-panels-need-090-dev3177 "Known issues"
