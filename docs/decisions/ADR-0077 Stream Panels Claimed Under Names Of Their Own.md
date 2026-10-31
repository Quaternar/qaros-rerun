---
id: ADR-0077
title: Stream panels are claimed under common names of their own and pointed at each run's stream
status: accepted
date: 2026-10-29
repo: qaros-rerun
---

# ADR-0077: Stream panels are claimed under common names of their own and pointed at each run's stream

## Context

A panel's content type is fixed when it is created: `navigate_to_uri` on a stream panel and `set_stream` on a website panel both return `QAR_STATUS_GUI_PANEL_CHANGE_NOT_PERMITTED` ([HDR] L4224-4243). A panel's id is derived from its common name ([GP] L101), and `get_or_create` returns an existing panel unchanged ([HDR] L4194-4206), so a stream panel reattached by name still names the previous run's sender, which ended with that run. The binding's panel listing fails as soon as one panel in the session shows a stream ([known issues](../known-issues.md#panel-listing-fails-while-a-panel-shows-a-video-stream)). Observed with the glasses: after a Stop, a stream panel can come back listed as closed with an empty display name, still on the earlier run's stream id.

ADR-0054 claims panels by common name and hides leftovers of earlier runs; ADR-0075 revives a panel closed in the Hub by its common name.

## Decision

- **Names.** Stream panels use `<view>.stream.<volume>.rerun` for views and `timeline.panel-stream.<volume>.rerun` for the timeline; the display name repeats the common name. Website panels that earlier builds left under `<view>.<volume>.rerun` and `timeline.panel.<volume>.rerun` are never claimed.
- **Listing.** Panels are listed through a raw listing that reads `qar_gui_panel_get_content_type` before the URI or the stream id ([HDR] L4557-4567, `crates/qaros-sink/src/panel_listing.rs`). A website panel listed under one of our stream names is logged and skipped, with no call on it.
- **Claim.** For each wanted panel: create the sender, `get_or_create` with `QarGuiPanelStreamIdExt` naming it, then always `set_stream` to this run's stream (it is a no-op for a new panel), then the size. The earlier panel is found by its common name, or by its panel id when the listing no longer carries the name. A panel listed hidden or closed, or not listed at all, is set visible once its stream is in place; a minimised one stays minimised. Never `navigate_to_uri`.
- **Leftovers.** A visible stream panel of this volume that the run does not hold, for a view it does not publish, is hidden (ADR-0054). Website panels left by earlier builds get no call at all, since hiding website panels was itself a mixer crash trigger on 3137; they are logged once, with the advice to close them in the Hub Display.
- **Amends.** ADR-0054 and ADR-0075 hold with these names; a revived stream panel gets `set_stream` instead of a navigation.

## Alternatives considered

- **The website panels' names.** A website panel under the same name cannot become a stream panel, so the viewer would have to close it first, the crash trigger ADR-0054 avoids.
- **Fresh names per run.** Every run would create new panels, the mixer would close the old ones once their streams end, and closed orphans would pile up in the session.
- **Trusting `get_or_create` alone.** The panel would keep showing the earlier run's dead stream and wait for it forever ([BC] L816-829).

## Consequences

- Positive: in a Hub session that still holds website panels of an earlier build, a run reattaches its own stream panels without duplicates and leaves the website panels as they are.
- Positive: unit tests over a fake stream backend cover the order (sender before panel; `set_stream` then visible on a reattached panel; no call on a website leftover), and stub tests check that the raw listing matches the binding's for website panels.
- Negative: website panels of earlier builds stay in the session until a user closes them.

## Evidence and sources

- [PN] `crates/qaros-sink/src/panels.rs`, `crates/qaros-sink/src/stream_panels.rs`, `crates/qaros-sink/src/panel_listing.rs`.
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L4194-4206 (`get_or_create`), L4224-4243 (content type fixed, `set_stream`), L4557-4567 (content type getter).
- [GP] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/gui-panels.mdx` L101.
- [BC] Mixer source at `c6e052d3`, `StereoKitGUI/src/Browser/BrowserControl.hpp` L816-829 (a stream panel waits for its stream's layout).

[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[GP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/gui-panels.mdx "GUI Panels"
