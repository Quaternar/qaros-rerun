---
id: ADR-0074
title: Local-app onboarding is the primary way to join a Hub on the same PC
status: accepted
date: 2026-10-19
repo: qaros-rerun
---

# ADR-0074: Local-app onboarding is the primary way to join a Hub on the same PC

## Context

The viewer runs on Windows, on the machine with the QAROS Hub (the [integration guide](../integration-guide.md#what-runs-where)). QAROS offers three ways in: a pairing code, which is short-lived and spent by one failed attempt, with the onboarding id stored for later rejoins; a launch invite on stdin for an app QAROS launches off Windows; and local enrollment, "on the QAROS PC, no code": the app chains `QarOnboardLocalAppExt`, Windows only, and the Hub asks a person to allow it on its first start ([OS] L24-26). The prompt shows what Windows says about the executable, waits `approval_timeout_ms`, "0 means the hub default of **120 s**", capped at 10 minutes ([OS] L100-123), and a local app's onboarding id is the zero id, with "nothing to persist" ([OS] L93). On Windows an app QAROS launches joins as a local app too (ADR-0035).

## Decision

- `--local-app` joins the Hub on this PC as a local app, with the Hub's default approval wait; nothing is read from stdin and no onboarding id is stored. The README and every demo page start the viewer this way.
- It takes precedence over a launch line, a stored id and a pairing code, and refuses `--pairing-code` and `--onboarding-id-file`, which it would ignore.
- With `--launched-by-qaros` on Windows the viewer joins this way without the flag (ADR-0035).
- The approval wait blocks the start. The log says so before it begins, with the wait's length: `qaros: onboarding as a local app; on its first start the Hub asks to allow it (up to 120 s), and a Stop meanwhile takes effect only once the wait ends`. The wait's cancel token lives on the thread that onboards, and the stdin watcher and the console handler do not hold a handle to it (ADR-0056).

## Alternatives considered

- A pairing code as the first-run path: someone reads the Hub's current code and types it on the command line, where it must not be logged; one failed attempt spends it, and the id file it produces is state tied to the working directory.
- An approval wait of the viewer's own: the Hub's default suits a person at the PC, and the Hub caps longer waits.

## Consequences

- Positive: a first run needs no code and leaves no id file; a person allows the viewer once in the Hub's prompt.
- Negative: Windows only, and only on the Hub's PC, started in the signed-in desktop session (outside it the Hub is not authentic to the library); every viewer executable the Hub has not seen before is asked about again.
- Negative: a Stop during the approval wait takes effect once the wait ends. `qaros::CancelToken::cancel_handle` hands such a cancel to another thread ([CT] L15-19, L117), the way out once the viewer uses it.

## Evidence and sources

- [CN] `crates/qaros-sink/src/connection.rs` (`join_session`, `ConnectOptions::local_app`, `LOCAL_APP_APPROVAL_TIMEOUT`, `HUB_DEFAULT_APPROVAL_TIMEOUT`), `crates/qaros-rerun/src/options.rs` (`--local-app` checks, `QarosOptions::joins_as_local_app`).
- [OS] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/onboarding-and-sessions.mdx` L24-26 ("Local app: on the QAROS PC, no code"), L93, L100-123 (the approval prompt).
- [CT] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/crates/qaros/src/cancel.rs` (`CancelToken::cancel_handle`).
- [IG] `docs/integration-guide.md`, "Troubleshooting" (`ONBOARDING_HUB_NOT_AUTHENTIC`, the approval wait).

[CN]: ../../crates/qaros-sink/src/connection.rs "QAROS connection"
[OS]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/onboarding-and-sessions.mdx "Onboarding and sessions"
[CT]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/crates/qaros/src/cancel.rs "qaros cancel token"
[IG]: ../integration-guide.md#troubleshooting "Integration guide"
