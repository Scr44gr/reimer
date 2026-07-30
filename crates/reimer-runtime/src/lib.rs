//! Small native runtime ABI used by generated programs.

use std::alloc::{self, Layout};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

#[expect(
    unsafe_code,
    reason = "synchronization primitives expose stable symbols to generated native code"
)]
mod concurrency;
#[expect(
    unsafe_code,
    reason = "the file-system ABI validates raw buffers at the generated-code boundary"
)]
mod filesystem;
#[expect(
    unsafe_code,
    reason = "the job ABI exposes stable symbols and invokes compiler-generated callback thunks"
)]
mod job;
#[expect(
    unsafe_code,
    reason = "scalar math helpers expose stable symbols to generated native code"
)]
mod mathematics;

pub use concurrency::{
    ATOMIC_CLONE_SYMBOL, ATOMIC_COMPARE_EXCHANGE_SYMBOL, ATOMIC_CREATE_SYMBOL,
    ATOMIC_DESTROY_SYMBOL, ATOMIC_FETCH_ADD_SYMBOL, ATOMIC_LOAD_SYMBOL, ATOMIC_STORE_SYMBOL,
    ATOMIC_SWAP_SYMBOL, BARRIER_CLONE_SYMBOL, BARRIER_CREATE_SYMBOL, BARRIER_DESTROY_SYMBOL,
    BARRIER_WAIT_SYMBOL, CHANNEL_CLONE_SYMBOL, CHANNEL_CLOSE_SYMBOL, CHANNEL_CREATE_SYMBOL,
    CHANNEL_DESTROY_SYMBOL, CHANNEL_RECEIVE_SYMBOL, CHANNEL_SEND_SYMBOL, MUTEX_CLONE_SYMBOL,
    MUTEX_CREATE_SYMBOL, MUTEX_DESTROY_SYMBOL, MUTEX_LOAD_SYMBOL, MUTEX_REPLACE_SYMBOL,
    RWLOCK_CLONE_SYMBOL, RWLOCK_CREATE_SYMBOL, RWLOCK_DESTROY_SYMBOL, RWLOCK_LOAD_SYMBOL,
    RWLOCK_REPLACE_SYMBOL, SEMAPHORE_ACQUIRE_SYMBOL, SEMAPHORE_CLONE_SYMBOL,
    SEMAPHORE_CREATE_SYMBOL, SEMAPHORE_DESTROY_SYMBOL, SEMAPHORE_RELEASE_SYMBOL,
    SEMAPHORE_TRY_ACQUIRE_SYMBOL, SYNC_CLOSED, SYNC_INVALID_HANDLE, SYNC_OK,
    THREAD_LOCAL_CLONE_SYMBOL, THREAD_LOCAL_CREATE_SYMBOL, THREAD_LOCAL_DESTROY_SYMBOL,
    THREAD_LOCAL_GET_SYMBOL, THREAD_LOCAL_SET_SYMBOL, atomic_clone, atomic_compare_exchange,
    atomic_create, atomic_destroy, atomic_fetch_add, atomic_load, atomic_store, atomic_swap,
    barrier_clone, barrier_create, barrier_destroy, barrier_wait, channel_clone, channel_close,
    channel_create, channel_destroy, channel_receive, channel_send, mutex_clone, mutex_create,
    mutex_destroy, mutex_load, mutex_replace, rwlock_clone, rwlock_create, rwlock_destroy,
    rwlock_load, rwlock_replace, semaphore_acquire, semaphore_clone, semaphore_create,
    semaphore_destroy, semaphore_release, semaphore_try_acquire, thread_local_clone,
    thread_local_create, thread_local_destroy, thread_local_get, thread_local_set,
};
pub use filesystem::{
    FILE_APPEND_SYMBOL, FILE_CLOSE_SYMBOL, FILE_CREATE_SYMBOL, FILE_FLUSH_SYMBOL, FILE_OPEN_SYMBOL,
    FILE_READ_EXACT_SYMBOL, FILE_READ_SYMBOL, FILE_REMAINING_LEN_SYMBOL, FILE_UNEXPECTED_EOF,
    FILE_WRITE_ALL_SYMBOL, FILE_WRITE_SYMBOL, PATH_EXISTS_SYMBOL, PATH_REMOVE_FILE_SYMBOL,
    PATH_RENAME_SYMBOL, file_append, file_close, file_create, file_flush, file_open, file_read,
    file_read_exact, file_remaining_len, file_write, file_write_all, path_exists, path_remove_file,
    path_rename,
};
pub use job::{
    JOB_JOIN_INVALID_HANDLE, JOB_JOIN_OK, JOB_JOIN_RESULT_MISMATCH, JOB_JOIN_WORKER_PANICKED,
    JOB_PARALLEL_FOR_SYMBOL, JOB_POOL_CLONE_SYMBOL, JOB_POOL_CREATE_SYMBOL,
    JOB_POOL_DESTROY_SYMBOL, JOB_SUBMIT_SYMBOL, JOB_WAIT_SYMBOL, ParallelForRequest,
    job_parallel_for, job_pool_clone, job_pool_create, job_pool_destroy, job_submit, job_wait,
    shutdown_all_job_pools, shutdown_job_pools,
};
pub use mathematics::{
    MATH_ABSOLUTE_F32_SYMBOL, MATH_ABSOLUTE_F64_SYMBOL, MATH_CEIL_F32_SYMBOL, MATH_CEIL_F64_SYMBOL,
    MATH_COS_F32_SYMBOL, MATH_COS_F64_SYMBOL, MATH_EXP_F32_SYMBOL, MATH_EXP_F64_SYMBOL,
    MATH_FLOOR_F32_SYMBOL, MATH_FLOOR_F64_SYMBOL, MATH_LN_F32_SYMBOL, MATH_LN_F64_SYMBOL,
    MATH_POW_F32_SYMBOL, MATH_POW_F64_SYMBOL, MATH_ROUND_F32_SYMBOL, MATH_ROUND_F64_SYMBOL,
    MATH_SIN_F32_SYMBOL, MATH_SIN_F64_SYMBOL, MATH_SQRT_F32_SYMBOL, MATH_SQRT_F64_SYMBOL,
    MATH_TAN_F32_SYMBOL, MATH_TAN_F64_SYMBOL, math_absolute_f32, math_absolute_f64, math_ceil_f32,
    math_ceil_f64, math_cos_f32, math_cos_f64, math_exp_f32, math_exp_f64, math_floor_f32,
    math_floor_f64, math_ln_f32, math_ln_f64, math_pow_f32, math_pow_f64, math_round_f32,
    math_round_f64, math_sin_f32, math_sin_f64, math_sqrt_f32, math_sqrt_f64, math_tan_f32,
    math_tan_f64,
};

