---
id: ADR-0022
title: Frame keys and a frame-number stamp for frame-to-pose pairing
status: accepted
date: 2026-10-06
repo: qaros-rerun
---

# ADR-0022: Frame keys and a frame-number stamp for frame-to-pose pairing

## Context

Every shown frame must be the one rendered for the pose QAROS issued for it. QAROS frames carry no number and `show_frame` takes the sender, so pairing relies on call order. With asynchronous readback and several users, an off-by-one or a swap between users raises no error and shows only as swimming in the glasses, never in the visualizer.

## Decision

- Every frame is keyed by user and a per-user frame id (`FrameKey`) from `begin_frames` to `show_frame`; readbacks carry the key, the sink keeps one open QAROS frame per user and shows only the key it issued; mismatches are abandoned. Frame ids count frames the sender delivered.
- `--frame-stamp` draws `U<user> F<frame> L|R` plus a machine-readable strip into each eye, on the CPU path and on the GPU path (a stamp texture over the top rows, at 1 m depth); `qaros-stub` decodes the strip at every `show_frame` and logs `paired` or `ERROR pairing`. Readbacks are identified per prepared frame. A peer that leaves and joins again comes back as a new user id (`PeerUsers` in `qaros-sink`), also when both happen within one poll, so a frame key is never reused.

## Alternatives considered

- Trust call order: silent failure mode.
- Pass the issued poses back as view overrides: the documentation does not say this is allowed.

## Consequences

- Positive: 160 of 160 frames paired against the multi-peer stub with a user joining and one leaving.
- Negative: QAROS's test extension delivers identity poses, so the pairing evidence here is the stub run.
- Negative: the stamp format exists twice (encoder here, decoder in `qaros-stub`) and must be mirrored; the stub decodes only CPU senders.

## Evidence and sources

- [FK] `crates/frame-sink/src/lib.rs` (`FrameKey`), `crates/frame-sink/src/frame_stamp.rs`; `crates/qaros-sink/src/users.rs` (`PeerUsers`).
- [EV] `docs/evidence/multi-user/stub-two-users-report.txt`, recorded with an earlier begin mode (`render_sender_begin_frame_async`); pairing does not depend on the begin mode (ADR-0039).
- [README] `README.md`, "Two users, frame stamp on".

[FK]: ../../crates/frame-sink/src/frame_stamp.rs "Frame stamp"
[EV]: ../evidence/multi-user/stub-two-users-report.txt "Two users against the stub"
[README]: ../../README.md "qaros-rerun README"
