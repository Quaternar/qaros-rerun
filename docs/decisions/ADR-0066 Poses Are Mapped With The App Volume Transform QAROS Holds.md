---
id: ADR-0066
title: Poses are mapped with the app volume transform QAROS holds, followed while the viewer runs
status: proposed
date: 2026-10-20
repo: qaros-rerun
---

# ADR-0066: Poses are mapped with the app volume transform QAROS holds, followed while the viewer runs

## Context

With app content pose space (ADR-0038) the sink maps every eye pose into the room with the volume pose, app pose and app scale, hands near and far back divided by the app scale, and the renderer places Rerun's world and its near plane with the same three values ([CODE] `PoseMapping`, `EyeCamera::new`). All of them came from the placement the viewer requested in `get_or_create` (`--origin`, `--yaw`, `--scale`). QAROS does not keep to that: `get_or_create` hands back an existing volume as it is, and a person can move, turn and scale the volume in the Hub. On 2026-10-20 the volume was dragged in the Hub's App Volumes tab; QAROS rendered with the UI's transform (pose at the origin, app scale 0.001) while the viewer still converted with its own, so eye poses and depth were converted with the wrong scale and the content vanished ([REQ]). The header offers the latest locally known pose, app pose and app scale per volume (`qar_app_volume_get_latest_*`, "fast-path") and an update subscription for volumes, whose callback runs on a library thread ([H]).

## Decision

- **Live at connect:** right after `get_or_create`, `connect` subscribes to app volume updates and reads the volume's latest pose, app pose and app scale (`qaros::AppVolumes::latest_pose`, `latest_app_pose`, `latest_app_scale`). The frames are mapped with them; when they differ from the requested values, both are logged at INFO. A read that fails, or values that cannot place anything (a scale that is not finite and positive), fall back to the requested transform with a warning.
- **Followed while running:** `QarosConnection::volume_transform` returns the transform for the frames begun now. Updates are forwarded from the library thread through a channel, as render requests and peer updates are; on an update for our volume, and at least every 100 ms (`LATEST_VALUES_POLL_PERIOD`) since the header does not say whether the fast-path setters reach the subscription, the latest values are read again on the thread that owns the session (the UI thread, or the render thread with `--decoupled-render`, ADR-0060), so the single-thread rule holds. An update only triggers that read; its own values are never applied, because the Hub delivers updates out of order: on 2026-10-21 the last update carried app scale 1.0 after 0.5 had been set and read back ([LAP]). When the latest values cannot be read, the current transform is kept until a read succeeds. Each change is logged once at INFO with the old and new values; an unusable change is warned about and ignored.
- **Per frame:** both sinks read the transform when they begin a frame, map that frame's poses with it, keep the mapping with the open frame and report near and far at its show with the same scale. `FrameRequest::volume` (and `SenderFrame::volume` on the Vulkan path) carries it to the renderer, which places Rerun's world for that frame in it (`AppPlacement::with_volume_transform`, in `prepare_frame`, which the default and the decoupled path share). A change therefore applies from the next frame begun; a frame in flight finishes with the transform it began with.
- **Our own placement stays ours:** `world_coordinates` and `world_center` keep coming from the view and the options. The yaw from `--yaw` is part of the requested app pose, so once QAROS holds another app pose that yaw is QAROS's too. Panels are still placed beside the requested volume pose.

## Alternatives considered

- Map once at connect with the live values: still wrong after the first drag in the Hub, the case that was seen.
- Take the update subscription's values only: the update carries the handle's stored values, and on 2026-10-20 the listed values were the requested ones while the render path used the UI's ([REQ]); the latest getters are the closer candidate for the render path, and polling them also covers fast-path changes that may not raise an update.
- Set the volume back to the requested transform when it differs: it would fight the person moving the volume, and the Hub kept rendering with the UI's transform after API updates anyway ([REQ]).
- Map with a transform carried by each frame: the C API delivers none; a frame's poses are in app units with no transform beside them.

## Consequences

- Positive: eyes, near and far, linear depth's `depth_scale` and the renderer's near plane use one transform per frame, the one QAROS reports, on both sinks and in both render modes.
- Positive: the log shows the requested and the reported transform at start and every change, which is what the Windows check needs.
- Negative: whether the latest values report the render path's transform after a Hub UI edit, or only the stored ones, is unverified; if only the stored ones, this change keeps the viewer consistent with QAROS's report but not with what QAROS renders, and the question goes to QuaternAR.
- Negative: up to three more C calls every 100 ms on the session's thread, and a change shows up at most 100 ms late when no update is raised.
- Negative: the UI clamps the app scale to at least 0.001, so a scene that needs a smaller scale (air traffic, 1.5e-06) cannot be set from the Hub; the viewer follows whatever QAROS reports.

## Evidence and sources

- [H] QAROS C header, `qar-streaming-c/include/qar_streaming.h` at `ebfe3bf`: room, app volume and app content space; `qar_app_volumes_change_app_scale` and `change_pose` ("low-latency fast-path update"); `qar_app_volumes_subscribe_updates`; `qar_app_volume_get_latest_*`.
- [REQ] Observed 2026-10-20 on a Windows Hub: after the volume was dragged in the Hub's App Volumes tab, QAROS rendered with the UI's transform (app scale 0.001) and the content vanished; the UI clamps the app scale to at least 0.001.
- [LAP] Observed 2026-10-21 with the Rust `app_volume_management` sample on Dev Hub 0.9.0-dev.2916: the last update carried app scale 1.0 after 0.5 had been set and read back.
- [CODE] `crates/qaros-sink/src/volume_transform.rs` (`FollowedVolume`, `LiveVolume`), `crates/qaros-sink/src/connection.rs` (`QarosConnection::volume_transform`), `crates/qaros-sink/src/sink.rs` (`PoseMapping`), `crates/qaros-sink/src/vulkan/qaros_stream.rs` (`try_begin_frame`), `crates/qaros-rerun/src/headset.rs` (`placement_for_request`), `crates/xr-math/src/placement.rs` (`VolumeTransform`).
- [TEST] `crates/qaros-sink/tests/stub.rs` `a_volume_held_with_another_transform_maps_eyes_with_the_live_one`, `an_app_volume_change_mid_run_maps_the_next_frame_with_it`, `an_app_volume_update_is_applied_only_through_a_fresh_read`, `a_vulkan_frame_after_an_app_volume_change_maps_with_it`; `crates/qaros-rerun/src/decoupled.rs` `an_app_volume_change_reaches_the_frames_begun_on_the_render_thread`; `crates/qaros-rerun/src/headset.rs` `a_frame_is_placed_in_the_volume_transform_its_poses_were_mapped_with`; `crates/qaros-sink/src/volume_transform.rs` and `crates/xr-math/src/placement.rs` unit tests.

[H]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[CODE]: ../../crates/qaros-sink/src/volume_transform.rs "Followed app volume transform"
[TEST]: ../../crates/qaros-sink/tests/stub.rs "Stub tests"