/// Runtime failure categories emitted by checked native operations.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// User-requested or invariant panic.
    Panic = 1,
    /// Bounds check failure.
    Bounds = 2,
    /// Integer overflow.
    Overflow = 3,
    /// Integer division by zero.
    DivisionByZero = 4,
    /// Shift amount outside the integer width.
    InvalidShift = 5,
    /// Defensive failure after an exhaustive match.
    NonExhaustiveMatch = 6,
    /// Compiler/runtime allocator handle mismatch.
    InvalidAllocator = 7,
    /// Compiler/runtime synchronization handle or layout mismatch.
    InvalidSynchronization = 8,
}

/// ABI symbol used for failures without a dynamic message.
pub const FAIL_SYMBOL: &str = "runtime_fail";
/// ABI symbol used by the source-level `panic(message)` intrinsic.
pub const PANIC_SYMBOL: &str = "runtime_panic";
/// ABI symbol used by `std::target::os()` to inspect the native host.
pub const TARGET_OS_SYMBOL: &str = "target_os_code";
/// ABI symbol used for explicit byte allocations.
pub const ALLOCATE_BYTES_SYMBOL: &str = "allocate_bytes";
/// ABI symbol used to release explicit byte allocations.
pub const DEALLOCATE_BYTES_SYMBOL: &str = "deallocate_bytes";
/// Demonstration C-ABI symbol used by the FFI vertical slice.
pub const ABS_I32_SYMBOL: &str = "absolute_i32";
/// ABI symbol used to create an arena allocator.
pub const ARENA_INIT_SYMBOL: &str = "arena_allocator_init";
/// ABI symbol used to release an arena allocator and all of its allocations.
pub const ARENA_DEINIT_SYMBOL: &str = "arena_allocator_deinit";
/// ABI symbol used to create a fixed-buffer allocator.
pub const FIXED_INIT_SYMBOL: &str = "fixed_buffer_allocator_init";
/// ABI symbol used to retire a fixed-buffer allocator.
pub const FIXED_DEINIT_SYMBOL: &str = "fixed_buffer_allocator_deinit";
/// ABI symbol used for one potentially partial output write.
pub const OUTPUT_WRITE_SYMBOL: &str = "output_write";
/// ABI symbol used to write a complete output buffer.
pub const OUTPUT_WRITE_ALL_SYMBOL: &str = "output_write_all";
/// ABI symbol used to flush one process output stream.
pub const OUTPUT_FLUSH_SYMBOL: &str = "output_flush";
/// ABI symbol used to inspect whether an output stream is a terminal.
pub const OUTPUT_IS_TERMINAL_SYMBOL: &str = "output_is_terminal";
/// ABI symbol used for one potentially partial standard-input read.
pub const INPUT_READ_SYMBOL: &str = "input_read";
/// ABI symbol used to fill an exact standard-input buffer.
pub const INPUT_READ_EXACT_SYMBOL: &str = "input_read_exact";
/// ABI symbol used to read one line from standard input.
pub const INPUT_READ_LINE_SYMBOL: &str = "input_read_line";
/// ABI symbol used to read standard input until EOF or buffer capacity.
pub const INPUT_READ_TO_END_SYMBOL: &str = "input_read_to_end";
/// ABI symbol used to inspect whether standard input is a terminal.
pub const INPUT_IS_TERMINAL_SYMBOL: &str = "input_is_terminal";
/// ABI symbol used for bounded input-buffer and string comparisons.
pub const BUFFER_EQUALS_SYMBOL: &str = "buffer_equals";
/// ABI symbol used for copying non-overlapping bounded byte regions.
pub const COPY_BYTES_SYMBOL: &str = "copy_bytes";
/// ABI symbol used for validating bounded UTF-8 byte regions.
pub const UTF8_IS_VALID_SYMBOL: &str = "utf8_is_valid";
/// ABI symbol used for decoding one Unicode scalar from a bounded UTF-8 view.
pub const UTF8_DECODE_NEXT_SYMBOL: &str = "utf8_decode_next";
/// ABI symbol used to start one native worker thread.
pub const THREAD_SPAWN_SYMBOL: &str = "thread_spawn";
/// ABI symbol used to join one native worker thread.
pub const THREAD_JOIN_SYMBOL: &str = "thread_join";

/// Thread join completed and copied the worker result.
pub const THREAD_JOIN_OK: i32 = 0;
/// The supplied thread handle does not identify a live joinable worker.
pub const THREAD_JOIN_INVALID_HANDLE: i32 = 1;
/// The native worker terminated by unwinding.
pub const THREAD_JOIN_WORKER_PANICKED: i32 = 2;
/// The requested result layout differs from the spawned callback layout.
pub const THREAD_JOIN_RESULT_MISMATCH: i32 = 3;

/// Stable handle for the general-purpose allocator.
pub const GENERAL_ALLOCATOR: usize = 1;
/// Stable handle for the page-granular allocator.
pub const PAGE_ALLOCATOR: usize = 2;

const PAGE_GRANULARITY: usize = 4096;
const FIRST_DYNAMIC_ALLOCATOR: usize = 3;
const FIRST_THREAD_HANDLE: usize = 1;
const FIRST_EXECUTION_SESSION: usize = 1;
const STANDARD_OUTPUT: u8 = 1;
const STANDARD_ERROR: u8 = 2;
const IO_FAILURE_CODE: isize = -1;
const UNEXPECTED_END_OF_INPUT_CODE: isize = -2;

#[derive(Debug)]
struct Allocation {
    address: usize,
    length: usize,
}

#[derive(Debug)]
enum DynamicAllocator {
    Arena {
        parent: usize,
        allocations: Vec<Allocation>,
    },
    Fixed {
        base: usize,
        length: usize,
        offset: usize,
    },
}

