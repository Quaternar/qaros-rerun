---
id: ADR-0080
title: Grab and spread move and scale the scene through QAROS's own rules; the viewer follows
status: proposed
date: 2026-10-31
repo: qaros-rerun
---

# ADR-0080: Grab and spread move and scale the scene through QAROS's own rules; the viewer follows

## Context

In the glasses, Rerun's camera controls become moves of the content: a one-hand grab moves the scene and a two-hand spread scales it (D5, tool 2 of the proof of concept). The viewer creates its app volume without a gesture configuration, so QAROS applies its default rules: one-hand 6DoF to the app pose (translation only) and the two-hand distance to the app scale, folded into the volume's transform by the runtime itself ([HDR] L1437-1523, L5112-5197). ADR-0066 already maps every frame with the transform QAROS holds, but reads it again only on a volume update or every 100 ms, which would move the content in steps during a grab. The `qaros` crate does not wrap app volume gestures.

## Decision

- **The runtime applies the gesture; the viewer applies nothing.** Every mapped event reports `was_mapped_to_app_transform`; applying its delta again would move the content twice ([HDR] `qar_app_volume_gesture_get_deltas`).
- **Gestures are subscribed through the raw table** (`qar_app_volumes_subscribe_all_gesture_events`, every kind on one callback so phases keep their order), as panel gestures are (ADR-0078). The callback copies each event and wakes the viewer; the session's thread drains them.
- **While a gesture runs, the transform is read every frame.** A started or updated grab, spread or rotate keeps the volume in motion until it ends or is cancelled; while one runs, and after any mapped event, `QarosConnection::volume_transform` reads the latest values for the frame being begun instead of waiting for the poll. ADR-0066's per-frame mapping does the rest: depth, near plane and panels stay in room metres at any scale.
- **Every event is logged:** starts, ends and instant events at INFO, updates at DEBUG, with kind, phase, hand, accumulated deltas, action point and whether the runtime applied it. This is the record a headset session reads, and the hook for hover and selection later.

## Alternatives considered

- **Apply the deltas in the viewer, as a camera move.** Duplicates the runtime's work, contradicts `was_mapped_to_app_transform`, and the other users' mixers would show a different placement.
- **A gesture configuration of our own now** (yaw rotation on, a scale clamp, grabs from inside the box only, as D5 proposes). Needs tuning in the headset; the defaults show the tool.
- **Wrap app volume gestures in the `qaros` crate first.** The right home for the wrapper, but a change to QuaternAR's repository; the raw module can move there unchanged.

## Consequences

- Positive: the content follows the hands at the runtime's latency and stays consistent for every user.
- Positive: unit tests cover decoding, the running-gesture tracking and the queue; the stub tests of ADR-0066 cover a transform change reaching the next frame on both frame paths and the render thread.
- Negative: `qaros-stub` publishes no app volume gestures, so the subscription itself is checked only against the real runtime.
- Negative: zoom is unclamped and grabs from outside the box also move the content (default rules); both wait for a headset session.

## Evidence and sources

- [HDR] QAROS C header, `qar-streaming-c/include/qar_streaming.h` at `ebfe3bf`: app volume gestures L4737-4965, gesture rules L1437-1523 and L5112-5197.
- [CODE] `crates/qaros-sink/src/volume_gestures.rs`, `connection.rs` (`volume_transform`), `volume_transform.rs`.

[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[CODE]: ../../crates/qaros-sink/src/volume_gestures.rs "Volume gestures"
