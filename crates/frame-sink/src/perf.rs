//! Frame timing log for performance work, off unless `QAROS_RERUN_PERF_LOG` names a file.
//!
//! Each line is `wall_us,event,user,value`: the wall clock in microseconds since the Unix
//! epoch (comparable with other processes on the same machine, unlike `Instant`), the event
//! name, the user it concerns (`-` for none) and a value, usually a duration in microseconds.
//! With `QAROS_RERUN_PERF_GPU_SYNC=1` the renderer also waits for the GPU after its
//! submissions, so GPU durations can be read at the cost of serializing CPU and GPU.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct PerfLog {
    writer: Mutex<BufWriter<File>>,
    gpu_sync: bool,
}

fn perf_log() -> Option<&'static PerfLog> {
    static LOG: OnceLock<Option<PerfLog>> = OnceLock::new();
    LOG.get_or_init(|| {
        let path = std::env::var_os("QAROS_RERUN_PERF_LOG")?;
        let file = match File::create(&path) {
            Ok(file) => file,
            Err(error) => {
                log::warn!("perf: cannot create {}: {error}", path.to_string_lossy());
                return None;
            }
        };
        let gpu_sync = std::env::var("QAROS_RERUN_PERF_GPU_SYNC").is_ok_and(|value| value == "1");
        log::info!(
            "perf: frame timing log in {} (GPU sync {gpu_sync})",
            path.to_string_lossy()
        );
        Some(PerfLog {
            writer: Mutex::new(BufWriter::new(file)),
            gpu_sync,
        })
    })
    .as_ref()
}

pub fn is_enabled() -> bool {
    perf_log().is_some()
}

pub fn gpu_sync() -> bool {
    perf_log().is_some_and(|log| log.gpu_sync)
}

pub fn wall_micros() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_micros())
}

/// Records `value` under `event`; `user` is any displayable id or `None`.
pub fn record(event: &str, user: Option<&dyn std::fmt::Display>, value: u128) {
    let Some(log) = perf_log() else {
        return;
    };
    let wall = wall_micros();
    let mut writer = log.writer.lock().unwrap_or_else(PoisonError::into_inner);
    let outcome = match user {
        Some(user) => writeln!(writer, "{wall},{event},{user},{value}"),
        None => writeln!(writer, "{wall},{event},-,{value}"),
    };
    if let Err(error) = outcome {
        log::warn!("perf: cannot write the frame timing log: {error}");
    }
}

pub fn flush() {
    if let Some(log) = perf_log()
        && let Err(error) = log
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
    {
        log::warn!("perf: cannot flush the frame timing log: {error}");
    }
}

/// Records the time from its creation to its drop, in microseconds.
pub struct Span {
    event: &'static str,
    started: Option<Instant>,
}

impl Span {
    pub fn start(event: &'static str) -> Self {
        Self {
            event,
            started: is_enabled().then(Instant::now),
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            record(self.event, None, started.elapsed().as_micros());
        }
    }
}

/// Times `work` and records it under `event` for `user`.
pub fn time_for_user<Output>(
    event: &str,
    user: &dyn std::fmt::Display,
    work: impl FnOnce() -> Output,
) -> Output {
    if !is_enabled() {
        return work();
    }
    let started = Instant::now();
    let output = work();
    record(event, Some(user), started.elapsed().as_micros());
    output
}
