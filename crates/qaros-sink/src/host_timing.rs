//! How Windows schedules this process, on the CPU and on the GPU.
//!
//! Launched by QAROS, the viewer runs headless: no visible window, so Windows may treat it as
//! background work. Two consequences are handled here:
//!
//! - Short waits end on the default 15.6 ms timer tick when Windows ignores a windowless
//!   process's timer resolution. Opting out of power throttling and raising the resolution to
//!   1 ms cut the wait for a fresh pose from about 10.5 to 6 ms in a headless run.
//! - In a headless run the frame's GPU work can complete about 28 ms late although it takes about
//!   0.15 ms, with the GPU nearly idle. One candidate is the GPU scheduling priority of the
//!   process; [`raise_gpu_priority`] sets it to high, to test that.

/// Opts the process out of power throttling, including Windows ignoring its timer resolution,
/// and asks for a 1 ms timer resolution. Logs what it could not do; nothing else depends on it.
#[cfg(windows)]
pub fn keep_process_waits_precise() {
    use windows_sys::Win32::Media::timeBeginPeriod;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
    };

    // The control mask names the policies decided here, and a state mask without them turns
    // both off: full speed, and timer resolution requests honoured without a visible window.
    let throttling = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    #[allow(clippy::cast_possible_truncation)]
    let throttling_size = std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32;
    #[allow(unsafe_code)]
    // SAFETY: the pseudo handle of this process, and a pointer to a live structure of the size
    // passed, as SetProcessInformation documents for ProcessPowerThrottling.
    let throttling_set = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&raw const throttling).cast(),
            throttling_size,
        )
    } != 0;
    #[allow(unsafe_code)]
    // SAFETY: a plain value argument; the period stays raised for the process's lifetime.
    let resolution_raised = unsafe { timeBeginPeriod(1) } == 0;
    if throttling_set && resolution_raised {
        log::info!("host: power throttling off and timer resolution raised to 1 ms");
    } else {
        log::warn!(
            "host: power throttling off: {throttling_set}, timer resolution at 1 ms: {resolution_raised}"
        );
    }
}

/// Only Windows coarsens the waits of a process without a visible window.
#[cfg(not(windows))]
pub fn keep_process_waits_precise() {}

// Declared here rather than through `windows-sys`'s `Wdk_Graphics_Direct3D` feature: enabling
// a feature of `windows-sys` rebuilds every crate of the viewer that depends on it.
#[cfg(windows)]
#[link(name = "gdi32")]
#[allow(unsafe_code)]
unsafe extern "system" {
    fn D3DKMTGetProcessSchedulingPriorityClass(
        process: windows_sys::Win32::Foundation::HANDLE,
        priority_class: *mut i32,
    ) -> i32;
    fn D3DKMTSetProcessSchedulingPriorityClass(
        process: windows_sys::Win32::Foundation::HANDLE,
        priority_class: i32,
    ) -> i32;
}

/// `D3DKMT_SCHEDULINGPRIORITYCLASS` values, from `d3dkmthk.h`.
#[cfg(windows)]
fn priority_class_name(priority_class: i32) -> &'static str {
    match priority_class {
        0 => "idle",
        1 => "below normal",
        2 => "normal",
        3 => "above normal",
        4 => "high",
        5 => "realtime",
        _ => "unknown",
    }
}

/// Logs the GPU scheduling priority class Windows gave the process, then sets it to high.
#[cfg(windows)]
pub fn raise_gpu_priority() {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    const HIGH: i32 = 4;
    let mut before = -1_i32;
    #[allow(unsafe_code)]
    // SAFETY: the pseudo handle of this process and a valid out-pointer.
    let read_status =
        unsafe { D3DKMTGetProcessSchedulingPriorityClass(GetCurrentProcess(), &raw mut before) };
    #[allow(unsafe_code)]
    // SAFETY: the pseudo handle of this process and a plain value.
    let set_status = unsafe { D3DKMTSetProcessSchedulingPriorityClass(GetCurrentProcess(), HIGH) };
    let mut after = -1_i32;
    #[allow(unsafe_code)]
    // SAFETY: as above.
    unsafe {
        D3DKMTGetProcessSchedulingPriorityClass(GetCurrentProcess(), &raw mut after);
    }
    let before_name = if read_status == 0 {
        priority_class_name(before)
    } else {
        "unreadable"
    };
    if set_status == 0 {
        log::info!(
            "host: GPU scheduling priority {before_name} -> {}",
            priority_class_name(after)
        );
    } else {
        log::warn!(
            "host: GPU scheduling priority stays {before_name}: setting high failed with NTSTATUS {set_status:#010x}"
        );
    }
}

/// GPU scheduling priority classes are a Windows (WDDM) notion.
#[cfg(not(windows))]
pub fn raise_gpu_priority() {}
