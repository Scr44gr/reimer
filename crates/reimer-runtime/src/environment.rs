//! Process-environment ABI used by the safe `std::env` wrappers.

use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::active_execution_session;

/// ABI symbol used to count command-line arguments.
pub const ENVIRONMENT_ARGUMENT_COUNT_SYMBOL: &str = "environment_argument_count";
/// ABI symbol used to snapshot one command-line argument.
pub const ENVIRONMENT_ARGUMENT_OPEN_SYMBOL: &str = "environment_argument_open";
/// ABI symbol used to snapshot one environment variable.
pub const ENVIRONMENT_VARIABLE_OPEN_SYMBOL: &str = "environment_variable_open";
/// ABI symbol used to snapshot the current working directory.
pub const ENVIRONMENT_CURRENT_DIR_OPEN_SYMBOL: &str = "environment_current_dir_open";
/// ABI symbol used to snapshot the current executable path.
pub const ENVIRONMENT_CURRENT_EXE_OPEN_SYMBOL: &str = "environment_current_exe_open";
/// ABI symbol used to query a snapshot's byte length.
pub const ENVIRONMENT_SNAPSHOT_LEN_SYMBOL: &str = "environment_snapshot_len";
/// ABI symbol used to copy a complete snapshot into caller-owned storage.
pub const ENVIRONMENT_SNAPSHOT_COPY_SYMBOL: &str = "environment_snapshot_copy";
/// ABI symbol used to release a snapshot.
pub const ENVIRONMENT_SNAPSHOT_CLOSE_SYMBOL: &str = "environment_snapshot_close";

/// The requested argument or environment variable does not exist.
pub const ENVIRONMENT_NOT_FOUND: isize = -1;
/// The native operating-system string cannot be represented as Reimer UTF-8.
pub const ENVIRONMENT_NOT_UNICODE: isize = -2;
/// The operating system could not provide the requested value.
pub const ENVIRONMENT_FAILED: isize = -3;
/// The supplied snapshot handle is no longer live.
pub const ENVIRONMENT_INVALID_HANDLE: isize = -4;
/// An environment-variable name is empty or contains a forbidden character.
pub const ENVIRONMENT_INVALID_NAME: isize = -5;

static NEXT_SNAPSHOT_HANDLE: AtomicUsize = AtomicUsize::new(1);
static SNAPSHOTS: OnceLock<Mutex<HashMap<usize, Vec<u8>>>> = OnceLock::new();
static EXECUTION_ARGUMENTS: OnceLock<Mutex<HashMap<usize, Arc<[OsString]>>>> = OnceLock::new();

fn lock_snapshots() -> std::sync::MutexGuard<'static, HashMap<usize, Vec<u8>>> {
    SNAPSHOTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_execution_arguments() -> std::sync::MutexGuard<'static, HashMap<usize, Arc<[OsString]>>> {
    EXECUTION_ARGUMENTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn register_execution_arguments(session: usize, arguments: Vec<OsString>) {
    lock_execution_arguments().insert(session, Arc::from(arguments));
}

pub(crate) fn remove_execution_arguments(session: usize) {
    lock_execution_arguments().remove(&session);
}

fn current_arguments() -> Arc<[OsString]> {
    let session = active_execution_session();
    if session != 0
        && let Some(arguments) = lock_execution_arguments().get(&session).cloned()
    {
        return arguments;
    }
    Arc::from(env::args_os().collect::<Vec<_>>())
}

fn store_snapshot(value: OsString) -> isize {
    let Ok(value) = value.into_string() else {
        return ENVIRONMENT_NOT_UNICODE;
    };
    let mut snapshots = lock_snapshots();
    loop {
        let handle = NEXT_SNAPSHOT_HANDLE.fetch_add(1, Ordering::Relaxed);
        let Ok(result) = isize::try_from(handle) else {
            continue;
        };
        if handle != 0 && !snapshots.contains_key(&handle) {
            snapshots.insert(handle, value.into_bytes());
            return result;
        }
    }
}

unsafe fn with_utf8<Output>(
    data: *const u8,
    length: usize,
    action: impl FnOnce(&str) -> Output,
) -> Option<Output> {
    if data.is_null() {
        return (length == 0).then(|| action(""));
    }
    // SAFETY: The caller guarantees `length` readable bytes at `data`.
    let bytes = unsafe { std::slice::from_raw_parts(data, length) };
    std::str::from_utf8(bytes).ok().map(action)
}

fn valid_variable_name(name: &str) -> bool {
    !name.is_empty() && !name.as_bytes().contains(&b'=') && !name.as_bytes().contains(&0)
}

/// Returns the number of command-line arguments, including the program path.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn environment_argument_count() -> usize {
    current_arguments().len()
}

