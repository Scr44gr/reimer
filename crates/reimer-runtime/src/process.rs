//! Child-process ABI used by the safe `std::process` wrappers.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use super::active_execution_session;

/// ABI symbol used to inspect the current process identifier.
pub const PROCESS_ID_SYMBOL: &str = "process_id";
/// ABI symbol used to terminate the current process with an exit code.
pub const PROCESS_EXIT_SYMBOL: &str = "process_exit";
/// ABI symbol used to create a child-process command.
pub const PROCESS_COMMAND_NEW_SYMBOL: &str = "process_command_new";
/// ABI symbol used to append one command argument.
pub const PROCESS_COMMAND_ARG_SYMBOL: &str = "process_command_arg";
/// ABI symbol used to set one child-specific environment variable.
pub const PROCESS_COMMAND_ENV_SYMBOL: &str = "process_command_env";
/// ABI symbol used to remove one inherited child environment variable.
pub const PROCESS_COMMAND_ENV_REMOVE_SYMBOL: &str = "process_command_env_remove";
/// ABI symbol used to clear the inherited child environment.
pub const PROCESS_COMMAND_ENV_CLEAR_SYMBOL: &str = "process_command_env_clear";
/// ABI symbol used to set a child working directory.
pub const PROCESS_COMMAND_CURRENT_DIR_SYMBOL: &str = "process_command_current_dir";
/// ABI symbol used to spawn a configured child process.
pub const PROCESS_COMMAND_SPAWN_SYMBOL: &str = "process_command_spawn";
/// ABI symbol used to execute and wait for a configured command.
pub const PROCESS_COMMAND_STATUS_SYMBOL: &str = "process_command_status";
/// ABI symbol used to release an unspawned command.
pub const PROCESS_COMMAND_CLOSE_SYMBOL: &str = "process_command_close";
/// ABI symbol used to inspect a child process identifier.
pub const PROCESS_CHILD_ID_SYMBOL: &str = "process_child_id";
/// ABI symbol used to request child termination.
pub const PROCESS_CHILD_KILL_SYMBOL: &str = "process_child_kill";
/// ABI symbol used to wait for and collect a child process.
pub const PROCESS_CHILD_WAIT_SYMBOL: &str = "process_child_wait";
/// ABI symbol used to terminate and collect an owned child process.
pub const PROCESS_CHILD_CLOSE_SYMBOL: &str = "process_child_close";

/// The operation completed successfully.
pub const PROCESS_OK: i32 = 0;
/// The supplied process handle is not live or has the wrong kind.
pub const PROCESS_INVALID_HANDLE: i32 = 1;
/// A program, argument, environment value, or path is invalid.
pub const PROCESS_INVALID_INPUT: i32 = 2;
/// The operating-system process operation failed.
pub const PROCESS_FAILED: i32 = 3;
/// Direct execution of Windows command scripts is intentionally unsupported.
pub const PROCESS_UNSUPPORTED_SCRIPT: i32 = 4;

const FIRST_PROCESS_HANDLE: usize = 1;
const PROCESS_CREATE_INVALID_INPUT: isize = -2;
const PROCESS_CREATE_FAILED: isize = -3;
const PROCESS_CREATE_UNSUPPORTED_SCRIPT: isize = -4;

static NEXT_PROCESS_HANDLE: AtomicUsize = AtomicUsize::new(FIRST_PROCESS_HANDLE);
static PROCESSES: OnceLock<Mutex<HashMap<usize, ProcessRecord>>> = OnceLock::new();

struct ProcessRecord {
    resource: ProcessResource,
    session: usize,
}

enum ProcessResource {
    Command(Command),
    Child(Child),
}

fn lock_processes() -> std::sync::MutexGuard<'static, HashMap<usize, ProcessRecord>> {
    PROCESSES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn store_resource(resource: ProcessResource) -> usize {
    store_resource_in_session(resource, active_execution_session())
}