static NEXT_ALLOCATOR: AtomicUsize = AtomicUsize::new(FIRST_DYNAMIC_ALLOCATOR);
static DYNAMIC_ALLOCATORS: OnceLock<Mutex<HashMap<usize, DynamicAllocator>>> = OnceLock::new();
static NEXT_THREAD_HANDLE: AtomicUsize = AtomicUsize::new(FIRST_THREAD_HANDLE);
static NEXT_EXECUTION_SESSION: AtomicUsize = AtomicUsize::new(FIRST_EXECUTION_SESSION);
static THREADS: OnceLock<Mutex<HashMap<usize, ThreadRecord>>> = OnceLock::new();

thread_local! {
    static ACTIVE_EXECUTION_SESSION: Cell<usize> = const { Cell::new(0) };
}

type ThreadThunk = unsafe extern "C" fn(usize, *const u8, *mut u8);

struct ThreadRecord {
    worker: JoinHandle<AlignedBytes>,
    result_size: usize,
    session: usize,
}

/// Identifies one live JIT execution so its native resources are isolated from
/// other programs running in the same compiler process.
pub struct ExecutionSession {
    id: usize,
    previous: usize,
}

impl ExecutionSession {
    /// Starts a distinct execution session on the current host thread.
    #[must_use]
    pub fn begin() -> Self {
        let mut id = NEXT_EXECUTION_SESSION.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            id = NEXT_EXECUTION_SESSION.fetch_add(1, Ordering::Relaxed);
        }
        Self::activate(id)
    }

    /// Returns the stable nonzero identity assigned to this execution.
    #[must_use]
    pub const fn id(&self) -> usize {
        self.id
    }

    fn activate(id: usize) -> Self {
        let previous = ACTIVE_EXECUTION_SESSION.replace(id);
        Self { id, previous }
    }
}

impl Drop for ExecutionSession {
    fn drop(&mut self) {
        ACTIVE_EXECUTION_SESSION.set(self.previous);
    }
}

pub(crate) fn active_execution_session() -> usize {
    ACTIVE_EXECUTION_SESSION.get()
}

#[derive(Clone)]
pub(crate) struct AlignedBytes {
    words: Vec<u128>,
    pub(crate) length: usize,
}

impl AlignedBytes {
    pub(crate) fn zeroed(length: usize) -> Self {
        Self {
            words: vec![0; length.div_ceil(size_of::<u128>())],
            length,
        }
    }

    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.words.as_ptr().cast()
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.words.as_mut_ptr().cast()
    }
}

/// Starts a native thread that invokes one compiler-generated callback thunk.
///
/// Returns zero if the OS cannot create the thread.
///
/// # Safety
///
/// `thunk` must be a live compiler-generated [`ThreadThunk`] address and
/// `callback` must match the signature encoded by that thunk. When
/// `argument_size` is nonzero, `argument` must point to that many readable
/// bytes for this call.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the thread ABI receives validated code and data addresses from generated code"
)]
pub unsafe extern "C" fn thread_spawn(
    thunk: usize,
    callback: usize,
    argument: *const u8,
    argument_size: usize,
    result_size: usize,
) -> usize {
    if thunk == 0 || callback == 0 || (argument_size != 0 && argument.is_null()) {
        return 0;
    }
    let mut argument_copy = AlignedBytes::zeroed(argument_size);
    if argument_size != 0 {
        // SAFETY: The ABI contract requires `argument_size` readable bytes.
        let source = unsafe { std::slice::from_raw_parts(argument, argument_size) };
        // SAFETY: `argument_copy` owns at least `argument_size` writable bytes.
        let destination =
            unsafe { std::slice::from_raw_parts_mut(argument_copy.as_mut_ptr(), argument_size) };
        destination.copy_from_slice(source);
    }
    let session = active_execution_session();
    let Ok(worker) = std::thread::Builder::new().spawn(move || {
        let _session = ExecutionSession::activate(session);
        let mut result = AlignedBytes::zeroed(result_size);
        // SAFETY: The caller supplies the compiler-generated thunk address and
        // its matching callback. Both remain live until every thread is joined.
        let thunk = unsafe { std::mem::transmute::<usize, ThreadThunk>(thunk) };
        // SAFETY: Both buffers use the exact layouts provided to the compiler
        // when it generated this thunk.
        unsafe { thunk(callback, argument_copy.as_ptr(), result.as_mut_ptr()) };
        result
    }) else {
        return 0;
    };
    let handle = NEXT_THREAD_HANDLE.fetch_add(1, Ordering::Relaxed);
    if handle == 0 {
        let _ = worker.join();
        return 0;
    }
    lock_threads().insert(
        handle,
        ThreadRecord {
            worker,
            result_size,
            session,
        },
    );
    handle
}

/// Joins a native thread and copies its result into compiler-owned storage.
///
/// # Safety
///
/// When `result_size` is nonzero, `destination` must point to that many live,
/// writable bytes aligned for the callback result type.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the join ABI writes into a result slot validated by generated code"
)]
pub unsafe extern "C" fn thread_join(
    handle: usize,
    destination: *mut u8,
    result_size: usize,
) -> i32 {
    let Some(record) = lock_threads().remove(&handle) else {
        return THREAD_JOIN_INVALID_HANDLE;
    };
    if result_size != record.result_size || (result_size != 0 && destination.is_null()) {
        let _ = record.worker.join();
        return THREAD_JOIN_RESULT_MISMATCH;
    }
    let Ok(result) = record.worker.join() else {
        return THREAD_JOIN_WORKER_PANICKED;
    };
    if result.length != result_size {
        return THREAD_JOIN_RESULT_MISMATCH;
    }
    if result_size != 0 {
        // SAFETY: The ABI contract supplies a live destination of `result_size`
        // bytes and the worker owns a result buffer of the same length.
        let destination = unsafe { std::slice::from_raw_parts_mut(destination, result_size) };
        // SAFETY: `result` owns at least `result_size` initialized bytes.
        let source = unsafe { std::slice::from_raw_parts(result.as_ptr(), result_size) };
        destination.copy_from_slice(source);
    }
    THREAD_JOIN_OK
}

/// Joins every outstanding native thread.
///
/// The JIT uses this before releasing executable memory so leaked source-level
/// handles cannot leave callbacks running against an unloaded module.
pub fn join_all_threads() {
    let records = {
        let mut threads = lock_threads();
        std::mem::take(&mut *threads)
    };
    for record in records.into_values() {
        let _ = record.worker.join();
    }
}

