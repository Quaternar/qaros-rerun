---
id: ADR-0039
title: Frames begun by polling try_begin_frame once per viewer frame
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0039: Frames begun by polling try_begin_frame once per viewer frame

## Context

The header offers three ways to begin a render frame ([HDR]): the blocking `qar_render_sender_begin_frame` with a cancel token, an async variant `qar_render_sender_begin_frame_async` that reports through a callback, and the non-blocking `qar_render_sender_try_begin_frame`, which starts the begin-frame wait on its first call and polls it on every later one. While a receiver has paused its stream the wait does not complete. The Vulkan cube sample polls `try_begin_frame`, and the viewer's UI thread serves every user in one viewer frame.

## Decision

Both sinks poll `qar_render_sender_try_begin_frame` once per viewer frame for each user without an open frame. Polling is the viewer's only begin mode: `--begin-frame` accepts `poll` alone and refuses `async` and `blocking` with the reason. The `qaros` crate keeps `begin_frame_async` and the blocking `begin_owned_frame` in its API.

A poll that finds no frame leaves the begin-frame wait running in the library, so a sender is destroyed only once its wait has ended: a released user's sender is parked and polled every viewer frame until the wait ends, and the frame it yields is dropped unshown. At Stop the senders with a running wait are polled for at most 500 ms (`BEGIN_WAIT_DRAIN_LIMIT`) before they go; a live receiver's wait ends within a frame interval or two, a paused receiver's costs the whole bound, inside the stop watchdog's 600 ms (ADR-0035). A sender destroyed with its wait still pending crashed the session's teardown on build 2916 ([KI] "Session teardown crash").

## Alternatives considered

- Async begin with a callback: releasing a user with a pending begin means cancelling it and waiting for its callback before the frame state may go. Unbounded, that wait stalls the UI thread on every such release; bounded, a late callback leaves state to leak, and a session with an unanswered begin is leaked at teardown instead of destroyed. On build 2916 a cancelled `begin_frame_async` never called back, which hung the viewer at exit ([KI] "Cancelled async calls").
- Blocking begin: paces the UI thread with several users, and a target that pauses its stream holds the call until it is cancelled, so one paused target stalls every other user and Stop.

## Consequences

- Positive: one begin mechanism on both paths, and no begin can hold the UI thread or the session past a Stop.
- Negative: a frame can start up to one viewer frame later than the async callback would have allowed.
- Negative: a released sender lives on until its wait ends, and a Stop with a paused receiver takes up to 500 ms longer.

## Evidence and sources

- [HDR] QAROS header at `b1338df`, `qar-streaming-c/include/qar_streaming.h` L3031-3085 (`qar_render_sender_begin_frame`, `qar_render_sender_try_begin_frame`, `qar_render_sender_begin_frame_async`).
- [OPT] `crates/qaros-rerun/src/options.rs` (`BeginFrameKind::Poll`).
- [DRAIN] `crates/qaros-sink/src/begin_drain.rs` (`drain_begin_waits`, `BEGIN_WAIT_DRAIN_LIMIT`), `crates/qaros-sink/src/sink.rs` and `crates/qaros-sink/src/vulkan/qaros_stream.rs` (`draining`).
- [KI] [Known issues](../known-issues.md): the teardown crash with a pending begin-frame wait, and cancelled async calls that did not call back.

[HDR]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[OPT]: ../../crates/qaros-rerun/src/options.rs "Viewer options"
[DRAIN]: ../../crates/qaros-sink/src/begin_drain.rs "Begin-frame waits at Stop"
[KI]: ../known-issues.md "Known issues"