fn store_resource_in_session(resource: ProcessResource, session: usize) -> usize {
    let mut processes = lock_processes();
    loop {
        let handle = NEXT_PROCESS_HANDLE.fetch_add(1, Ordering::Relaxed);
        if handle != 0 && !processes.contains_key(&handle) {
            processes.insert(handle, ProcessRecord { resource, session });
            return handle;
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

fn valid_value(value: &str) -> bool {
    !value.as_bytes().contains(&0)
}

fn valid_environment_name(name: &str) -> bool {
    !name.is_empty() && !name.as_bytes().contains(&b'=') && valid_value(name)
}

fn is_windows_command_script(program: &str) -> bool {
    cfg!(windows)
        && Path::new(program)
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
            })
}

fn with_command(handle: usize, action: impl FnOnce(&mut Command)) -> i32 {
    let mut processes = lock_processes();
    let Some(ProcessRecord {
        resource: ProcessResource::Command(command),
        ..
    }) = processes.get_mut(&handle)
    else {
        return PROCESS_INVALID_HANDLE;
    };
    action(command);
    PROCESS_OK
}

unsafe fn write_exit_status(
    status: ExitStatus,
    code: *mut i32,
    has_code: *mut bool,
    succeeded: *mut bool,
) -> i32 {
    if code.is_null() || has_code.is_null() || succeeded.is_null() {
        return PROCESS_INVALID_INPUT;
    }
    let native_code = status.code();
    // SAFETY: The caller guarantees three live scalar output slots.
    unsafe {
        code.write(native_code.unwrap_or(0));
        has_code.write(native_code.is_some());
        succeeded.write(status.success());
    }
    PROCESS_OK
}

/// Returns the current native process identifier.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn process_id() -> u64 {
    u64::from(std::process::id())
}

/// Terminates the current process immediately without running destructors.
#[unsafe(no_mangle)]
pub extern "C" fn process_exit(code: i32) {
    std::process::exit(code)
}

/// Creates an owned command handle for direct executable invocation.
///
/// # Safety
///
/// `program` must either be null with a zero `length`, or point to `length`
/// readable UTF-8 bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn process_command_new(program: *const u8, length: usize) -> isize {
    // SAFETY: The caller upholds the bounded UTF-8 contract documented above.
    unsafe {
        with_utf8(program, length, |program| {
            if program.is_empty() || !valid_value(program) {
                return PROCESS_CREATE_INVALID_INPUT;
            }
            if is_windows_command_script(program) {
                return PROCESS_CREATE_UNSUPPORTED_SCRIPT;
            }
            isize::try_from(store_resource(ProcessResource::Command(Command::new(
                program,
            ))))
            .unwrap_or(PROCESS_CREATE_FAILED)
        })
    }
    .unwrap_or(PROCESS_CREATE_INVALID_INPUT)
}

/// Appends one UTF-8 argument without invoking a command shell.
///
/// # Safety
///
/// `argument` must describe `length` readable UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn process_command_arg(
    handle: usize,
    argument: *const u8,
    length: usize,
) -> i32 {
    // SAFETY: The caller upholds the bounded UTF-8 contract documented above.
    unsafe {
        with_utf8(argument, length, |argument| {
            if valid_value(argument) {
                with_command(handle, |command| {
                    command.arg(argument);
                })
            } else {
                PROCESS_INVALID_INPUT
            }
        })
    }
    .unwrap_or(PROCESS_INVALID_INPUT)
}

/// Sets one environment variable only for the configured child.
///
/// # Safety
///
/// Both pointer/length pairs must describe readable UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn process_command_env(
    handle: usize,
    name: *const u8,
    name_length: usize,
    value: *const u8,
    value_length: usize,
) -> i32 {
    // SAFETY: The caller upholds both bounded UTF-8 contracts documented above.
    unsafe {
        with_utf8(name, name_length, |name| {
            with_utf8(value, value_length, |value| {
                if !valid_environment_name(name) || !valid_value(value) {
                    PROCESS_INVALID_INPUT
                } else {
                    with_command(handle, |command| {
                        command.env(name, value);
                    })
                }
            })
        })
    }
    .flatten()
    .unwrap_or(PROCESS_INVALID_INPUT)
}

/// Removes one inherited environment variable from the configured child.
///
/// # Safety
///
/// `name` must describe `length` readable UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn process_command_env_remove(
    handle: usize,
    name: *const u8,
    length: usize,
) -> i32 {
    // SAFETY: The caller upholds the bounded UTF-8 contract documented above.
    unsafe {
        with_utf8(name, length, |name| {
            if valid_environment_name(name) {
                with_command(handle, |command| {
                    command.env_remove(name);
                })
            } else {
                PROCESS_INVALID_INPUT
            }
        })
    }
    .unwrap_or(PROCESS_INVALID_INPUT)
}

/// Clears the inherited environment for the configured child.
#[unsafe(no_mangle)]
pub extern "C" fn process_command_env_clear(handle: usize) -> i32 {
    with_command(handle, |command| {
        command.env_clear();
    })
}

/// Sets the working directory for the configured child.
///
/// # Safety
///
/// `path` must describe `length` readable UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn process_command_current_dir(
    handle: usize,
    path: *const u8,
    length: usize,
) -> i32 {
    // SAFETY: The caller upholds the bounded UTF-8 contract documented above.
    unsafe {
        with_utf8(path, length, |path| {
            if valid_value(path) {
                with_command(handle, |command| {
                    command.current_dir(path);
                })
            } else {
                PROCESS_INVALID_INPUT
            }
        })
    }
    .unwrap_or(PROCESS_INVALID_INPUT)
}