/// Joins outstanding native threads owned by one JIT execution.
pub fn join_session_threads(session: usize) {
    let records = {
        let mut threads = lock_threads();
        threads
            .extract_if(|_, record| record.session == session)
            .map(|(_, record)| record)
            .collect::<Vec<_>>()
    };
    for record in records {
        let _ = record.worker.join();
    }
}

fn lock_threads() -> std::sync::MutexGuard<'static, HashMap<usize, ThreadRecord>> {
    THREADS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Returns the operating-system code for the native host.
///
/// The numeric mapping is part of the private runtime ABI consumed by
/// `std::target`: Windows is 0, Linux is 1, macOS is 2, FreeBSD is 3, and
/// every other host is 4.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the runtime ABI must expose a stable native symbol"
)]
pub extern "C" fn target_os_code() -> u8 {
    if cfg!(target_os = "windows") {
        0
    } else if cfg!(target_os = "linux") {
        1
    } else if cfg!(target_os = "macos") {
        2
    } else if cfg!(target_os = "freebsd") {
        3
    } else {
        4
    }
}

/// Reports a checked runtime failure and terminates the process.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the runtime ABI must expose a stable native symbol"
)]
pub extern "C" fn runtime_fail(code: u32) -> ! {
    let message = failure_message(code);
    let _ = writeln!(std::io::stderr(), "Reimer panic: {message}");
    std::process::abort()
}

/// Reports a source-level panic message and terminates the process.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length` live
/// bytes for the duration of this call.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable runtime ABI receives a validated UTF-8 view from generated Reimer code"
)]
pub unsafe extern "C" fn runtime_panic(data: *const u8, length: usize, byte_offset: usize) -> ! {
    let bytes = if data.is_null() {
        &[]
    } else {
        // SAFETY: Generated code passes a live string-view pointer and its exact byte length.
        unsafe { std::slice::from_raw_parts(data, length) }
    };
    let message = String::from_utf8_lossy(bytes);
    let _ = writeln!(
        std::io::stderr(),
        "Reimer panic at source byte {byte_offset}: {message}"
    );
    std::process::abort()
}

/// Allocates an owned byte region through an explicit allocator handle.
///
/// Null is reserved for allocation failure. Unknown handles are compiler or
/// runtime bugs and terminate through the checked failure path.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the allocator ABI must expose a stable native symbol"
)]
pub extern "C" fn allocate_bytes(allocator: usize, length: usize) -> *mut u8 {
    match allocator {
        GENERAL_ALLOCATOR | PAGE_ALLOCATOR => allocate_static(allocator, length),
        _ => allocate_dynamic(allocator, length),
    }
}

/// Releases a byte region returned by [`allocate_bytes`].
///
/// # Safety
///
/// `data` and `length` must identify one live allocation returned by this ABI
/// with the same `allocator`. The region must not be used after this call.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the allocator ABI receives an owned raw allocation from generated code"
)]
pub unsafe extern "C" fn deallocate_bytes(allocator: usize, data: *mut u8, length: usize) {
    if data.is_null() {
        return;
    }
    match allocator {
        GENERAL_ALLOCATOR | PAGE_ALLOCATOR => {
            // SAFETY: This public ABI's contract guarantees matching allocation metadata.
            unsafe { deallocate_static(allocator, data, length) };
        }
        _ => validate_dynamic_deallocation(allocator, data, length),
    }
}

/// Creates an arena that releases all child allocations together.
///
/// The initial implementation deliberately accepts only the two stable
/// runtime allocators as parents, which avoids recursive allocator graphs.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the allocator ABI must expose a stable native symbol"
)]
pub extern "C" fn arena_allocator_init(parent: usize) -> usize {
    if !matches!(parent, GENERAL_ALLOCATOR | PAGE_ALLOCATOR) {
        return 0;
    }
    insert_dynamic_allocator(DynamicAllocator::Arena {
        parent,
        allocations: Vec::new(),
    })
}

/// Releases an arena and all allocations that remain owned by it.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the allocator ABI must expose a stable native symbol"
)]
pub extern "C" fn arena_allocator_deinit(handle: usize) {
    let allocator = remove_dynamic_allocator(handle);
    let DynamicAllocator::Arena {
        parent,
        allocations,
    } = allocator
    else {
        runtime_fail(Failure::InvalidAllocator as u32);
    };
    for allocation in allocations {
        // SAFETY: Arena entries are recorded from live allocations made with this parent.
        unsafe {
            deallocate_static(parent, allocation.address as *mut u8, allocation.length);
        }
    }
}

/// Creates a bump allocator over a caller-owned byte region.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the allocator ABI must expose a stable native symbol"
)]
pub extern "C" fn fixed_buffer_allocator_init(data: *mut u8, length: usize) -> usize {
    if data.is_null() || length == 0 {
        return 0;
    }
    insert_dynamic_allocator(DynamicAllocator::Fixed {
        base: data as usize,
        length,
        offset: 0,
    })
}

/// Retires a fixed-buffer allocator without freeing its caller-owned storage.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the allocator ABI must expose a stable native symbol"
)]
pub extern "C" fn fixed_buffer_allocator_deinit(handle: usize) {
    if !matches!(
        remove_dynamic_allocator(handle),
        DynamicAllocator::Fixed { .. }
    ) {
        runtime_fail(Failure::InvalidAllocator as u32);
    }
}

/// Writes at most `length` bytes to standard output or standard error.
///
/// `stream` is `1` for standard output and `2` for standard error. A
/// non-negative result is the number of bytes written; `-1` reports an I/O
/// failure or an invalid stream.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// readable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the I/O ABI borrows a raw byte view from generated code"
)]
pub unsafe extern "C" fn output_write(stream: u8, data: *const u8, length: usize) -> isize {
    // SAFETY: The public ABI contract guarantees that this byte view is live.
    let Some(bytes) = (unsafe { bytes_from_raw_parts(data, length) }) else {
        return IO_FAILURE_CODE;
    };
    write_output(stream, bytes).map_or(IO_FAILURE_CODE, count_as_io_result)
}

/// Writes a complete byte view and an optional newline to one output stream.
///
/// Returns zero on success and `-1` on failure.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// readable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the I/O ABI borrows a raw byte view from generated code"
)]
pub unsafe extern "C" fn output_write_all(
    stream: u8,
    data: *const u8,
    length: usize,
    append_newline: bool,
) -> i32 {
    // SAFETY: The public ABI contract guarantees that this byte view is live.
    let Some(bytes) = (unsafe { bytes_from_raw_parts(data, length) }) else {
        return -1;
    };
    write_all_output(stream, bytes, append_newline).map_or(-1, |()| 0)
}

