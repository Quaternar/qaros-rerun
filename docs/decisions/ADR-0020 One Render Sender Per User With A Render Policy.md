---
id: ADR-0020
title: One render sender per user, with a render policy that caps users
status: accepted
date: 2026-10-06
repo: qaros-rerun
---

# ADR-0020: One render sender per user, with a render policy that caps users

## Context

Several people in a QAROS session may each want the Rerun scene in their own glasses, and the application must decide whether to render for each of them. The public header has no user concept, no user event and no user id; a mixer runs per user.

## Decision

Each user gets a render sender and a camera of their own. `frame-sink` defines `UserId`, `FrameKey`, `UserEvent`, a `UserRoster`, and a `RenderPolicy` trait whose default `MaxUsers` renders up to `--max-users` (default 2) and admits a waiting user when one leaves. Users come from a `UserEventSource` trait so a future user event can replace the discovery mechanism. On the QAROS sinks a user takes a slot once its sender exists, not when it joins; the policy is asked then (`UserRoster::would_render`). The headset renderer draws every rendered user's two eyes per viewer frame from the same draw data; a late readback for one user does not hold up others. A departed user's sender and open frame are dropped. Both the CPU and the Vulkan path give each user a render sender of its own.

## Alternatives considered

- One sender for all targets: the header gives no way to address several targets from one sender.

## Consequences

- Positive: two senders on one session stream pixel-exact to two targets on the Windows QAROS package.
- Positive: the trait seam isolates the discovery mechanism (ADR-0036).
- Negative: per-user render cost scales linearly in one viewer frame; on a Windows laptop two targets at 1832x1920 reach 18.7 fps each on the Vulkan path.

## Evidence and sources

- [USERS] `crates/frame-sink/src/users.rs` (`UserRoster`, `RenderPolicy`, `MaxUsers`); `crates/qaros-sink/src/users.rs`.
- [EV] `docs/evidence/multi-user/` (two users against `qaros-stub`).
- [README] `README.md`, "Two users, frame stamp on", "Stream rate per user".

[USERS]: ../../crates/frame-sink/src/users.rs "Users and render policy"
[EV]: ../evidence/multi-user "Multi-user evidence"
[README]: ../../README.md "qaros-rerun README"