/// Creates a stable UTF-8 snapshot of one command-line argument.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn environment_argument_open(index: usize) -> isize {
    current_arguments()
        .get(index)
        .cloned()
        .map_or(ENVIRONMENT_NOT_FOUND, store_snapshot)
}

/// Creates a stable UTF-8 snapshot of one environment variable.
///
/// # Safety
///
/// `name` must either be null with a zero `length`, or point to `length`
/// readable UTF-8 bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn environment_variable_open(name: *const u8, length: usize) -> isize {
    // SAFETY: The caller upholds the bounded UTF-8 contract documented above.
    unsafe {
        with_utf8(name, length, |name| {
            if !valid_variable_name(name) {
                return ENVIRONMENT_INVALID_NAME;
            }
            env::var_os(name).map_or(ENVIRONMENT_NOT_FOUND, store_snapshot)
        })
    }
    .unwrap_or(ENVIRONMENT_INVALID_NAME)
}

/// Creates a stable UTF-8 snapshot of the current working directory.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn environment_current_dir_open() -> isize {
    env::current_dir().map_or(ENVIRONMENT_FAILED, |path| {
        store_snapshot(path.into_os_string())
    })
}

/// Creates a stable UTF-8 snapshot of the current executable path.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn environment_current_exe_open() -> isize {
    env::current_exe().map_or(ENVIRONMENT_FAILED, |path| {
        store_snapshot(path.into_os_string())
    })
}

/// Returns the byte length of a live UTF-8 snapshot.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn environment_snapshot_len(handle: usize) -> isize {
    lock_snapshots()
        .get(&handle)
        .and_then(|value| isize::try_from(value.len()).ok())
        .unwrap_or(ENVIRONMENT_INVALID_HANDLE)
}

/// Copies a complete snapshot into caller-owned storage.
///
/// # Safety
///
/// When `capacity` is nonzero, `destination` must point to `capacity` live,
/// writable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn environment_snapshot_copy(
    handle: usize,
    destination: *mut u8,
    capacity: usize,
) -> isize {
    let snapshots = lock_snapshots();
    let Some(value) = snapshots.get(&handle) else {
        return ENVIRONMENT_INVALID_HANDLE;
    };
    if value.len() > capacity || (!value.is_empty() && destination.is_null()) {
        return ENVIRONMENT_FAILED;
    }
    if !value.is_empty() {
        // SAFETY: The ABI contract provides a writable destination large enough
        // for `value`, and the two allocations cannot overlap.
        unsafe { std::ptr::copy_nonoverlapping(value.as_ptr(), destination, value.len()) };
    }
    isize::try_from(value.len()).unwrap_or(ENVIRONMENT_FAILED)
}

/// Releases one owned environment snapshot.
#[unsafe(no_mangle)]
pub extern "C" fn environment_snapshot_close(handle: usize) -> i32 {
    i32::from(lock_snapshots().remove(&handle).is_none())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{
        ENVIRONMENT_INVALID_NAME, ENVIRONMENT_NOT_FOUND, environment_argument_count,
        environment_argument_open, environment_snapshot_close, environment_snapshot_copy,
        environment_snapshot_len, environment_variable_open,
    };
    use crate::ExecutionSession;

    #[test]
    fn argument_snapshots_should_use_session_specific_arguments() {
        let _session = ExecutionSession::begin_with_arguments(vec![
            OsString::from("demo.reim"),
            OsString::from("hello"),
        ]);

        assert_eq!(environment_argument_count(), 2);
        let handle = environment_argument_open(1);
        assert!(handle > 0);
        let handle = handle.cast_unsigned();
        assert_eq!(environment_snapshot_len(handle), 5);
        let mut bytes = [0_u8; 5];
        // SAFETY: `bytes` is a live five-byte destination.
        let copied = unsafe { environment_snapshot_copy(handle, bytes.as_mut_ptr(), bytes.len()) };
        assert_eq!(copied, 5);
        assert_eq!(&bytes, b"hello");
        assert_eq!(environment_snapshot_close(handle), 0);
    }

    #[test]
    fn argument_snapshots_should_report_missing_indices() {
        let _session = ExecutionSession::begin_with_arguments(vec![OsString::from("demo")]);

        assert_eq!(environment_argument_open(3), ENVIRONMENT_NOT_FOUND);
    }

    #[test]
    fn environment_variables_should_reject_invalid_names() {
        let invalid = b"BAD=NAME";
        // SAFETY: `invalid` remains live for the complete bounded call.
        let result = unsafe { environment_variable_open(invalid.as_ptr(), invalid.len()) };

        assert_eq!(result, ENVIRONMENT_INVALID_NAME);
    }
}
