---
id: ADR-0060
title: With decoupled rendering the sink and the QAROS session live on the render thread
status: proposed
date: 2026-10-19
repo: qaros-rerun
---

# ADR-0060: With decoupled rendering the sink and the QAROS session live on the render thread

## Context

The render thread (ADR-0058) must begin, show and abandon frames at its own rate, so it needs the sink. QAROS handles are `!Send` and the SDK is single-threaded per runtime (ADR-0017, the `qaros` crate); the GUI panel hook uses the same session as the render senders. The image sink's pose sources and render policy are not `Send` either.

## Decision

With `--decoupled-render` the whole sink setup (`build_sink`: onboarding, app volume, the viewer's Vulkan device on the session's adapter, the sink, the panel hook) runs on the render thread through the tokio runtime's handle, and the sink and panel hook never leave it. Back to the UI thread go the stop request (`Arc<AtomicBool>`) and the viewer's own Vulkan device, on which eframe then starts; the render thread waits for Rerun's context to exist and takes the device from it. The panel hook runs once per eye frame on the render thread and still syncs once a second. QAROS callbacks wake both the UI and the render thread.

Stop keeps its order: the UI thread sees the stop request, `leave_qaros` stops and joins the render thread, which abandons open frames, drops the sink (senders) and then the panel hook (the last holder of the connection), so the session and the library are released on the thread that created them, and only then drops its own `re_renderer` context; then the process ends as before, under the stop watchdog. A render context that cannot be created on the render thread and a panic anywhere after the sink was built (the renderer's setup, the wait for the device, the render context, the eye loop) end the run the same way: the teardown still runs, and the render thread marks itself failed, which the UI thread treats as a stop request, ending the run with an error. A panic in the teardown is caught as well. The teardown takes the submission lock (ADR-0061) only for a sink that submits outside wgpu, before it unpacks the sink, so a sink dropped while the teardown unwinds still drops under the lock.

## Alternatives considered

- Keep the sink on the UI thread and proxy calls from the render thread: every begin and show would wait for the UI thread, which is what decoupling removes.
- Move only the render senders: they share the session with the panels, so two threads would call into one runtime.
- Make the sink traits `Send`: does not make QAROS handles `Send`.

## Consequences

- Positive: QAROS stays single-threaded; the frame cycle runs at the render thread's rate.
- Positive: the image sink works unchanged, built on the render thread.
- Negative: anything that later calls QAROS from the UI thread would be a second thread on one runtime; today the sink and the panel hook are the only callers.
- Negative: the Stop teardown's wait for begin-frame waits to end (at most 500 ms, ADR-0039) and every sender's device wait run on the render thread inside the UI thread's join, so a Stop stays inside the 1 s watchdog only if the eye frame in progress ends quickly.
- Negative: in decoupled mode the onboarding runs before eframe starts, on the render thread, so its log lines come from another thread.

## Evidence and sources

- [CODE] `crates/qaros-rerun/src/main.rs` (`build_viewer_sinks`), `crates/qaros-rerun/src/decoupled.rs` (`DecoupledHeadset::start`, `tear_down`).
- [DOC] `docs/decoupled-render.md`, "Thread model and hand-off" and "Stop and teardown".

[CODE]: ../../crates/qaros-rerun/src/decoupled.rs "Render thread"
[DOC]: ../decoupled-render.md "Decoupled headset render loop"