/// Spawns a configured command with inherited standard streams.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn process_command_spawn(handle: usize) -> isize {
    let Some(record) = lock_processes().remove(&handle) else {
        return -isize::try_from(PROCESS_INVALID_HANDLE).unwrap_or(1);
    };
    let session = record.session;
    let ProcessResource::Command(mut command) = record.resource else {
        lock_processes().insert(handle, record);
        return -isize::try_from(PROCESS_INVALID_HANDLE).unwrap_or(1);
    };
    command
        .spawn()
        .map(|child| store_resource_in_session(ProcessResource::Child(child), session))
        .ok()
        .and_then(|child| isize::try_from(child).ok())
        .unwrap_or(PROCESS_CREATE_FAILED)
}

/// Executes a configured command and waits for its exit status.
///
/// # Safety
///
/// The three output pointers must identify live writable scalar values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn process_command_status(
    handle: usize,
    code: *mut i32,
    has_code: *mut bool,
    succeeded: *mut bool,
) -> i32 {
    let Some(ProcessRecord {
        resource: ProcessResource::Command(mut command),
        ..
    }) = lock_processes().remove(&handle)
    else {
        return PROCESS_INVALID_HANDLE;
    };
    let Ok(status) = command.status() else {
        return PROCESS_FAILED;
    };
    // SAFETY: The caller upholds the output-slot contract documented above.
    unsafe { write_exit_status(status, code, has_code, succeeded) }
}

/// Releases one unspawned command handle.
#[unsafe(no_mangle)]
pub extern "C" fn process_command_close(handle: usize) -> i32 {
    match lock_processes().remove(&handle) {
        Some(ProcessRecord {
            resource: ProcessResource::Command(_),
            ..
        }) => PROCESS_OK,
        Some(record) => {
            lock_processes().insert(handle, record);
            PROCESS_INVALID_HANDLE
        }
        None => PROCESS_INVALID_HANDLE,
    }
}

/// Returns a child process identifier, or zero for an invalid handle.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn process_child_id(handle: usize) -> u64 {
    let processes = lock_processes();
    let Some(ProcessRecord {
        resource: ProcessResource::Child(child),
        ..
    }) = processes.get(&handle)
    else {
        return 0;
    };
    u64::from(child.id())
}

/// Requests forceful child termination.
#[unsafe(no_mangle)]
pub extern "C" fn process_child_kill(handle: usize) -> i32 {
    let mut processes = lock_processes();
    let Some(ProcessRecord {
        resource: ProcessResource::Child(child),
        ..
    }) = processes.get_mut(&handle)
    else {
        return PROCESS_INVALID_HANDLE;
    };
    if child.kill().is_ok() {
        PROCESS_OK
    } else {
        PROCESS_FAILED
    }
}

/// Waits for a child and removes its process-table entry.
///
/// # Safety
///
/// The three output pointers must identify live writable scalar values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn process_child_wait(
    handle: usize,
    code: *mut i32,
    has_code: *mut bool,
    succeeded: *mut bool,
) -> i32 {
    let Some(ProcessRecord {
        resource: ProcessResource::Child(mut child),
        ..
    }) = lock_processes().remove(&handle)
    else {
        return PROCESS_INVALID_HANDLE;
    };
    let Ok(status) = child.wait() else {
        return PROCESS_FAILED;
    };
    // SAFETY: The caller upholds the output-slot contract documented above.
    unsafe { write_exit_status(status, code, has_code, succeeded) }
}

/// Terminates a still-running child, waits for it, and releases its handle.
#[unsafe(no_mangle)]
pub extern "C" fn process_child_close(handle: usize) -> i32 {
    let Some(ProcessRecord {
        resource: ProcessResource::Child(child),
        ..
    }) = lock_processes().remove(&handle)
    else {
        return PROCESS_INVALID_HANDLE;
    };
    close_child(child)
}

fn close_child(mut child: Child) -> i32 {
    match child.try_wait() {
        Ok(Some(_)) => PROCESS_OK,
        Ok(None) => {
            let _ = child.kill();
            if child.wait().is_ok() {
                PROCESS_OK
            } else {
                PROCESS_FAILED
            }
        }
        Err(_) => PROCESS_FAILED,
    }
}

