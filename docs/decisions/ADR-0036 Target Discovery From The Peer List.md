---
id: ADR-0036
title: Render targets discovered from the peer list, one sender per running target app
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0036: Render targets discovered from the peer list, one sender per running target app

## Context

At `b1338df`, `rendering-streams.mdx:46` says a target requests content only once a sender has attached it to the app volume, so an app that waits for render requests before creating a sender never streams to a target. The header's "Basic flow" and the "Finding rendering targets" section find targets in the peer list instead ([RS]). The viewer does not rely on a listed peer's state alone to know that it has left, so a peer whose state is anything but running counts as gone.

## Decision

`qaros_sink::TargetPeers` (behind `UserEventSource`) treats a peer for which `qar_peer_spec_is_target_app` is true and whose state is `QAR_APP_STATE_RUNNING` as a user. It subscribes to peer updates first, lists the peers once, then follows every update; every listing applies to every listed peer, and the list is re-read after updates and at least every 2 s. A peer that stops running or leaves the list has left. A render request from an unlisted peer still adds it. A peer the sink released (its sender failed or its stream closed) joins again on the next news about it (the list, an update or a request) once its back-off has passed (ADR-0040). Each peer's state and target flag are logged when they change. The Rust sample `cpu_rendering_visualizer` uses the same discovery.

## Alternatives considered

- Wait for render requests: a target asks for content only after a sender attached it, so no stream ever starts.
- Updates only, without re-reading: a running target can appear in a listing that no update announced, and would never be added.
- Why 2 s for the re-read: rationale not recorded.

## Consequences

- Positive: one and two targets verified headless on the binary package; the viewer (robot arm demo) streams to the visualizer.
- Negative: polling the peer list; whether peer updates announce every running target is not documented.
- Negative: `QarPeerSpec` has no liveness signal, so the viewer does not rely on a target listed as running still answering; ADR-0040 bounds what a target that does not answer costs.

## Evidence and sources

- [RS] QAROS docs at `b1338df`, `docs/website/docs/developer-guide/rendering-streams.mdx`, "Finding rendering targets"; `peers.mdx`, app states.
- [USR] `crates/qaros-sink/src/users.rs` (`TargetPeers`, `PEER_LIST_INTERVAL`, `is_render_target`).

[RS]: https://github.com/Quaternar/qaros/blob/b1338df/docs/website/docs/developer-guide/rendering-streams.mdx "Rendering streams"
[USR]: ../../crates/qaros-sink/src/users.rs "TargetPeers"
