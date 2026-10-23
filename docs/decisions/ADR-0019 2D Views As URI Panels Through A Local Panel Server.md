---
id: ADR-0019
title: 2D views as URI panels through a local panel server (route B)
status: accepted
date: 2026-10-05
repo: qaros-rerun
---

# ADR-0019: 2D views as URI panels through a local panel server (route B)

## Context

Rerun's 2D views (images, time series, bar charts, text logs, dataframes) must appear as QAROS GUI panels that follow the desktop time cursor. A QAROS GUI panel's content is a URI (web content). Rerun paints many 2D views with egui, and offers no per-view off-screen rendering. Two routes exist: (A) Rerun's web viewer in the panel, (B) our viewer renders each view and a small web page shows it.

## Decision

Route B. `panel-server` (tiny_http) keeps the latest PNG per view and serves a minimal page per view that polls for new frames. Views are cropped from the viewer's own UI (window or headless-harness screenshot). `QarosPanels` opens one 0.6 m wide panel per published view in a row right of the volume, top edges level with the volume. At Stop it hides them and keeps them in the session for the next run to reattach.

## Alternatives considered

- Route A, Rerun's web viewer in the panel: depends on the panel's browser running Rerun's WebAssembly viewer with WebGL or WebGPU, on a URL selecting one view, and on a second viewer staying in step with the desktop time cursor.
- A frame-fed panel: the header at `b1338df` has video-stream panels (`QarVideoSender` with `QarGuiPanelStreamIdExt`); not used yet.

## Consequences

- Positive: every view type works, from one data store and one time cursor.
- Negative: an encoder, a local web server and a polling hop of latency; nothing travels back (panel pages only display).
- Negative: moving to video-stream panels is open.

## Evidence and sources

- [PS] `crates/panel-server/src/` (server, page); `crates/qaros-sink/src/panels.rs` (`QarosPanels`); `crates/qaros-rerun/src/panel_capture.rs`.
- [README] `README.md`, "The QAROS sink", panels paragraph.
- [GP] QAROS GUI panels documentation at `b1338df`.

[PS]: ../../crates/panel-server/src/server.rs "Panel server"
[README]: ../../README.md "qaros-rerun README"
[GP]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/gui-panels.mdx "QAROS GUI panels"
