---
id: ADR-0071
title: The head that places panels is a headset's, chosen by the peer's device category, with up to 60 s of waiting for one
status: accepted
date: 2026-10-26
repo: qaros-rerun
---

# ADR-0071: The head that places panels is a headset's, chosen by the peer's device category, with up to 60 s of waiting for one

## Context

New panels open within reach of one head near the app volume (ADR-0070), and the first head to stream is often not a headset's. The Hub's Display Receiver on the PC, a Visualizer receiver, is a target app too: it streams the app volume and its virtual camera can stand near the volume, but nobody can reach a panel placed by it. Panels placed by it stay there across runs (ADR-0070).

The header names what kind of device a peer is: `qar_peer_spec_get_category` returns `QAR_DEVICE_CATEGORY_HEADSET` for "a headset or glasses of any model" and `QAR_DEVICE_CATEGORY_VISUALIZER` for "a Visualizer receiver showing streams on the PC" ([HDR] L605-630, L3667-3697). The `qaros` crate reads category and device name only through its raw table. A target can refine its device identity once the device reports its model, so the category read at join time may still be `UNKNOWN`. A run without glasses still needs panels.

## Decision

- Each streaming user's sink records the user's peer. While no head places panels, the viewer asks QAROS for the device category and name of each streaming viewer near the volume, again at most every 1 s (`DEVICE_LOOKUP_INTERVAL`) until it says headset ([PD] `read_peer_device`).
- A headset's head near the volume places the panels at once.
- Any other viewer near the volume (a Visualizer receiver, or a device whose category QAROS cannot say) places them only once no headset has done so within 60 s (`HEADSET_WAIT`) of the first head near the volume.
- The log names the viewer, its peer and its device, for example `new panels open within reach of user-2 (peer ..., headset "...")`, and says once per viewer when one near the volume is no headset and panels wait.

## Alternatives considered

- The first streaming head, whatever its device: the Display Receiver usually streams first, and the panels then open in front of a virtual camera.
- Headsets only, with no fallback: a run with only the Display Receiver would never open panels.
- A shorter wait: a headset taken out of its case after the viewer started has little time to join before the receiver wins; 60 s covers putting the glasses on.
- The device name or family instead of the category: names change with every model, and the header keeps the category for "a new kind of device", never a new model ([HDR] L605-612).

## Consequences

- Positive: with glasses in the session, panels open within reach of the glasses' wearer; unit tests cover a headset placing at once, a Display Receiver only after the wait, a distant receiver camera never, and the log line.
- Negative: a run without glasses opens its panels a minute after the first viewer arrives.
- Negative: one peer list query per viewer and second while panels wait; it stops once a head places them.

## Evidence and sources

- [PN] `crates/qaros-sink/src/panels.rs` (`places_panels`, `HEADSET_WAIT`, `DEVICE_LOOKUP_INTERVAL`, `viewer_device`, `describe_viewer`).
- [PD] `crates/qaros-sink/src/peer_devices.rs` (`PeerDevice`, `read_peer_device`).
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L605-630 (`QarDeviceCategory`), L3667-3697 (`qar_peer_spec_get_category`).

[PN]: ../../crates/qaros-sink/src/panels.rs "QAROS panels"
[PD]: ../../crates/qaros-sink/src/peer_devices.rs "Peer devices"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