/// Flushes standard output or standard error.
///
/// Returns zero on success and `-1` on failure or for an invalid `stream`.
#[must_use]
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "the I/O ABI must expose a stable native symbol")]
pub extern "C" fn output_flush(stream: u8) -> i32 {
    flush_output(stream).map_or(-1, |()| 0)
}

/// Reports whether standard output or standard error is attached to a terminal.
#[must_use]
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "the I/O ABI must expose a stable native symbol")]
pub extern "C" fn output_is_terminal(stream: u8) -> bool {
    match stream {
        STANDARD_OUTPUT => io::stdout().is_terminal(),
        STANDARD_ERROR => io::stderr().is_terminal(),
        _ => false,
    }
}

/// Performs one potentially partial read from standard input.
///
/// A non-negative result is the number of bytes read; zero means EOF and `-1`
/// reports an I/O failure.
///
/// # Safety
///
/// `data` must either be null with a zero `capacity`, or point to `capacity`
/// writable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the I/O ABI fills a raw owned byte region from generated code"
)]
pub unsafe extern "C" fn input_read(data: *mut u8, capacity: usize) -> isize {
    // SAFETY: The public ABI contract guarantees exclusive access to this byte region.
    let Some(buffer) = (unsafe { bytes_from_raw_parts_mut(data, capacity) }) else {
        return IO_FAILURE_CODE;
    };
    io::stdin()
        .lock()
        .read(buffer)
        .map_or(IO_FAILURE_CODE, count_as_io_result)
}

/// Fills a standard-input buffer or reports an early EOF.
///
/// Returns `length` on success, `-2` for an early EOF, and `-1` for another
/// I/O failure.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// writable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the I/O ABI fills a raw owned byte region from generated code"
)]
pub unsafe extern "C" fn input_read_exact(data: *mut u8, length: usize) -> isize {
    // SAFETY: The public ABI contract guarantees exclusive access to this byte region.
    let Some(buffer) = (unsafe { bytes_from_raw_parts_mut(data, length) }) else {
        return IO_FAILURE_CODE;
    };
    match io::stdin().lock().read_exact(buffer) {
        Ok(()) => count_as_io_result(length),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => UNEXPECTED_END_OF_INPUT_CODE,
        Err(_) => IO_FAILURE_CODE,
    }
}

/// Reads through a newline, EOF, or the supplied buffer capacity.
///
/// The newline is retained, matching Rust's `BufRead::read_line` convention.
/// A non-negative result is the number of initialized bytes and `-1` reports an
/// I/O failure.
///
/// # Safety
///
/// `data` must either be null with a zero `capacity`, or point to `capacity`
/// writable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the I/O ABI fills a raw owned byte region from generated code"
)]
pub unsafe extern "C" fn input_read_line(data: *mut u8, capacity: usize) -> isize {
    // SAFETY: The public ABI contract guarantees exclusive access to this byte region.
    let Some(buffer) = (unsafe { bytes_from_raw_parts_mut(data, capacity) }) else {
        return IO_FAILURE_CODE;
    };
    read_line_into(&mut io::stdin().lock(), buffer).map_or(IO_FAILURE_CODE, count_as_io_result)
}

/// Reads standard input until EOF or the supplied buffer capacity.
///
/// A non-negative result is the number of initialized bytes and `-1` reports an
/// I/O failure.
///
/// # Safety
///
/// `data` must either be null with a zero `capacity`, or point to `capacity`
/// writable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the I/O ABI fills a raw owned byte region from generated code"
)]
pub unsafe extern "C" fn input_read_to_end(data: *mut u8, capacity: usize) -> isize {
    // SAFETY: The public ABI contract guarantees exclusive access to this byte region.
    let Some(buffer) = (unsafe { bytes_from_raw_parts_mut(data, capacity) }) else {
        return IO_FAILURE_CODE;
    };
    read_to_end_into(&mut io::stdin().lock(), buffer).map_or(IO_FAILURE_CODE, count_as_io_result)
}

/// Reports whether standard input is attached to a terminal.
#[must_use]
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "the I/O ABI must expose a stable native symbol")]
pub extern "C" fn input_is_terminal() -> bool {
    io::stdin().is_terminal()
}

/// Compares two bounded byte views.
///
/// # Safety
///
/// Each pointer must either be null with a zero corresponding length, or point
/// to that many readable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the I/O ABI compares raw bounded byte views from generated code"
)]
pub unsafe extern "C" fn buffer_equals(
    left: *const u8,
    left_length: usize,
    right: *const u8,
    right_length: usize,
) -> bool {
    // SAFETY: The public ABI contract guarantees that both byte views are live.
    let Some(left) = (unsafe { bytes_from_raw_parts(left, left_length) }) else {
        return false;
    };
    // SAFETY: The public ABI contract guarantees that both byte views are live.
    let Some(right) = (unsafe { bytes_from_raw_parts(right, right_length) }) else {
        return false;
    };
    left == right
}

/// Copies one bounded byte region into another.
///
/// Returns zero on success and `-1` when either bounded view is invalid.
///
/// # Safety
///
/// `source` must identify `length` readable bytes and `destination` must
/// identify `length` writable bytes. The two regions must not overlap.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the runtime ABI copies between raw bounded byte regions"
)]
pub unsafe extern "C" fn copy_bytes(destination: *mut u8, source: *const u8, length: usize) -> i32 {
    // SAFETY: The public ABI contract guarantees a live readable source region.
    let Some(source) = (unsafe { bytes_from_raw_parts(source, length) }) else {
        return -1;
    };
    // SAFETY: The public ABI contract guarantees an exclusive writable destination region.
    let Some(destination) = (unsafe { bytes_from_raw_parts_mut(destination, length) }) else {
        return -1;
    };
    destination.copy_from_slice(source);
    0
}

/// Reports whether a bounded byte region contains valid UTF-8.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or identify `length`
/// readable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the runtime ABI validates a raw bounded byte region"
)]
pub unsafe extern "C" fn utf8_is_valid(data: *const u8, length: usize) -> bool {
    // SAFETY: The public ABI contract guarantees a live readable byte region.
    let Some(bytes) = (unsafe { bytes_from_raw_parts(data, length) }) else {
        return false;
    };
    std::str::from_utf8(bytes).is_ok()
}

