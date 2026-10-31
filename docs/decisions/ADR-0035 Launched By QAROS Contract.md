---
id: ADR-0035
title: Follow QAROS's contract for API apps it launches
status: accepted
date: 2026-10-13
repo: qaros-rerun
---

# ADR-0035: Follow QAROS's contract for API apps it launches

## Context

QAROS launches API apps from the visualizer's Source Applications panel ([OS] L595-619). On Windows a launched app "joins as a local app, exactly as when a user starts it. The hub recognizes the process it started and gives it the peer id it planned. The approval prompt applies the same way"; the app onboards with `QarOnboardLocalAppExt` and prompts for nothing. Where local enrollment is not available (not Windows), QAROS writes a one-line JSON launch invite as the first line of stdin instead. QAROS sets no environment variable, the app creates its own app volume, and stdin is a pipe that QAROS closes on Stop: the app treats the end of stdin as a request to stop, and "QAROS kills the process when it has not exited about 1 s after closing stdin" ([OS] L611, [HUB] L61). An app that exits within 10 s of starting is restarted with backoff ([OS] L613).

## Decision

- **Joining.** With `--launched-by-qaros` on Windows the viewer joins as a local app, as `--local-app` does (ADR-0074; `QarosOptions::joins_as_local_app` in `crates/qaros-rerun/src/options.rs`), and logs `qaros: launched by QAROS on Windows; joining as a local app`; it reads nothing from stdin. On other platforms the first stdin line starting with `{` is deserialized with `qar_onboarding_invite_deserialize` and onboarded with; anything else is a code. The invite is used for that start only: never stored, never logged, no rejoin.
- **Stop.** End of stdin closes the app, windowed or headless. Stdin is watched from the start (after the launch line, where there is one), so a Stop during onboarding ends the app as soon as onboarding returns; a Stop during the local-app approval wait takes effect once that wait ends (ADR-0056). On Stop the viewer leaves QAROS first (senders, panels, session, runtime, library) and then exits without its own teardown.
- **Exit without unload code.** On Windows that exit flushes the frame timing log, the logger, stdout, stderr and the C runtime's streams, then ends the process with `TerminateProcess`, so DLLs still mapped run no unload code; the calling thread then parks until the termination takes it, and only a failed `TerminateProcess` falls back to `std::process::exit`, with an error line. Elsewhere it calls `std::process::exit`. On Windows `std::process::exit` is no substitute: it flushes only Rust's stdout (`rt::cleanup`) and then calls `ExitProcess` ([RX]), which runs the detach code and static destructors of every DLL still mapped ([EP]), code the viewer neither controls nor needs once QAROS is torn down. On build 2916 that code raised a pure virtual call in `qar-streaming-c.dll` ([KI] "Pure virtual call"). Every other end of a viewer with `--sink qaros` takes the same path once the viewer has run: a closed window, `--quit-when-done`, and an error after the sink was built, with exit code 0, or 1 for an error printed as `main` would. The app's QAROS state (sink, panels, connection) is dropped first. What can fail without QAROS (the async runtime handle, the gRPC address) is checked before the library is loaded ([EX]). The QAROS library stays mapped for the life of the process. `QAROS_STORAGE_DIR` moves the runtime state.
- **Console events.** Ctrl+C, Ctrl+Break and a closed console (its Close button, or End Task in Task Manager) stop a viewer with `--sink qaros` as a QAROS Stop does, launched by QAROS or not ([CC]). Before the library is loaded the viewer installs a console control handler (`SetConsoleCtrlHandler`) that sets the stop request and wakes the viewer, so the UI loop leaves QAROS and ends the process through `end_process_after_teardown`; the list's default handler calls `ExitProcess` ([HR]), which runs the unload code of the mapped library. Any second event ends the process at once with code 1, however soon after the first: no launcher in this repository forwards Ctrl+C, so a quick second press is deliberate. Windows ends the process 5 s after a close event once the handler returns ([HR] "Timeouts"), so for a close the handler does not return: it waits 4 s for the teardown, then ends the process itself. Both forced ends go through `end_process_after_bounded_flush`: the log line, the flush of the frame timing log, the logger and C stdio run on a helper thread that gets at most 200 ms (`BOUNDED_FLUSH_WAIT`), then `TerminateProcess` runs whatever the flush did, because a hung teardown can hold one of those locks and a flush on the handler thread would hang with it. Logoff and shutdown events, which reach services only ([HR]), take the close path. Elsewhere SIGINT keeps its default, which runs no unload code, and QAROS runs on Windows only. The demo launcher, which gets the same Ctrl+C from the console, waits up to 5 s for the viewer to end itself before it terminates the viewer.
- **Deadline.** Every stop request has a deadline of the viewer's own ([WD]): QAROS's Stop, Ctrl+C, Ctrl+Break and a closed console set the request through one function, which also starts a watchdog thread, once per process. If the process still runs 600 ms after the first request (`STOP_DEADLINE`), the watchdog logs one line naming the teardown step in progress (not started, leave sink, hide panels, close volume, session destroy, library destroy, end process) and the time since the stop request as measured when it wakes, in ms, flushes through `end_process_after_bounded_flush` (at most 200 ms) and terminates with code 1. The teardown code keeps the step in an atomic as it goes. Watchdog and flush end by about 800 ms, inside QAROS's kill about 1 s after closing stdin, so a hung teardown leaves the hung step and the exit line in the log rather than losing them to the kill. The 500 ms drain of begin-frame waits at Stop (ADR-0039) fits inside the deadline. Stops that hung in the QAROS teardown on build 2916 are why the watchdog exists ([KI] "Cancelled async calls"). The closed-console grace of 4 s stays as a fallback for a watchdog thread that could not start.

