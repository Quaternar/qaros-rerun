//! Reading a `QarResult` from the raw table, for the modules that call it directly.

use std::ffi::{CStr, c_char};

use qaros::qaros_sys::{QarApi, QarResult, QarStatusCode};

/// Read from the status code, as the `qaros` crate does, rather than through
/// `qar_result_is_success`, which `qaros-stub` does not implement.
pub(crate) fn is_success(result: QarResult) -> bool {
    result.code == QarStatusCode::QAR_STATUS_SUCCESS
}

/// `Err` names `function` and carries the library's message for a failed `result`.
pub(crate) fn check(api: &QarApi, function: &str, result: QarResult) -> Result<(), String> {
    if is_success(result) {
        return Ok(());
    }
    let mut buffer = [0 as c_char; 256];
    // SAFETY: the buffer is writable for its full length, which is passed, and is NUL-terminated
    // by the call.
    let message = unsafe {
        (api.qar_impl_result_message)(result, buffer.as_mut_ptr(), buffer.len());
        CStr::from_ptr(buffer.as_ptr())
    };
    Err(format!("{function}: {}", message.to_string_lossy()))
}

/// The message of a `result` already known to have failed.
pub(crate) fn failure(api: &QarApi, function: &str, result: QarResult) -> String {
    check(api, function, result)
        .err()
        .unwrap_or_else(|| format!("{function} succeeded"))
}