/// Decodes one Unicode scalar at `offset` in a bounded UTF-8 byte region.
///
/// The low three result bits contain the consumed byte width and the remaining
/// bits contain the Unicode scalar. Zero denotes the end of the view or an
/// invalid boundary.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or identify `length`
/// readable bytes for the duration of this call.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the runtime ABI decodes a raw bounded UTF-8 view from generated code"
)]
pub unsafe extern "C" fn utf8_decode_next(data: *const u8, length: usize, offset: usize) -> u32 {
    // SAFETY: The public ABI contract guarantees a live readable byte region.
    let Some(bytes) = (unsafe { bytes_from_raw_parts(data, length) }) else {
        return 0;
    };
    let Some(suffix) = bytes.get(offset..) else {
        return 0;
    };
    let Ok(text) = std::str::from_utf8(suffix) else {
        return 0;
    };
    let Some(character) = text.chars().next() else {
        return 0;
    };
    let width = match character.len_utf8() {
        1 => 1_u32,
        2 => 2,
        3 => 3,
        4 => 4,
        _ => return 0,
    };
    (u32::from(character) << 3) | width
}

/// Returns the checked absolute value used by the FFI reference program.
#[must_use]
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the FFI reference symbol must use a stable unmangled C ABI"
)]
pub extern "C" fn absolute_i32(value: i32) -> i32 {
    value
        .checked_abs()
        .unwrap_or_else(|| runtime_fail(Failure::Overflow as u32))
}

fn write_output(stream: u8, bytes: &[u8]) -> io::Result<usize> {
    match stream {
        STANDARD_OUTPUT => io::stdout().lock().write(bytes),
        STANDARD_ERROR => io::stderr().lock().write(bytes),
        _ => Err(invalid_stream_error(stream)),
    }
}

fn write_all_output(stream: u8, bytes: &[u8], append_newline: bool) -> io::Result<()> {
    match stream {
        STANDARD_OUTPUT => {
            write_all_with_optional_newline(&mut io::stdout().lock(), bytes, append_newline)
        }
        STANDARD_ERROR => {
            write_all_with_optional_newline(&mut io::stderr().lock(), bytes, append_newline)
        }
        _ => Err(invalid_stream_error(stream)),
    }
}

fn write_all_with_optional_newline(
    output: &mut impl Write,
    bytes: &[u8],
    append_newline: bool,
) -> io::Result<()> {
    output.write_all(bytes)?;
    if append_newline {
        output.write_all(b"\n")?;
    }
    Ok(())
}

fn flush_output(stream: u8) -> io::Result<()> {
    match stream {
        STANDARD_OUTPUT => io::stdout().lock().flush(),
        STANDARD_ERROR => io::stderr().lock().flush(),
        _ => Err(invalid_stream_error(stream)),
    }
}

fn invalid_stream_error(stream: u8) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("invalid output stream {stream}"),
    )
}

fn read_line_into(input: &mut impl BufRead, destination: &mut [u8]) -> io::Result<usize> {
    let mut initialized = 0;
    while initialized < destination.len() {
        let (consumed, found_newline) = {
            let available = match input.fill_buf() {
                Ok(available) => available,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if available.is_empty() {
                return Ok(initialized);
            }
            let through_newline = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            let consumed = through_newline.min(destination.len() - initialized);
            destination[initialized..initialized + consumed]
                .copy_from_slice(&available[..consumed]);
            (
                consumed,
                consumed == through_newline && available[consumed - 1] == b'\n',
            )
        };
        input.consume(consumed);
        initialized += consumed;
        if found_newline {
            break;
        }
    }
    Ok(initialized)
}

fn read_to_end_into(input: &mut impl Read, destination: &mut [u8]) -> io::Result<usize> {
    let mut initialized = 0;
    while initialized < destination.len() {
        match input.read(&mut destination[initialized..]) {
            Ok(0) => break,
            Ok(count) => initialized += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(initialized)
}

fn count_as_io_result(count: usize) -> isize {
    isize::try_from(count).unwrap_or(IO_FAILURE_CODE)
}

#[expect(
    unsafe_code,
    reason = "this helper centralizes validation before constructing borrowed raw byte views"
)]
unsafe fn bytes_from_raw_parts<'data>(data: *const u8, length: usize) -> Option<&'data [u8]> {
    if length == 0 {
        return Some(&[]);
    }
    if data.is_null() {
        return None;
    }
    // SAFETY: The caller's ABI contract guarantees a live region of this exact length.
    Some(unsafe { std::slice::from_raw_parts(data, length) })
}

#[expect(
    unsafe_code,
    reason = "this helper centralizes validation before constructing mutable raw byte views"
)]
unsafe fn bytes_from_raw_parts_mut<'data>(data: *mut u8, length: usize) -> Option<&'data mut [u8]> {
    if length == 0 {
        return Some(&mut []);
    }
    if data.is_null() {
        return None;
    }
    // SAFETY: The caller's ABI contract guarantees exclusive access to this exact region.
    Some(unsafe { std::slice::from_raw_parts_mut(data, length) })
}

fn allocation_layout(allocator: usize, length: usize) -> Layout {
    match allocator {
        GENERAL_ALLOCATOR => Layout::array::<u8>(length.max(1))
            .unwrap_or_else(|_| runtime_fail(Failure::Overflow as u32)),
        PAGE_ALLOCATOR => {
            let rounded = length.max(1).checked_add(PAGE_GRANULARITY - 1).map_or_else(
                || runtime_fail(Failure::Overflow as u32),
                |value| value / PAGE_GRANULARITY * PAGE_GRANULARITY,
            );
            Layout::from_size_align(rounded, PAGE_GRANULARITY)
                .unwrap_or_else(|_| runtime_fail(Failure::Overflow as u32))
        }
        _ => runtime_fail(Failure::InvalidAllocator as u32),
    }
}

#[expect(
    unsafe_code,
    reason = "Rust's allocation primitive is wrapped behind validated runtime layouts"
)]
fn allocate_static(allocator: usize, length: usize) -> *mut u8 {
    let layout = allocation_layout(allocator, length);
    // SAFETY: `layout` is non-zero and was validated by `Layout`.
    unsafe { alloc::alloc(layout) }
}

