//! Ctrl+C, Ctrl+Break and a closed console as a Stop.
//!
//! Windows' default console handler ends the process with `ExitProcess`, which runs the detach
//! code of every DLL still mapped, QAROS's among them (QAROS DLLs stay mapped for the life of
//! the process). The handler here asks the viewer to stop instead, the
//! same request as a QAROS Stop, so the UI loop leaves QAROS and the process ends through
//! [`end_process_after_teardown`](crate::end_process_after_teardown).

use std::time::Duration;

/// How long a closed console waits for the teardown before the handler ends the process
/// itself: Windows ends it once the handler returns or 5 s after the event (`HandlerRoutine`,
/// "Timeouts"), by a path this module cannot vouch for.
pub const CLOSE_GRACE: Duration = Duration::from_secs(4);

/// The console events that stop the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleEvent {
    CtrlC,
    CtrlBreak,
    /// The console window closes, or Task Manager's End Task; Windows ends the process once
    /// the handler returns.
    Close,
    /// Logoff and shutdown reach services only (`HandlerRoutine`); they take the close path.
    Logoff,
    Shutdown,
}

impl ConsoleEvent {
    /// Windows ends the process once the handler returns for these, so the handler waits for
    /// the teardown instead of returning.
    pub fn ends_the_process_on_return(self) -> bool {
        matches!(self, Self::Close | Self::Logoff | Self::Shutdown)
    }

    fn describe(self) -> &'static str {
        match self {
            Self::CtrlC => "Ctrl+C",
            Self::CtrlBreak => "Ctrl+Break",
            Self::Close => "console closed",
            Self::Logoff => "logoff",
            Self::Shutdown => "shutdown",
        }
    }
}

/// What a console event does to the viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleResponse {
    /// The first event: the viewer stops as on a QAROS Stop.
    RequestStop,
    /// Any later event, however soon: the user does not want to wait for the teardown, so the
    /// process ends without it.
    ForceEnd,
}

/// Turns the console events of one process into stop requests.
#[derive(Debug, Default)]
pub struct ConsoleStopState {
    stop_requested: bool,
}

impl ConsoleStopState {
    pub fn respond(&mut self) -> ConsoleResponse {
        if self.stop_requested {
            return ConsoleResponse::ForceEnd;
        }
        self.stop_requested = true;
        ConsoleResponse::RequestStop
    }
}

/// Exit code of a process a second Ctrl+C ended before its teardown was done.
pub const FORCED_END_CODE: i32 = 1;

/// The log line for a console event's response. The forced end's line is logged off the
/// handler thread, as part of [`end_process_after_bounded_flush`](crate::end_process_after_bounded_flush).
fn response_line(event: ConsoleEvent, response: ConsoleResponse) -> String {
    let event = event.describe();
    match response {
        ConsoleResponse::RequestStop => format!(
            "qaros: {event}; stopping as on a QAROS Stop (press Ctrl+C again to end at once)"
        ),
        ConsoleResponse::ForceEnd => {
            format!("qaros: {event} again; ending the process without waiting for the teardown")
        }
    }
}

/// Installs a console control handler that calls `request_stop` on the first Ctrl+C,
/// Ctrl+Break, console close, logoff or shutdown, and ends the process on a second press.
/// Install it before the QAROS library is loaded. Elsewhere than on Windows it installs
/// nothing: SIGINT keeps its default, which runs no unload code, and QAROS runs on Windows only.
pub fn stop_on_console_events(
    request_stop: impl Fn() + Send + Sync + 'static,
) -> Result<(), String> {
    platform::install(Box::new(request_stop))
}

