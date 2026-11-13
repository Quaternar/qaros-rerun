---
id: ADR-0073
title: A failed panel listing opens no panel and is retried after half a second, then every 30 s
status: accepted
date: 2026-10-26
repo: qaros-rerun
---

# ADR-0073: A failed panel listing opens no panel and is retried after half a second, then every 30 s

## Context

Before it opens a panel, the viewer lists the session's panels: without the list a panel an earlier run left would pass for a new one, be navigated and stay hidden (ADR-0054). The `qaros` crate's `gui_panels().list()` reads `qar_gui_panel_get_content_uri` for every listed panel ([QP] L207, L333-337). For a panel that shows a video stream instead of a URI that read fails, so the whole listing fails whenever any panel in the session, of any app, shows a stream; the header has `qar_gui_panel_get_content_type` to tell the two apart first ([HDR] L1150-1154, L4561). Each failed listing blocked the viewer's UI thread for 3 to 4 s ([KI]). The panel hook syncs every second.

## Decision

`QarosPanels::sync` opens nothing while the listing fails, and logs the failure with the time to the next try. The first retry comes 0.5 s later (`LISTING_RETRY_FAST`), in case the failure was passing; every later one 30 s after the last (`LISTING_RETRY_SLOW`). A listing that succeeds resets the count.

## Alternatives considered

- Retry at every sync, once a second: a lasting failure blocks the UI thread most of the time.
- Open panels without the list: reattached panels would be navigated as new ones, the call the viewer avoids (ADR-0054).
- Read the panels through the raw table, checking the content type first: a second copy of the binding's listing in this repository; the fix belongs in the binding.

## Consequences

- Positive: a stream panel elsewhere in the session costs the viewer at most one blocked UI-thread moment every 30 s, and no panel is navigated by mistake; a unit test pins the delays, and a stub test that a failed listing opens nothing and the next sync reattaches.
- Negative: while another app's panel shows a stream, this viewer's panels open at most every 30 s, and not at all while it lasts.
- Watch: relax the retry once the `qaros` crate checks the content type before reading the URI.

## Evidence and sources

- [PN] `crates/qaros-sink/src/panels.rs` (`QarosPanels::sync`, `listing_retry_delay`, `LISTING_RETRY_FAST`, `LISTING_RETRY_SLOW`), `crates/qaros-sink/tests/stub.rs` (`a_failed_panel_listing_opens_nothing_and_the_next_sync_reattaches`).
- [QP] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/crates/qaros/src/panels.rs` L207 (`GuiPanels::list`), L333-337.
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L1150-1154 (`QarGuiPanelContentType`), L4561 (`qar_gui_panel_get_content_type`).
- [KI] [Known issues](../known-issues.md#panel-listing-fails-while-a-panel-shows-a-video-stream).

[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[QP]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/crates/qaros/src/panels.rs "qaros GUI panels"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[KI]: ../known-issues.md#panel-listing-fails-while-a-panel-shows-a-video-stream "Known issues"