#[expect(
    unsafe_code,
    reason = "the private helper centralizes the allocator provenance contract"
)]
unsafe fn deallocate_static(allocator: usize, data: *mut u8, length: usize) {
    let layout = allocation_layout(allocator, length);
    // SAFETY: The caller guarantees matching provenance, size, allocator, and liveness.
    unsafe { alloc::dealloc(data, layout) };
}

fn allocate_dynamic(allocator: usize, length: usize) -> *mut u8 {
    let mut allocators = lock_dynamic_allocators();
    let Some(dynamic) = allocators.get_mut(&allocator) else {
        runtime_fail(Failure::InvalidAllocator as u32);
    };
    match dynamic {
        DynamicAllocator::Arena {
            parent,
            allocations,
        } => {
            let data = allocate_static(*parent, length);
            if !data.is_null() {
                allocations.push(Allocation {
                    address: data as usize,
                    length,
                });
            }
            data
        }
        DynamicAllocator::Fixed {
            base,
            length: capacity,
            offset,
        } => {
            let requested = length.max(1);
            let Some(end) = offset.checked_add(requested) else {
                return std::ptr::null_mut();
            };
            if end > *capacity {
                return std::ptr::null_mut();
            }
            let Some(address) = base.checked_add(*offset) else {
                return std::ptr::null_mut();
            };
            *offset = end;
            address as *mut u8
        }
    }
}

fn validate_dynamic_deallocation(allocator: usize, data: *mut u8, length: usize) {
    let allocators = lock_dynamic_allocators();
    let Some(dynamic) = allocators.get(&allocator) else {
        runtime_fail(Failure::InvalidAllocator as u32);
    };
    let valid = match dynamic {
        DynamicAllocator::Arena { allocations, .. } => allocations
            .iter()
            .any(|allocation| allocation.address == data as usize && allocation.length == length),
        DynamicAllocator::Fixed {
            base,
            length: capacity,
            ..
        } => base
            .checked_add(*capacity)
            .is_some_and(|end| (data as usize) >= *base && (data as usize) < end),
    };
    if !valid {
        runtime_fail(Failure::InvalidAllocator as u32);
    }
}

fn insert_dynamic_allocator(allocator: DynamicAllocator) -> usize {
    let handle = NEXT_ALLOCATOR.fetch_add(1, Ordering::Relaxed);
    if handle < FIRST_DYNAMIC_ALLOCATOR {
        return 0;
    }
    lock_dynamic_allocators().insert(handle, allocator);
    handle
}

fn remove_dynamic_allocator(handle: usize) -> DynamicAllocator {
    lock_dynamic_allocators()
        .remove(&handle)
        .unwrap_or_else(|| runtime_fail(Failure::InvalidAllocator as u32))
}

fn lock_dynamic_allocators() -> std::sync::MutexGuard<'static, HashMap<usize, DynamicAllocator>> {
    DYNAMIC_ALLOCATORS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|_| runtime_fail(Failure::InvalidAllocator as u32))
}

