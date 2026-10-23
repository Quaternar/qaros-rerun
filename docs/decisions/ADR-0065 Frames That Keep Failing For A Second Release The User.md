---
id: ADR-0065
title: Begins and shows that keep failing for a second release the user; an unresolved app volume is not a failure
status: proposed
date: 2026-10-20
repo: qaros-rerun
---

# ADR-0065: Begins and shows that keep failing for a second release the user; an unresolved app volume is not a failure

## Context

A user whose frames keep failing holds a sender and a slot that another target could use, so the sink releases it and lets it join again after a back-off (ADR-0040). A limit counted in calls depends on the caller's rate: with `--decoupled-render` the render thread begins frames far faster than the UI pass does (ADR-0058), so a fixed number of failures can pass in well under a second. Not every error is lasting: `QAR_STATUS_RENDERING_PRODUCER_APP_VOLUME_UNRESOLVED` is recoverable, "keep calling, a frame begins once the volume resolves", each call held about 16 ms first ([HDR] `qar_render_sender_begin_frame`, `qar_render_sender_try_begin_frame`). A user released for it while its volume resolves late loses its sender and waits out a back-off. On the CPU path a failed show must count too, or a sender whose shows keep failing holds its slot for good.

## Decision

- **Time and count:** failures of a user's begins and, on the CPU path, its shows are counted in a run that a shown frame ends. The user is released with `ReleaseCause::Failed` once the run has lasted `FRAME_FAILURE_PERIOD` (1 s) from its first failure and holds at least `FRAME_FAILURE_MINIMUM` (10) failures, so a few failures spread over a pause of the viewer are not taken for a lasting error. The first failure of a run is a warning, the rest are debug lines, and the release names the run's length and count (`qaros: frames for user-N kept failing for ..., N in a row (...); releasing it`).
- **Unresolved app volume:** a begin that fails with `QAR_STATUS_RENDERING_PRODUCER_APP_VOLUME_UNRESOLVED` (`qaros::StatusCode::RENDERING_APP_VOLUME_UNRESOLVED`) is treated as not ready, on both paths: it neither counts nor ends a run, and it is warned once per user until a frame of that user is shown (`qaros: the app volume is not resolved for user-N yet; its frames begin once it is`). The stall rule in `TargetPeers` still applies, since no frame begins: a peer that joined by its own request and gets no frame begun in 10 s (`STALL_TIMEOUT`) is taken for gone, a listed target only while another user waits for its slot.
- **Shows:** a CPU show that fails with a closed stream releases the user at once; any other show failure counts like a begin failure. The Vulkan path releases at the first failed show, since that frame's semaphore was signalled already.

## Alternatives considered

- A count of failures, raised for the render thread: the right number still depends on the rate, which differs per machine, mode and scene.
- Time alone: two failures a second apart with a pause of the viewer between them would release a working user.
- Count `APP_VOLUME_UNRESOLVED` with a longer period: the header promises the frame begins once the volume resolves, and a removed volume never resolves whatever the period; telling the two apart needs the volume list, a separate change.

## Consequences

- Positive: the release takes about a second at any begin rate, and a sender whose shows keep failing on the CPU path gives its slot up.
- Positive: a volume that resolves late does not cost the user its sender and a back-off.
- Negative: a removed app volume keeps its users' senders until they stall or the peer leaves; the warning is the only sign.
- Negative: at a begin rate below 10 per second the release takes longer than a second.

## Evidence and sources

- [HDR] QAROS header at `b1338df`, `qar-streaming-c/include/qar_streaming.h` L359-361 (the status, recoverable), L3031-3074 (`qar_render_sender_begin_frame`, `qar_render_sender_try_begin_frame`).
- [CODE] `crates/qaros-sink/src/users.rs` (`PeerUsers::frame_failed`, `PeerUsers::volume_unresolved`, `FRAME_FAILURE_PERIOD`, `FRAME_FAILURE_MINIMUM`, `STALL_TIMEOUT`), `crates/qaros-sink/src/sink.rs` (`QarosSink::frame_failed`), `crates/qaros-sink/src/vulkan/qaros_stream.rs` (`try_begin_frame`).
- [TEST] `crates/qaros-sink/src/users.rs` `a_user_whose_frames_keep_failing_for_a_second_is_given_up_unless_a_frame_shows`; `crates/qaros-sink/tests/stub.rs` `a_user_whose_begins_keep_failing_is_released`, `a_user_whose_shows_keep_failing_is_released`, `begins_refused_for_an_unresolved_app_volume_do_not_release_the_user`, `a_vulkan_user_whose_begins_keep_failing_is_released`.

[HDR]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[CODE]: ../../crates/qaros-sink/src/users.rs "User lifecycle"
[TEST]: ../../crates/qaros-sink/tests/stub.rs "Stub tests"