## Alternatives considered

- Read a launch invite from stdin on Windows too: QAROS writes none there, so the viewer would wait for a line that never comes until Stop closes stdin.
- Require `--local-app` beside `--launched-by-qaros` on Windows: one more argument for the operator to enter in Source Applications, and a viewer that hangs at start when it is forgotten.
- Store the onboarding id from a launch and rejoin: the contract gives every launch its own way in, and a local app's onboarding id is the zero id, with nothing to store ([OS] L93).
- End with `std::process::exit` after the teardown: on Windows it runs the unload code of every mapped DLL ([EP]), which the viewer does not need and cannot bound.

## Consequences

- Positive: the viewer playing the robot arm demo starts from Source Applications with `--sink`, `qaros`, `--launched-by-qaros` on every platform QAROS launches it on; onboarding with an invite takes 55 to 60 ms.
- Positive: every viewer exit on Windows, Ctrl+C included, skips the unload code of the DLLs it has mapped.
- Negative: on its first launch the Hub asks a person to allow the viewer, as for any local app; a Stop during that wait is late by up to the wait's length, and QAROS's kill ends it.
- Negative: the `fflush(NULL)` before `TerminateProcess` reaches only the C runtime the viewer links, the shared ucrt (`ucrtbase.dll`) of Rust's default dynamic CRT on MSVC ([EX]). A DLL linked with a static CRT keeps its own stdio buffers and loses its buffered console output.
- Negative: the real console event is not exercised by a test; the stop request it maps to, the force rule, the stdin path, the watchdog's line and the teardown steps are.
- Negative: a teardown that needs longer than 600 ms (a slow but finishing session destroy) is cut short by the watchdog where it would have finished; QAROS's kill would have cut it at about 1 s anyway.

## Evidence and sources

- [OS] QAROS docs at `ebfe3bf`, `docs/website/docs/developer-guide/onboarding-and-sessions.mdx` L93 (a local app's zero id), L595-619 ("Launched by QAROS: QAROS API apps", "Platforms without local enrollment").
- [HUB] QAROS docs at `ebfe3bf`, `docs/website/docs/operator-guide/running-the-hub.md` L61 (Stop kills after about 1 s).
- [KI] [Known issues](../known-issues.md): the pure virtual call at exit, and Stops that hung in the teardown.
- [VP] `docs/vulkan-path.md`, "Launched by QAROS".
- [WD] `crates/qaros-sink/src/stop_watchdog.rs` (`start_stop_watchdog`, `STOP_DEADLINE`, `TeardownStep`), `crates/qaros-rerun/src/qaros_setup.rs` (`stop_and_wake`).
- [CC] `crates/qaros-sink/src/console_stop.rs` (`stop_on_console_events`, `ConsoleStopState`), `crates/qaros-sink/src/process_exit.rs` (`end_process_after_bounded_flush`, `BOUNDED_FLUSH_WAIT`), `crates/qaros-rerun/src/qaros_setup.rs` (`stop_and_wake`), `demos/launcher.py` (`give_the_viewer_time_to_stop`).
- [EX] `crates/qaros-sink/src/process_exit.rs` (`end_process_after_teardown`), `crates/qaros-rerun/src/main.rs` (`exit_code_after_teardown`, `run_viewer`).
- [HR] Microsoft Learn, "HandlerRoutine callback function": the default handler calls `ExitProcess`; return values for close, logoff and shutdown; "Timeouts"; logoff and shutdown received only by services.
- [RX] Rust 1.96.0 standard library: `library/std/src/process.rs` L2468-2471 (`exit` calls `rt::cleanup`, then `sys::exit::exit`), `library/std/src/rt.rs` L139-146 (`cleanup` flushes stdout), `library/std/src/sys/exit.rs` L71 and L133 (`ExitProcess` on Windows).
- [EP] Microsoft Learn, "ExitProcess function": the entry point of every attached DLL is called with `DLL_PROCESS_DETACH`.

[OS]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/developer-guide/onboarding-and-sessions.mdx "Onboarding and sessions"
[HUB]: https://github.com/Quaternar/qaros/blob/ebfe3bf/docs/website/docs/operator-guide/running-the-hub.md "Running the Hub"
[KI]: ../known-issues.md "Known issues"
[VP]: ../vulkan-path.md "The QAROS Vulkan path"
[WD]: ../../crates/qaros-sink/src/stop_watchdog.rs "Stop watchdog"
[CC]: ../../crates/qaros-sink/src/console_stop.rs "Ctrl+C as a Stop"
[EX]: ../../crates/qaros-sink/src/process_exit.rs "Process exit after the QAROS teardown"
[HR]: https://learn.microsoft.com/en-us/windows/console/handlerroutine "HandlerRoutine callback function"
[RX]: https://github.com/rust-lang/rust/blob/1.96.0/library/std/src/process.rs#L2468-L2471 "Rust 1.96.0 std::process::exit"
[EP]: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-exitprocess "ExitProcess function"
