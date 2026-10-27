---
id: ADR-0056
title: A stored onboarding id that cannot be rejoined falls back to the pairing code
status: accepted
date: 2026-10-19
repo: qaros-rerun
---

# ADR-0056: A stored onboarding id that cannot be rejoined falls back to the pairing code

## Context

`connect` joins the session in this order: as a local app (`--local-app`, or `--launched-by-qaros` on Windows, ADR-0074), then with the launch line from stdin (`--launched-by-qaros` elsewhere), then by rejoining with the id in `--onboarding-id-file`, then by onboarding with `--pairing-code` ([CN] `join_session`). A stored id can be unusable in several ways: the file is unreadable or holds no UUID, the runtime no longer knows the id (`ONBOARDING_SESSION_NOT_FOUND`), or the rejoin fails for another reason. A file holding the zero id fails as well: `qaros` refuses to rejoin the zero id before calling the library, and the zero id is what local-app onboarding returns, "nothing to persist" ([OS] L93).

Local-app onboarding blocks while the Hub waits for a person to allow the app, by default up to 120 s ([OS] L121). No session exists during the wait. The wait takes a cancel token on the onboarding thread; the stdin watcher and the console handler run on other threads.

## Decision

- With `--pairing-code`, a stored id that cannot be rejoined (unreadable or damaged file, zero id, any rejoin error) logs a warning naming the reason and onboards with the code, storing the new id. Without a code the start stops with the reason. An id the runtime does not know onboards with the code, or asks for one.
- `--local-app` refuses `--pairing-code` and `--onboarding-id-file`: the mode keeps no onboarding id and takes no code.
- A Stop during the local-app approval wait takes effect once the wait ends; the onboarding log line says so.
- `--launched-by-qaros` with a launch line on stdin logs one line before it reads it.

## Alternatives considered

- Skip the rejoin whenever `--pairing-code` is given: a command line that keeps the code would onboard anew on every start and never use the stored id.
- Cancel the approval wait at Stop from the stdin watcher or the console handler: possible with `CancelToken::cancel_handle`, which the `qaros` crate offers "to cancel from another thread (a Ctrl+C handler, a watcher of stdin)" ([CT] L15-19). Not done yet; with no session during the wait, an exit that waits for the wait to end, or QAROS's kill about 1 s after Stop, loses nothing but a clean exit.

## Consequences

- Positive: a damaged, zero or stale id file does not block a start that carries a code; stub tests cover a damaged file, the zero id and an injected rejoin failure, each storing a new non-zero id, and a damaged file without a code stopping the start.
- Negative: a rejoin that fails for a passing reason while a code is given replaces the stored id with a new one.
- Negative: a Stop during the approval wait is late by up to the wait's length.

## Evidence and sources

- [CN] `crates/qaros-sink/src/connection.rs` (`join_session`, `rejoin_with_stored_id`), `crates/qaros-rerun/src/options.rs` (`--local-app` checks).
- [TS] `crates/qaros-sink/tests/stub.rs` (`a_damaged_onboarding_id_file_falls_back_to_the_pairing_code`, `a_zero_onboarding_id_falls_back_to_the_pairing_code`, `a_failed_rejoin_falls_back_to_the_pairing_code`).
- [OS] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/onboarding-and-sessions.mdx` L93 (a local app's zero id), L121 (the 120 s default).
- [CT] The `qaros` crate at `ebfe3bf`, `qar-streaming-rust/crates/qaros/src/cancel.rs` (`CancelToken::cancel_handle`).

[CN]: ../../crates/qaros-sink/src/connection.rs "QAROS connection"
[TS]: ../../crates/qaros-sink/tests/stub.rs "Sink stub tests"
[OS]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/onboarding-and-sessions.mdx "Onboarding and sessions"
[CT]: https://github.com/Quaternar/qaros/blob/ebfe3bf/qar-streaming-rust/crates/qaros/src/cancel.rs "qaros cancel token"
