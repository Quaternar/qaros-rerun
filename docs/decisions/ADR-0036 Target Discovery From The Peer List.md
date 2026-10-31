---
id: ADR-0036
title: Render targets discovered from the peer list, one sender per present target app
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0036: Render targets discovered from the peer list, one sender per present target app

## Context

Rendering Streams says a target requests content only once a sender has attached it to the app volume ([RS] L77), so an app that waits for render requests before creating a sender never streams to a target. The header's "Basic flow" and the "Finding rendering targets" section find targets in the peer list instead: a peer whose role is `QAR_PEER_ROLE_TARGET` and whose presence is `QAR_PEER_PRESENCE_ONLINE`, or `QAR_PEER_PRESENCE_UNKNOWN` on a hub that has not reconciled it ([HDR] L68-73, [RS] L21). Presence carries "the hub's own liveliness evidence" and the leave a peer announces ([HDR] L552-579). The app state is deprecated and computed from the presence: offline and leaving peers read as `QAR_APP_STATE_SHUTTING_DOWN`, retired ones as `QAR_APP_STATE_RETIRED`, online and unreconciled ones as running ([HDR] L533-538, L3585-3593). The `qaros` crate exposes the app state on `PeerInfo` and the presence through its raw table only ([PI] L14-21).

## Decision

`qaros_sink::TargetPeers` (behind `UserEventSource`) treats a peer for which `qar_peer_spec_is_target_app` is true and whose app state reads `QAR_APP_STATE_RUNNING` as a user. Through the computed app state this is the header's rule: a target app whose presence is online or unknown. It subscribes to peer updates first, lists the peers once, then follows every update; every listing applies to every listed peer, and the list is re-read after updates and at least every 2 s. A peer that reads anything but running (offline, leaving, retired) or leaves the list has left. A render request from an unlisted peer still adds it. A peer the sink released (its sender failed or its stream closed) joins again on the next news about it (the list, an update or a request) once its back-off has passed (ADR-0040). Each peer's state and target flag are logged when they change. The Rust sample `cpu_rendering_visualizer` uses the same discovery.

## Alternatives considered

- Wait for render requests: a target asks for content only after a sender attached it, so no stream ever starts.
- Updates only, without re-reading: a running target can appear in a listing that no update announced, and would never be added.
- Why 2 s for the re-read: rationale not recorded.

## Consequences

- Positive: one and two targets verified headless on the binary package; the viewer (robot arm demo) streams to the visualizer.
- Negative: polling the peer list; whether peer updates announce every running target is not documented.
- Negative: a target that is online but does not answer still costs a sender creation; ADR-0040 bounds that cost. On builds without presence, a target that had ended stayed listed as running ([KI]).

## Evidence and sources

- [RS] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/rendering-streams.mdx` L16-21 ("Finding rendering targets"), L77.
- [USR] `crates/qaros-sink/src/users.rs` (`TargetPeers`, `PEER_LIST_INTERVAL`, `is_render_target`).
- [HDR] QAROS header at `ebfe3bf`, `qar-streaming-c/include/qar_streaming.h` L68-73 ("Basic flow", step 5), L533-538 (`QarAppState` deprecated), L552-579 (`QarPeerPresence`), L3585-3593 (`qar_peer_spec_get_app_state`).
- [PI] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/crates/qaros/src/peers.rs` L14-21 (`PeerInfo::app_state`).
- [KI] [Known issues](../known-issues.md#ended-target-apps-listed-as-running).

[RS]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/rendering-streams.mdx "Rendering streams"
[USR]: ../../crates/qaros-sink/src/users.rs "TargetPeers"
[HDR]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[PI]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/crates/qaros/src/peers.rs "qaros PeerInfo"
[KI]: ../known-issues.md#ended-target-apps-listed-as-running "Known issues"
