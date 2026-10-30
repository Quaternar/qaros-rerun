---
id: ADR-0079
title: At Stop, stream panels are hidden, then their gestures cancelled and their senders destroyed, all before the session
status: accepted
date: 2026-10-29
repo: qaros-rerun
---

# ADR-0079: At Stop, stream panels are hidden, then their gestures cancelled and their senders destroyed, all before the session

## Context

The whole Stop, from the request to the end of the process, must fit in 600 ms (ADR-0035). A video sender must not outlive its session, which crashed applications on builds up to 3137. A gesture subscription's cancel waits for a running callback ([HDR] L2703-2733). The mixer at `c6e052d3` polls every stream panel's receiver whatever the panel's state; once a stream fails for good, the panel is queued for closing and its browser is destroyed ([BC] L249-264, L1074-1140). Every browser that is not closed owns an Ultralight renderer on that build ([BC] L907-985, [BR] L1851-1873).

## Decision

- **Order.** Inside `QarosPanels::drop`, on the thread that owns the session: hide every held panel (unless `--panels-at-stop keep`), then a teardown step of its own, `ClosePanelStreams`: cancel the gesture subscription, then drop every video sender. The render senders and the sink go before the panel hook, and the hook before the session (`crates/qaros-sink/src/stop_watchdog.rs`, `TeardownStep`).
- **Measured, not assumed.** Every teardown step logs its duration and its offset from the stop request.
- **No in-run revival.** A panel the mixer closes during a run stays closed until the next run claims it (ADR-0075, ADR-0077): a listing cannot tell a mixer's close from a user's ([Quaternar#344]).

## Measured outcome

- **0.9.0-dev.3137.** Stop took 250 to 283 ms end to end; hiding took under 1 ms and closing the panel streams about 2 to 4 ms. About a minute after Stop, when the streams' reconnect window ran out, the mixer closed the hidden panels, destroying their renderers. At the next Start, reviving them built new renderers next to live ones, and the mixer crashed in `WebCore.dll`. No order avoids this on that build: hiding first does not stop the receiver polling, the sender cannot outlive the session, and a stream panel cannot switch to a page.
- **0.9.0-dev.3177.** Two clean restarts with the same order, Stop between 244 and 261 ms, no crash dumps; the mixer shares one renderer, so a panel's close and revival no longer touch shared WebCore state.

## Alternatives considered

- **Destroy the senders before hiding.** No difference on 3137: the mixer closes the panel either way once the stream is gone.
- **Leave the senders to the session's destruction.** The sender-outlives-session crash on builds up to 3137.
- **A black frame before Stop.** It needs a ready begin, and a hidden panel's receiver may report the stream paused instead.

## Consequences

- Positive: the stream panels' teardown costs a few milliseconds of the 600 ms budget, and every step is visible in the log.
- Negative: on builds before 3177 stream panels cannot survive a restart, so 0.9.0-dev.3177 is the minimum the viewer supports (ADR-0076).

## Evidence and sources

- [SW] `crates/qaros-sink/src/stop_watchdog.rs` (`TeardownStep::ClosePanelStreams`), `crates/qaros-sink/src/panels.rs` (`Drop for QarosPanels`).
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L2703-2733 (cancel waits for a running callback), L376-383 (paused, reconnecting and no-transfer statuses).
- [BC] Mixer source at `c6e052d3`, `StereoKitGUI/src/Browser/BrowserControl.hpp` L249-264, L907-985, L1074-1140.
- [BR] Mixer source at `c6e052d3`, `StereoKitGUI/src/Browser/Browser.hpp` L1851-1873.
- [Quaternar#344] A GUI panel failure in one viewer's mixer closes the panel for every viewer.

[SW]: ../../crates/qaros-sink/src/stop_watchdog.rs "Stop watchdog"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[Quaternar#344]: https://github.com/Quaternar/Quaternar/issues/344 "A GUI panel failure in one viewer's mixer closes the panel for every viewer"
