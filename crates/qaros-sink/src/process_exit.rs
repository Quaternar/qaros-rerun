//! Ending the viewer process once its QAROS teardown is done.
//!
//! On Windows a normal exit (`std::process::exit`, `ExitProcess`) runs the detach code and
//! static destructors of every DLL still mapped. QAROS DLLs stay mapped for the life of the
//! process, and the viewer's QAROS teardown is already complete at this point, so there is
//! nothing left for that code to do. Terminating the process skips it; the operating system
//! reclaims the memory, handles and sockets either way.

use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

use crate::stop_watchdog::{TeardownStep, enter_teardown_step};

/// How long a forced end waits for the logs to flush before it terminates the process anyway.
pub const BOUNDED_FLUSH_WAIT: Duration = Duration::from_millis(200);

/// Flushes the frame timing log, the logger, stdout, stderr and the C runtime's streams, then
/// ends the process with `code`: on Windows by `TerminateProcess`, without unloading any DLL;
/// elsewhere by `std::process::exit`.
pub fn end_process_after_teardown(code: i32) -> ! {
    enter_teardown_step(TeardownStep::EndProcess);
    log_how_the_process_ends(code);
    flush_everything();
    end_process(code, flush_everything)
}

/// Ends the process with `code` while a teardown may still run, as
/// [`end_process_after_teardown`] does, but logs `reason` and flushes on a helper thread and
/// waits for it at most [`BOUNDED_FLUSH_WAIT`]: a hung teardown can hold the logger's, the
/// frame timing log's or a C stdio lock, and logging or flushing on this thread would then hang
/// with it.
pub fn end_process_after_bounded_flush(code: i32, reason: String) -> ! {
    let flushed = run_within(BOUNDED_FLUSH_WAIT, move || {
        log::warn!("{reason}");
        log_how_the_process_ends(code);
        flush_everything();
    });
    if !flushed {
        eprintln!("exit: flushing took over {BOUNDED_FLUSH_WAIT:?}; terminating without it");
    }
    end_process(code, || {
        run_within(BOUNDED_FLUSH_WAIT, flush_everything);
    })
}

/// Runs `work` on a thread of its own and waits for it at most `wait`. Returns whether it
/// finished; one that did not keeps running until the process ends.
fn run_within(wait: Duration, work: impl FnOnce() + Send + 'static) -> bool {
    let (finished, finish_seen) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("exit flush".to_owned())
        .spawn(move || {
            work();
            // The waiter may have given up and dropped its end.
            let _ = finished.send(());
        });
    spawned.is_ok() && finish_seen.recv_timeout(wait).is_ok()
}

fn flush_everything() {
    frame_sink::perf::flush();
    flush_output(&mut [&mut std::io::stdout(), &mut std::io::stderr()]);
    flush_c_runtime_streams();
}

fn flush_output(streams: &mut [&mut dyn Write]) {
    log::logger().flush();
    for stream in streams {
        if let Err(error) = stream.flush() {
            log::warn!("exit: cannot flush an output stream: {error}");
        }
    }
}

// QAROS DLLs log to the console through C stdio, whose buffers only `ExitProcess` would
// flush. `fflush(NULL)` reaches the streams of the C runtime this binary links, the shared
// ucrt (`ucrtbase.dll`) of the default dynamic CRT, so it covers the DLLs on that same ucrt
// only: a DLL with its own static CRT keeps its buffers.
#[cfg(windows)]
#[allow(unsafe_code)]
unsafe extern "C" {
    fn fflush(stream: *mut std::ffi::c_void) -> std::ffi::c_int;
}

#[cfg(windows)]
fn flush_c_runtime_streams() {
    #[allow(unsafe_code)]
    // SAFETY: a null stream is documented to flush every open output stream.
    let status = unsafe { fflush(std::ptr::null_mut()) };
    if status != 0 {
        log::warn!("exit: fflush(NULL) of the C runtime's streams returned {status}");
    }
}