fn failure_message(code: u32) -> &'static str {
    match code {
        value if value == Failure::Panic as u32 => "explicit panic",
        value if value == Failure::Bounds as u32 => "index out of bounds",
        value if value == Failure::Overflow as u32 => "integer overflow",
        value if value == Failure::DivisionByZero as u32 => "integer division by zero",
        value if value == Failure::InvalidShift as u32 => "invalid shift amount",
        value if value == Failure::NonExhaustiveMatch as u32 => {
            "non-exhaustive match reached the backend"
        }
        value if value == Failure::InvalidAllocator as u32 => "invalid allocator handle",
        value if value == Failure::InvalidSynchronization as u32 => {
            "invalid synchronization handle or value layout"
        }
        _ => "unknown runtime failure",
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::mem::size_of;

    use super::{
        Failure, GENERAL_ALLOCATOR, PAGE_ALLOCATOR, PAGE_GRANULARITY, THREAD_JOIN_OK,
        allocate_bytes, arena_allocator_deinit, arena_allocator_init, buffer_equals, copy_bytes,
        deallocate_bytes, failure_message, fixed_buffer_allocator_deinit,
        fixed_buffer_allocator_init, read_line_into, read_to_end_into, target_os_code, thread_join,
        thread_spawn, utf8_decode_next, utf8_is_valid, write_all_with_optional_newline,
    };

    #[expect(
        unsafe_code,
        reason = "the test thunk mirrors the compiler-generated thread ABI"
    )]
    unsafe extern "C" fn increment_thread_value(
        _callback: usize,
        argument: *const u8,
        result: *mut u8,
    ) {
        let mut bytes = [0_u8; size_of::<i32>()];
        // SAFETY: The test passes four readable argument bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(argument, bytes.as_mut_ptr(), bytes.len());
        }
        let value = i32::from_ne_bytes(bytes) + 1;
        let bytes = value.to_ne_bytes();
        // SAFETY: The test passes four writable result bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), result, bytes.len());
        }
    }

    #[test]
    fn division_by_zero_should_have_a_stable_message() {
        assert_eq!(
            failure_message(Failure::DivisionByZero as u32),
            "integer division by zero"
        );
    }

    #[test]
    fn unknown_failure_codes_should_have_a_stable_message() {
        assert_eq!(failure_message(u32::MAX), "unknown runtime failure");
    }

    #[test]
    fn target_os_code_should_match_the_compilation_host() {
        let expected = if cfg!(target_os = "windows") {
            0
        } else if cfg!(target_os = "linux") {
            1
        } else if cfg!(target_os = "macos") {
            2
        } else if cfg!(target_os = "freebsd") {
            3
        } else {
            4
        };

        assert_eq!(target_os_code(), expected);
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies exact buffers and a matching compiler-style thunk"
    )]
    fn native_thread_should_transfer_argument_and_result_bytes() {
        let argument = 41_i32;
        // SAFETY: The thunk and argument match the declared four-byte layouts.
        let handle = unsafe {
            thread_spawn(
                increment_thread_value as *const () as usize,
                1,
                (&raw const argument).cast(),
                size_of::<i32>(),
                size_of::<i32>(),
            )
        };
        let mut result = 0_i32;

        // SAFETY: `result` is a live aligned four-byte destination.
        let status = unsafe { thread_join(handle, (&raw mut result).cast(), size_of::<i32>()) };

        assert_eq!((status, result), (THREAD_JOIN_OK, 42));
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test releases the exact allocation returned by the runtime ABI"
    )]
    fn general_allocator_should_allocate_a_live_byte_region() {
        let data = allocate_bytes(GENERAL_ALLOCATOR, 16);

        assert!(!data.is_null());
        // SAFETY: `data` is live and uses the same handle and length.
        unsafe { deallocate_bytes(GENERAL_ALLOCATOR, data, 16) };
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test releases the exact allocation returned by the runtime ABI"
    )]
    fn page_allocator_should_return_page_aligned_memory() {
        let data = allocate_bytes(PAGE_ALLOCATOR, 16);

        assert!(!data.is_null() && (data as usize).is_multiple_of(PAGE_GRANULARITY));
        // SAFETY: `data` is live and uses the same handle and length.
        unsafe { deallocate_bytes(PAGE_ALLOCATOR, data, 16) };
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test releases a logical allocation before retiring its arena"
    )]
    fn arena_allocator_should_own_child_allocations_until_deinit() {
        let arena = arena_allocator_init(GENERAL_ALLOCATOR);
        let data = allocate_bytes(arena, 32);

        assert!(arena >= 3);
        assert!(!data.is_null());
        // SAFETY: The allocation belongs to this live arena and is logically released once.
        unsafe { deallocate_bytes(arena, data, 32) };
        arena_allocator_deinit(arena);
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test passes one live stack buffer through the fixed allocator ABI"
    )]
    fn fixed_allocator_should_report_out_of_memory_without_leaving_its_buffer() {
        let mut storage = [0_u8; 16];
        let fixed = fixed_buffer_allocator_init(storage.as_mut_ptr(), storage.len());
        let first = allocate_bytes(fixed, 12);
        let second = allocate_bytes(fixed, 8);

        assert_eq!(first, storage.as_mut_ptr());
        assert!(second.is_null());
        // SAFETY: `first` identifies the logical allocation returned from this fixed buffer.
        unsafe { deallocate_bytes(fixed, first, 12) };
        fixed_buffer_allocator_deinit(fixed);
    }

    #[test]
    fn line_reader_should_retain_newline_and_leave_following_input_buffered() {
        let mut input = Cursor::new(b"first\nsecond");
        let mut first = [0_u8; 16];
        let mut second = [0_u8; 16];

        let first_length = read_line_into(&mut input, &mut first).unwrap();
        let second_length = read_line_into(&mut input, &mut second).unwrap();

        assert_eq!(&first[..first_length], b"first\n");
        assert_eq!(&second[..second_length], b"second");
    }

    #[test]
    fn line_reader_should_stop_at_capacity_without_discarding_input() {
        let mut input = Cursor::new(b"abcdef\n");
        let mut first = [0_u8; 3];
        let mut second = [0_u8; 8];

        let first_length = read_line_into(&mut input, &mut first).unwrap();
        let second_length = read_line_into(&mut input, &mut second).unwrap();

        assert_eq!(&first[..first_length], b"abc");
        assert_eq!(&second[..second_length], b"def\n");
    }

    #[test]
    fn read_to_end_should_stop_at_destination_capacity() {
        let mut input = Cursor::new(b"abcdef");
        let mut destination = [0_u8; 4];

        let length = read_to_end_into(&mut input, &mut destination).unwrap();

        assert_eq!(length, destination.len());
        assert_eq!(&destination, b"abcd");
    }

    #[test]
    fn complete_output_should_append_one_optional_newline() {
        let mut destination = Vec::new();

        write_all_with_optional_newline(&mut destination, b"hello", true).unwrap();

        assert_eq!(destination, b"hello\n");
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies live byte views to the runtime comparison ABI"
    )]
    fn bounded_buffer_comparison_should_respect_lengths() {
        let left = b"answer";
        let same_prefix = b"answer-extra";

        // SAFETY: Both pointers refer to live arrays for the supplied lengths.
        assert!(unsafe {
            buffer_equals(left.as_ptr(), left.len(), same_prefix.as_ptr(), left.len())
        });
        // SAFETY: Both pointers refer to live arrays for the supplied lengths.
        assert!(!unsafe {
            buffer_equals(
                left.as_ptr(),
                left.len(),
                same_prefix.as_ptr(),
                same_prefix.len(),
            )
        });
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies two disjoint live byte regions to the runtime ABI"
    )]
    fn bounded_copy_should_initialize_the_destination() {
        let source = *b"typed";
        let mut destination = [0_u8; 5];

        // SAFETY: Both arrays are live, disjoint, and exactly five bytes long.
        let status =
            unsafe { copy_bytes(destination.as_mut_ptr(), source.as_ptr(), destination.len()) };

        assert_eq!(status, 0);
        assert_eq!(destination, source);
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies live byte regions to the UTF-8 validation ABI"
    )]
    fn utf8_validation_should_reject_invalid_sequences() {
        let valid = "typed".as_bytes();
        let invalid = [0xC3_u8, 0x28];

        // SAFETY: Both slices remain live for their exact supplied lengths.
        assert!(unsafe { utf8_is_valid(valid.as_ptr(), valid.len()) });
        // SAFETY: Both slices remain live for their exact supplied lengths.
        assert!(!unsafe { utf8_is_valid(invalid.as_ptr(), invalid.len()) });
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies a live string view to the UTF-8 decoding ABI"
    )]
    fn utf8_decoder_should_report_scalars_and_byte_widths() {
        let text = "Aé🦀\0";
        let mut offset = 0;
        let expected = [('A', 1_u32), ('é', 2), ('🦀', 4), ('\0', 1)];

        for (character, width) in expected {
            // SAFETY: `text` remains live for its exact supplied byte length.
            let decoded = unsafe { utf8_decode_next(text.as_ptr(), text.len(), offset) };
            assert_eq!(decoded & 0b111, width);
            assert_eq!(decoded >> 3, u32::from(character));
            offset += usize::try_from(width).expect("UTF-8 width should fit usize");
        }

        // SAFETY: `offset` is exactly at the end of the same live string.
        assert_eq!(
            unsafe { utf8_decode_next(text.as_ptr(), text.len(), offset) },
            0
        );
        // SAFETY: The byte region is live; the offset intentionally targets a continuation byte.
        assert_eq!(unsafe { utf8_decode_next(text.as_ptr(), text.len(), 2) }, 0);
    }
}