/// Releases every command and child owned by one completed execution session.
///
/// Live children are terminated and collected before this function returns.
pub fn shutdown_processes(session: usize) {
    loop {
        let record = {
            let mut processes = lock_processes();
            let handle = processes
                .iter()
                .find_map(|(handle, record)| (record.session == session).then_some(*handle));
            handle.and_then(|handle| processes.remove(&handle))
        };
        let Some(record) = record else {
            return;
        };
        if let ProcessResource::Child(child) = record.resource {
            close_child(child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PROCESS_INVALID_HANDLE, PROCESS_INVALID_INPUT, PROCESS_OK, process_child_id,
        process_child_wait, process_command_arg, process_command_close, process_command_new,
        process_command_spawn, process_command_status, process_id, shutdown_processes,
    };
    use crate::ExecutionSession;

    fn successful_command() -> (&'static str, &'static str) {
        if cfg!(windows) {
            ("cmd.exe", "/c")
        } else {
            ("true", "")
        }
    }

    #[test]
    fn process_id_should_be_nonzero() {
        assert_ne!(process_id(), 0);
    }

    #[test]
    fn command_status_should_report_success() {
        let (program, first_argument) = successful_command();
        // SAFETY: Both string slices remain live for their bounded ABI calls.
        let handle = unsafe { process_command_new(program.as_ptr(), program.len()) };
        assert!(handle > 0);
        let handle = handle.cast_unsigned();
        if !first_argument.is_empty() {
            // SAFETY: The string slice remains live for the bounded ABI call.
            assert_eq!(
                unsafe {
                    process_command_arg(handle, first_argument.as_ptr(), first_argument.len())
                },
                PROCESS_OK
            );
            let exit = b"exit 0";
            // SAFETY: The byte string remains live for the bounded ABI call.
            assert_eq!(
                unsafe { process_command_arg(handle, exit.as_ptr(), exit.len()) },
                PROCESS_OK
            );
        }
        let mut code = -1;
        let mut has_code = false;
        let mut succeeded = false;
        // SAFETY: All output pointers identify live scalar values.
        let status = unsafe {
            process_command_status(handle, &raw mut code, &raw mut has_code, &raw mut succeeded)
        };

        assert_eq!(status, PROCESS_OK);
        assert!(has_code);
        assert!(succeeded);
        assert_eq!(code, 0);
    }

    #[test]
    fn commands_should_reject_nul_arguments() {
        let program = "echo";
        // SAFETY: The string slice remains live for the bounded ABI call.
        let handle = unsafe { process_command_new(program.as_ptr(), program.len()) };
        assert!(handle > 0);
        let handle = handle.cast_unsigned();
        let argument = b"bad\0argument";
        // SAFETY: The byte string remains live for the bounded ABI call.
        let status = unsafe { process_command_arg(handle, argument.as_ptr(), argument.len()) };

        assert_eq!(status, PROCESS_INVALID_INPUT);
    }

    #[test]
    fn spawned_children_should_expose_an_id_and_be_collectable() {
        let (program, first_argument) = successful_command();
        // SAFETY: Both string slices remain live for their bounded ABI calls.
        let handle = unsafe { process_command_new(program.as_ptr(), program.len()) };
        assert!(handle > 0);
        let handle = handle.cast_unsigned();
        if !first_argument.is_empty() {
            // SAFETY: The string slice remains live for the bounded ABI call.
            assert_eq!(
                unsafe {
                    process_command_arg(handle, first_argument.as_ptr(), first_argument.len())
                },
                PROCESS_OK
            );
            let exit = b"exit 0";
            // SAFETY: The byte string remains live for the bounded ABI call.
            assert_eq!(
                unsafe { process_command_arg(handle, exit.as_ptr(), exit.len()) },
                PROCESS_OK
            );
        }
        let child = process_command_spawn(handle);
        assert!(child > 0);
        let child = child.cast_unsigned();
        assert_ne!(process_child_id(child), 0);
        let mut code = -1;
        let mut has_code = false;
        let mut succeeded = false;
        // SAFETY: All output pointers identify live scalar values.
        let status = unsafe {
            process_child_wait(child, &raw mut code, &raw mut has_code, &raw mut succeeded)
        };

        assert_eq!(status, PROCESS_OK);
        assert!(has_code);
        assert!(succeeded);
        assert_eq!(code, 0);
    }

    #[test]
    fn waiting_on_an_unknown_child_should_fail() {
        let mut code = 0;
        let mut has_code = false;
        let mut succeeded = false;
        // SAFETY: All output pointers identify live scalar values.
        let status = unsafe {
            process_child_wait(
                usize::MAX,
                &raw mut code,
                &raw mut has_code,
                &raw mut succeeded,
            )
        };

        assert_ne!(status, PROCESS_OK);
    }

    #[test]
    fn session_shutdown_should_release_abandoned_commands() {
        let session = ExecutionSession::begin();
        let program = "echo";
        // SAFETY: The string slice remains live for the bounded ABI call.
        let handle = unsafe { process_command_new(program.as_ptr(), program.len()) };
        assert!(handle > 0);
        let handle = handle.cast_unsigned();

        shutdown_processes(session.id());

        assert_eq!(process_command_close(handle), PROCESS_INVALID_HANDLE);
    }
}