// Elsewhere `std::process::exit` calls the C `exit`, which flushes C stdio itself.
#[cfg(not(windows))]
fn flush_c_runtime_streams() {}

#[cfg(windows)]
fn log_how_the_process_ends(code: i32) {
    log::info!("exit: terminating the process with code {code}, without unloading its DLLs");
}

#[cfg(not(windows))]
fn log_how_the_process_ends(code: i32) {
    log::info!("exit: exiting with code {code}");
}

/// `flush_before_normal_exit` runs only when termination fails and the process exits normally.
#[cfg(windows)]
fn end_process(code: i32, flush_before_normal_exit: impl FnOnce()) -> ! {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

    #[allow(unsafe_code)]
    // SAFETY: the pseudo handle of this process and a plain exit code.
    let terminated = unsafe { TerminateProcess(GetCurrentProcess(), code.cast_unsigned()) };
    if terminated != 0 {
        // Termination is asynchronous and may return before this thread is stopped; any exit
        // from here would run the DLL detach code the termination avoids.
        loop {
            std::thread::park();
        }
    }
    let termination_error = std::io::Error::last_os_error();
    log::error!(
        "exit: TerminateProcess failed ({termination_error}); exiting normally with code {code}"
    );
    flush_before_normal_exit();
    std::process::exit(code)
}

#[cfg(not(windows))]
fn end_process(code: i32, _flush_before_normal_exit: impl FnOnce()) -> ! {
    std::process::exit(code)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::BufWriter;
    use std::rc::Rc;

    use super::*;

    struct RecordingStream {
        received: Rc<RefCell<Vec<u8>>>,
    }

    impl Write for RecordingStream {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.received.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn buffered_stream() -> (BufWriter<RecordingStream>, Rc<RefCell<Vec<u8>>>) {
        let received = Rc::new(RefCell::new(Vec::new()));
        let stream = BufWriter::new(RecordingStream {
            received: Rc::clone(&received),
        });
        (stream, received)
    }

    #[test]
    fn a_flush_that_finishes_in_time_is_waited_for() {
        let (written, written_seen) = mpsc::channel();

        let finished = run_within(BOUNDED_FLUSH_WAIT, move || {
            written.send("qaros: teardown took 12 ms").unwrap();
        });

        assert!(finished);
        assert_eq!(written_seen.try_recv(), Ok("qaros: teardown took 12 ms"));
    }

    #[test]
    fn a_flush_blocked_by_a_held_lock_is_given_up_after_the_wait() {
        let log_lock = std::sync::Arc::new(std::sync::Mutex::new(()));
        let held_by_hung_teardown = log_lock.lock().unwrap();
        let lock_for_flush = std::sync::Arc::clone(&log_lock);
        let started = std::time::Instant::now();

        let finished = run_within(Duration::from_millis(50), move || {
            drop(lock_for_flush.lock());
        });

        let waited = started.elapsed();
        assert!(!finished);
        assert!(waited >= Duration::from_millis(50), "waited {waited:?}");
        assert!(waited < Duration::from_secs(1), "waited {waited:?}");
        drop(held_by_hung_teardown);
    }

    #[test]
    fn buffered_last_lines_reach_every_stream_before_the_process_ends() {
        let (mut stdout, stdout_received) = buffered_stream();
        let (mut stderr, stderr_received) = buffered_stream();
        stdout
            .write_all(b"headset: 120 frames\n")
            .unwrap_or_else(|error| panic!("buffered write: {error}"));
        stderr
            .write_all(b"qaros: teardown took 12 ms\n")
            .unwrap_or_else(|error| panic!("buffered write: {error}"));
        assert!(stdout_received.borrow().is_empty(), "still buffered");

        flush_output(&mut [&mut stdout, &mut stderr]);

        assert_eq!(
            stdout_received.borrow().as_slice(),
            b"headset: 120 frames\n"
        );
        assert_eq!(
            stderr_received.borrow().as_slice(),
            b"qaros: teardown took 12 ms\n"
        );
    }
}
