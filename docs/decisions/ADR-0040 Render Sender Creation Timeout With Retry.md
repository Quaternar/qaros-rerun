---
id: ADR-0040
title: Render sender creation times out after 10 s and the target is retried
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0040: Render sender creation times out after 10 s and the target is retried

## Context

`qar_render_sender_create` negotiates the stream with its target and takes an optional cancel token ([HDR]); it returns once the target answers, and the docs do not say what it waits for or how long that can take. Without a cancel token, a creation for a listed target that was not receiving did not return for minutes. Peer presence drops a target the hub no longer sees (ADR-0036), but a target that is online and does not answer still has to be bounded, and on build 2916, without presence, ended targets stayed listed as running ([KI] "Ended target apps listed as running"). QAROS handles are bound to the thread that created them (ADR-0017), so creation cannot move to a thread of the viewer's own; the header offers `qar_render_sender_create_async` instead.

## Decision

Both sinks create senders in the background with `qar_render_sender_create_async` and a library cancel token that times out after 10 s (`SENDER_CREATION_TIMEOUT`); the viewer is woken when a creation finishes.

- **Hedged creation:** a target that joins becomes a candidate, and senders are created for the candidates in parallel, up to `HEDGED_CREATION_LIMIT` (8). Candidates that asked for a stream come first, then the others in the order they joined. A candidate takes a slot of the `UserRoster` only once its sender exists: the first created get the free slots, and a sender created when every slot is taken goes again, its candidate waiting for a slot. A creation that runs therefore holds no slot, and a target that does not answer costs its timeout in the background, not a live target's wait.
- **Back-off after a failure:** a target whose sender failed may join again, whatever brings news about it (the peer list, a peer update or a render request), after 2 s, doubling up to 60 s while it keeps failing, the approach of the Rust `vulkan_cube` sample. During the first 30 s of the run the delay does not double, since a first creation can fail while QAROS is still setting the session up. A stream that starts resets the delay (`RetryPlan` in `users.rs`).
- **Unanswered targets:** a target whose creation timed out waits 60 s at once. An unchanged running listing is no news about it; a render request from it is, and so is a change of its listed state or a new version id, which lift the wait. A timeout in the first 30 s of the run is retried after 2 s instead.
- **Memory across runs:** a timed-out target is remembered for 10 minutes (`UNANSWERED_MEMORY`) in a file next to the onboarding id (`qaros-rerun.unanswered-peers.txt`), so later runs hold it back too. A held-back target is still tried in the background at most once a minute, outside the hedged limit, and takes a slot only when one remains for every other candidate; it is forgotten once it answers, asks for a stream, changes its listed state or is listed under another version id. A creation begun before its target was listed under a new version id says nothing about the restarted app.

## Alternatives considered

- Blocking `qar_render_sender_create` on the UI thread with the same timeout: every unanswering target stalls the viewer for up to 10 s per attempt.
- One creation at a time, in join order: a target that does not answer delays every target behind it.
- Retry every listed target at every start: a target that stays listed without answering costs every run its timeout before the live ones stream.
- A shorter or longer timeout than 10 s: a healthy creation takes about 1 s (1.3 to 1.4 s for the first sender of a run), and a freshly started mixer's first sender took about 21 s ([KI] "Slow first sender on a fresh mixer"). 10 s leaves a healthy creation ample margin and bounds a silent target, and a timeout in the first 30 s is retried after 2 s in the background, so the slow first sender still arrives without a long timeout for everyone.

## Consequences

- Positive: no creation blocks the UI thread, and live targets stream while creations for silent ones run out.
- Negative: up to 8 creations run at once, each holding library resources until it resolves.
- Negative: a target that comes back under the same listing within 10 minutes waits up to a minute for its background retry, unless it asks for a stream.
- Negative: the file of unanswered targets is state that outlives the run; deleting it clears the memory. With presence, it covers only targets that are online and do not answer; it can go once the minimum supported QAROS build has presence.

## Evidence and sources

- [HDR] QAROS header at `b1338df`, `qar-streaming-c/include/qar_streaming.h` L2924-2960 (`qar_render_sender_create`, `qar_render_sender_create_async`).
- [CON] `crates/qaros-sink/src/connection.rs` (`SENDER_CREATION_TIMEOUT`, `start_render_sender`).
- [USR] `crates/qaros-sink/src/users.rs` (`PeerUsers`, `HEDGED_CREATION_LIMIT`, `RetryPlan`, `TargetPeers`).
- [UNA] `crates/qaros-sink/src/unanswered.rs` (`UnansweredPeers`, `UNANSWERED_MEMORY`), `crates/qaros-rerun/src/options.rs` (`UNANSWERED_PEERS_FILE_NAME`).
- [KI] [Known issues](../known-issues.md): ended target apps listed as running, the slow first sender on a fresh mixer, cancelled async calls.

[HDR]: https://github.com/Quaternar/qaros/blob/b1338df/qar-streaming-c/include/qar_streaming.h "QAROS C header"
[CON]: ../../crates/qaros-sink/src/connection.rs "Connection"
[USR]: ../../crates/qaros-sink/src/users.rs "Users from the peer list"
[UNA]: ../../crates/qaros-sink/src/unanswered.rs "Unanswered targets"
[KI]: ../known-issues.md "Known issues"
