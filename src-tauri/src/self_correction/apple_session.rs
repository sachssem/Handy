//! FFI to the self-correction on-device session
//! (`swift/fork_self_correction.swift`): one prewarmed `LanguageModelSession`
//! with the pass's instructions, replaced after every use, generation
//! cancelled on timeout.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::time::Duration;

const STATUS_OK: c_int = 0;
const STATUS_TIMEOUT: c_int = 1;
const STATUS_UNAVAILABLE: c_int = 3;

#[repr(C)]
struct ForkScResult {
    text: *mut c_char,
    status: c_int,
    error: *mut c_char,
}

extern "C" {
    fn fork_sc_available() -> c_int;
    fn fork_sc_prepare(instructions: *const c_char, freshness_ms: i64);
    fn fork_sc_prepared_age_ms() -> i64;
    fn fork_sc_run(text: *const c_char, timeout_ms: i64) -> *mut ForkScResult;
    fn fork_sc_free_result(result: *mut ForkScResult);
}

/// Why a [`run`] produced no text.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RunError {
    Timeout,
    Unavailable,
    Busy,
    Failed(String),
}

impl RunError {
    pub(super) fn reason(&self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Unavailable => "apple_unavailable",
            Self::Busy => "busy",
            Self::Failed(_) => "error",
        }
    }
}

/// Whether the on-device model is usable right now.
pub(super) fn available() -> bool {
    unsafe { fork_sc_available() == 1 }
}

/// Idempotent and non-blocking: create + prewarm the session for
/// `instructions` in the background unless a fresh one is ready or on its way.
pub(super) fn prepare(instructions: &str) {
    let Ok(instructions) = CString::new(instructions) else {
        return;
    };
    let freshness_ms = super::WARM_FRESH_FOR.as_millis() as i64;
    unsafe { fork_sc_prepare(instructions.as_ptr(), freshness_ms) }
}

/// Time since the ready session was prewarmed; `None` when none is ready.
pub(super) fn prepared_age() -> Option<Duration> {
    let ms = unsafe { fork_sc_prepared_age_ms() };
    u64::try_from(ms).ok().map(Duration::from_millis)
}

/// Blocking: one generation, returning after at most `timeout` (Swift cancels
/// the generation then and prewarms a replacement session once it unwound).
pub(super) fn run(text: &str, timeout: Duration) -> Result<String, RunError> {
    let text = CString::new(text).map_err(|e| RunError::Failed(e.to_string()))?;
    let timeout_ms = i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX);
    let result_ptr = unsafe { fork_sc_run(text.as_ptr(), timeout_ms) };
    if result_ptr.is_null() {
        return Err(RunError::Failed("null result".to_string()));
    }
    let result = unsafe { &*result_ptr };
    let owned = |ptr: *mut c_char| {
        (!ptr.is_null()).then(|| {
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        })
    };
    let outcome = match result.status {
        STATUS_OK => Ok(owned(result.text).unwrap_or_default()),
        STATUS_TIMEOUT => Err(RunError::Timeout),
        STATUS_UNAVAILABLE => Err(unavailable_reason(owned(result.error).as_deref())),
        _ => Err(RunError::Failed(
            owned(result.error).unwrap_or_else(|| "unknown error".to_string()),
        )),
    };
    unsafe { fork_sc_free_result(result_ptr) };
    outcome
}

/// Swift uses UNAVAILABLE + "busy" while a cancelled generation still unwinds
/// (or a session is being prepared). Device unavailability remains distinct.
fn unavailable_reason(error: Option<&str>) -> RunError {
    match error {
        Some("busy") => RunError::Busy,
        _ => RunError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_busy_maps_to_busy_instead_of_device_unavailability() {
        assert_eq!(unavailable_reason(Some("busy")), RunError::Busy);
        assert_eq!(unavailable_reason(Some("busy")).reason(), "busy");
        assert_eq!(unavailable_reason(None).reason(), "apple_unavailable");
        assert_eq!(unavailable_reason(None), RunError::Unavailable);
        assert_eq!(
            unavailable_reason(Some("Apple Intelligence is unavailable")),
            RunError::Unavailable
        );
    }
}
