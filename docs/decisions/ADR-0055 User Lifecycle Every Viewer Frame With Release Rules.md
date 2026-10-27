---
id: ADR-0055
title: Users are updated every viewer frame and released when they stall, do not answer or keep failing
status: accepted
date: 2026-10-19
repo: qaros-rerun
---

# ADR-0055: Users are updated every viewer frame and released when they stall, do not answer or keep failing

## Context

Users come from `TargetPeers` (ADR-0036), one render sender each, capped by `--max-users` (ADR-0020). A sender is created in the background with a 10 s timeout (ADR-0040). Not every listed target is worth a slot: a target can be listed without answering sender creation (on builds without peer presence, an ended target stayed listed as running, [KI]), a target whose sender was created can stop asking for frames, a begin can keep failing with something other than a closed stream, and a Vulkan frame whose record cannot be read leaves its sender waiting on a semaphore nobody signals. A target that stops asking is not always gone: a receiver pauses its stream while its display is not visible, and the docs say to "Keep the sender and skip the frame; it resumes when the receiver does" ([TR] L46). Frames are begun from the 3D view, which may not be drawn in every viewer frame.

## Decision

- **Every viewer frame:** `FrameSink::update_users` and `GpuFrameSink::update_users` apply joins, leaves and finished or abandoned creations. The headset renderer calls it after the UI pass when the pass did not begin frames (which applies them itself), so it runs once per viewer frame whether or not a 3D view is drawn. Frames are still begun and rendered from the 3D view.
- **Release causes:** a release from the sink's side carries a `ReleaseCause`. `Unanswered` (a creation that timed out or was given up, a stall) gives the peer the longest back-off, 60 s, at once; a timeout in the first 30 s of the run is retried after 2 s instead (ADR-0040). An unchanged running listing does not lift it; a render request from the peer or a change of its listed state does. `Failed` (anything else) keeps the back-off of ADR-0040: 2 s during the first 30 s, then doubling up to 60 s, not lifted by requests.
- **Stall:** a user with a started stream for which no frame began in 10 s (`STALL_TIMEOUT`) has stalled. A stalled requester (a peer that joined by its own render request) leaves. A stalled listed target leaves only while a slot is wanted, that is when the first candidate without a running creation would get no slot; otherwise it keeps its sender, which `try_begin_frame` keeps not ready through a pause until the receiver resumes ([HDR] `QAR_STATUS_MEDIA_STREAMING_STREAM_IS_PAUSED`, `qar_render_sender_try_begin_frame`). A peer that leaves this way waits the `Unanswered` back-off but is not remembered across runs; a sender whose begin-frame wait still runs is drained before it goes (ADR-0039). The check runs just before frames are begun (`UserEventSource::leave_stalled`), and a gap between checks longer than 1 s (or half the timeout) counts only that threshold, so viewer frames without a drawn 3D view do not count.
- **Failing frames:** begins and shows that keep failing release the user with `Failed` (ADR-0065).
- **Vulkan record:** a begun frame whose `qar_render_sender_frame_vulkan` record cannot be read is dropped and its user's sender released with `Failed`.

## Alternatives considered

- Release on the first begin error: a transient error would cost the user its stream and a back-off.
- Count the stall clock in wall time: a 2D-only layout or a collapsed 3D tab would release every user after 10 s.
- Release every stalled target: a headset taken off or a minimised visualizer would lose its sender and renegotiate when it resumes, which the docs advise against.
- Let every render request lift any back-off: a target whose creations fail quickly and that keeps requesting would take a slot at each request.

## Consequences

- Positive: a target listed as running that does not answer costs a background creation at most once a minute, not a slot; a live peer's own request still admits it at once.
- Positive: joins, leaves and creations do not depend on a drawn 3D view.
- Positive: a headset taken off or a minimised visualizer keeps its sender while nobody waits, and its frames resume at the next begin once it resumes.
- Negative: a target slow to issue its first frames after its stream starts (over 10 s while the viewer asks) is released when a user waits for its slot, and then waits 60 s unless it requests again.
- Negative: a dead target that answered its creation keeps its slot until another user wants it.

## Evidence and sources

- [CODE] `crates/qaros-sink/src/users.rs` (`ReleaseCause`, `RetryPlan`, `TargetPeers::leave_stalled`, `STALL_TIMEOUT`), `crates/qaros-rerun/src/headset.rs` (`update_users_unless_begun`), `crates/qaros-sink/src/vulkan/qaros_stream.rs` (`try_begin_frame`).
- [TEST] `crates/qaros-sink/tests/stub.rs`: `a_creation_that_times_out_holds_the_target_back_until_it_asks_for_a_stream`, `a_stalled_requester_is_taken_for_gone_and_a_listed_target_only_for_a_waiting_user`, `a_paused_stream_keeps_its_sender_until_a_user_waits_for_its_slot`, `user_updates_admit_and_release_users_without_beginning_frames`, `a_vulkan_frame_whose_record_cannot_be_read_releases_the_users_sender`.
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` (`QAR_STATUS_MEDIA_STREAMING_STREAM_IS_PAUSED`, `qar_render_sender_try_begin_frame`).
- [TR] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/troubleshooting.mdx` L46.
- [KI] [Known issues](../known-issues.md#ended-target-apps-listed-as-running).

[CODE]: ../../crates/qaros-sink/src/users.rs "User lifecycle"
[TEST]: ../../crates/qaros-sink/tests/stub.rs "Sink stub tests"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[TR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/troubleshooting.mdx "Troubleshooting"
[KI]: ../known-issues.md#ended-target-apps-listed-as-running "Known issues"