#[cfg(windows)]
mod platform {
    use std::sync::{Mutex, OnceLock, PoisonError};

    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
        SetConsoleCtrlHandler,
    };
    use windows_sys::core::BOOL;

    use super::{
        CLOSE_GRACE, ConsoleEvent, ConsoleResponse, ConsoleStopState, FORCED_END_CODE,
        response_line,
    };
    use crate::end_process_after_bounded_flush;

    struct Installed {
        state: Mutex<ConsoleStopState>,
        request_stop: Box<dyn Fn() + Send + Sync>,
    }

    static INSTALLED: OnceLock<Installed> = OnceLock::new();

    pub(super) fn event_from_code(code: u32) -> Option<ConsoleEvent> {
        match code {
            CTRL_C_EVENT => Some(ConsoleEvent::CtrlC),
            CTRL_BREAK_EVENT => Some(ConsoleEvent::CtrlBreak),
            CTRL_CLOSE_EVENT => Some(ConsoleEvent::Close),
            CTRL_LOGOFF_EVENT => Some(ConsoleEvent::Logoff),
            CTRL_SHUTDOWN_EVENT => Some(ConsoleEvent::Shutdown),
            _ => None,
        }
    }

    pub(super) fn install(request_stop: Box<dyn Fn() + Send + Sync>) -> Result<(), String> {
        let installed = Installed {
            state: Mutex::new(ConsoleStopState::default()),
            request_stop,
        };
        if INSTALLED.set(installed).is_err() {
            return Err("a console stop handler is already installed".to_owned());
        }
        #[allow(unsafe_code)]
        // SAFETY: `on_console_event` matches `PHANDLER_ROUTINE` and lives as long as the process.
        let added = unsafe { SetConsoleCtrlHandler(Some(on_console_event), 1) };
        if added == 0 {
            return Err(format!(
                "SetConsoleCtrlHandler failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    /// Windows calls this on a thread of its own for each event.
    #[allow(unsafe_code)]
    unsafe extern "system" fn on_console_event(code: u32) -> BOOL {
        let (Some(event), Some(installed)) = (event_from_code(code), INSTALLED.get()) else {
            return 0;
        };
        let response = installed
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .respond();
        let line = response_line(event, response);
        match response {
            ConsoleResponse::RequestStop => {
                log::info!("{line}");
                (installed.request_stop)();
            }
            ConsoleResponse::ForceEnd => end_process_after_bounded_flush(FORCED_END_CODE, line),
        }
        if event.ends_the_process_on_return() {
            // The UI loop's teardown ends the process and this thread with it; one that has
            // not after the grace ends here rather than by Windows' own exit.
            std::thread::sleep(CLOSE_GRACE);
            end_process_after_bounded_flush(
                FORCED_END_CODE,
                format!("qaros: teardown not done {CLOSE_GRACE:?} after the console closed"),
            );
        }
        1
    }
}

#[cfg(not(windows))]
mod platform {
    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn install(_request_stop: Box<dyn Fn() + Send + Sync>) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_event_stops_and_any_later_one_forces_the_end() {
        let mut state = ConsoleStopState::default();

        assert_eq!(state.respond(), ConsoleResponse::RequestStop);
        assert_eq!(
            state.respond(),
            ConsoleResponse::ForceEnd,
            "a deliberate quick second press"
        );
        assert_eq!(state.respond(), ConsoleResponse::ForceEnd);
    }

    #[test]
    fn each_response_names_the_event_and_what_follows() {
        assert_eq!(
            response_line(ConsoleEvent::CtrlC, ConsoleResponse::RequestStop),
            "qaros: Ctrl+C; stopping as on a QAROS Stop (press Ctrl+C again to end at once)"
        );
        assert_eq!(
            response_line(ConsoleEvent::Close, ConsoleResponse::ForceEnd),
            "qaros: console closed again; ending the process without waiting for the teardown"
        );
    }

    #[test]
    fn only_close_logoff_and_shutdown_end_the_process_when_the_handler_returns() {
        assert!(!ConsoleEvent::CtrlC.ends_the_process_on_return());
        assert!(!ConsoleEvent::CtrlBreak.ends_the_process_on_return());
        assert!(ConsoleEvent::Close.ends_the_process_on_return());
        assert!(ConsoleEvent::Logoff.ends_the_process_on_return());
        assert!(ConsoleEvent::Shutdown.ends_the_process_on_return());
        assert!(
            CLOSE_GRACE + crate::BOUNDED_FLUSH_WAIT < Duration::from_secs(5),
            "the forced end after the grace, flush wait included, is inside Windows' 5 s"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_event_codes_map_to_console_events() {
        assert_eq!(platform::event_from_code(0), Some(ConsoleEvent::CtrlC));
        assert_eq!(platform::event_from_code(1), Some(ConsoleEvent::CtrlBreak));
        assert_eq!(platform::event_from_code(2), Some(ConsoleEvent::Close));
        assert_eq!(platform::event_from_code(5), Some(ConsoleEvent::Logoff));
        assert_eq!(platform::event_from_code(6), Some(ConsoleEvent::Shutdown));
        assert_eq!(platform::event_from_code(3), None);
    }
}
