//! Fixed-worker job pools with worker-local queues and work stealing.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;

use super::{AlignedBytes, ExecutionSession, active_execution_session};

/// ABI symbol used to create a fixed-worker job pool.
pub const JOB_POOL_CREATE_SYMBOL: &str = "job_pool_create";
/// ABI symbol used to clone ownership of a job pool.
pub const JOB_POOL_CLONE_SYMBOL: &str = "job_pool_clone";
/// ABI symbol used to stop and retire a job pool.
pub const JOB_POOL_DESTROY_SYMBOL: &str = "job_pool_destroy";
/// ABI symbol used to submit one typed callback job.
pub const JOB_SUBMIT_SYMBOL: &str = "job_submit";
/// ABI symbol used to wait for and transfer one job result.
pub const JOB_WAIT_SYMBOL: &str = "job_wait";
/// ABI symbol used for scoped parallel iteration over disjoint mutable chunks.
pub const JOB_PARALLEL_FOR_SYMBOL: &str = "job_parallel_for";

/// Job result transfer completed successfully.
pub const JOB_JOIN_OK: i32 = 0;
/// The supplied job handle is not live.
pub const JOB_JOIN_INVALID_HANDLE: i32 = 1;
/// The worker panicked while executing the job.
pub const JOB_JOIN_WORKER_PANICKED: i32 = 2;
/// The destination layout does not match the submitted result type.
pub const JOB_JOIN_RESULT_MISMATCH: i32 = 3;

/// Stable machine-word request consumed by [`job_parallel_for`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ParallelForRequest {
    /// Opaque job-pool handle.
    pub pool: usize,
    /// Compiler-generated callback thunk address.
    pub thunk: usize,
    /// Typed source callback address.
    pub callback: usize,
    /// First element of the exclusive mutable input slice.
    pub data: usize,
    /// Number of input elements.
    pub length: usize,
    /// Native byte stride of one input element.
    pub element_size: usize,
    /// Smallest preferred number of elements per task.
    pub minimum_chunk: usize,
    /// Native size of the slice descriptor passed to the callback.
    pub descriptor_size: usize,
    /// Byte offset of the descriptor data pointer.
    pub data_offset: usize,
    /// Byte offset of the descriptor element count.
    pub length_offset: usize,
}

const FIRST_POOL_HANDLE: usize = 1;
const FIRST_JOB_HANDLE: usize = 1;

static NEXT_POOL_HANDLE: AtomicUsize = AtomicUsize::new(FIRST_POOL_HANDLE);
static NEXT_JOB_HANDLE: AtomicUsize = AtomicUsize::new(FIRST_JOB_HANDLE);
static POOLS: OnceLock<Mutex<HashMap<usize, Arc<JobPoolState>>>> = OnceLock::new();
static JOBS: OnceLock<Mutex<HashMap<usize, Arc<JobCompletion>>>> = OnceLock::new();

type JobThunk = unsafe extern "C" fn(usize, *const u8, *mut u8);

struct JobPoolState {
    session: usize,
    queues: Vec<Mutex<VecDeque<JobTask>>>,
    pending: AtomicUsize,
    next_queue: AtomicUsize,
    shutting_down: AtomicBool,
    owners: AtomicUsize,
    lifecycle: Mutex<()>,
    wake_gate: Mutex<()>,
    wake_workers: Condvar,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

struct JobTask {
    thunk: usize,
    callback: usize,
    argument: AlignedBytes,
    result_size: usize,
    completion: Arc<JobCompletion>,
}

struct JobCompletion {
    session: usize,
    result_size: usize,
    outcome: Mutex<Option<JobOutcome>>,
    ready: Condvar,
}

enum JobOutcome {
    Completed(AlignedBytes),
    WorkerPanicked,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wait<'guard, T>(condition: &Condvar, guard: MutexGuard<'guard, T>) -> MutexGuard<'guard, T> {
    condition
        .wait(guard)
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn pools() -> &'static Mutex<HashMap<usize, Arc<JobPoolState>>> {
    POOLS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn jobs() -> &'static Mutex<HashMap<usize, Arc<JobCompletion>>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_nonzero(counter: &AtomicUsize) -> Option<usize> {
    let handle = counter.fetch_add(1, Ordering::Relaxed);
    (handle != 0).then_some(handle)
}

fn worker_loop(pool: &Arc<JobPoolState>, queue_index: usize) {
    let _session = ExecutionSession::activate(pool.session);
    loop {
        if let Some(task) = take_task(pool, queue_index) {
            pool.pending.fetch_sub(1, Ordering::AcqRel);
            execute_task(&task);
            continue;
        }
        if pool.shutting_down.load(Ordering::Acquire) && pool.pending.load(Ordering::Acquire) == 0 {
            break;
        }
        let mut gate = lock(&pool.wake_gate);
        while pool.pending.load(Ordering::Acquire) == 0
            && !pool.shutting_down.load(Ordering::Acquire)
        {
            gate = wait(&pool.wake_workers, gate);
        }
    }
}

fn take_task(pool: &JobPoolState, queue_index: usize) -> Option<JobTask> {
    if let Some(task) = lock(&pool.queues[queue_index]).pop_front() {
        return Some(task);
    }
    for offset in 1..pool.queues.len() {
        let victim = (queue_index + offset) % pool.queues.len();
        if let Some(task) = lock(&pool.queues[victim]).pop_back() {
            return Some(task);
        }
    }
    None
}

fn execute_task(task: &JobTask) {
    let mut result = AlignedBytes::zeroed(task.result_size);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: Submission validates a nonzero compiler-generated thunk and
        // matching callback. Their code remains live until the pool is stopped.
        let thunk = unsafe { std::mem::transmute::<usize, JobThunk>(task.thunk) };
        // SAFETY: The task buffers use the layouts encoded in the thunk.
        unsafe {
            thunk(task.callback, task.argument.as_ptr(), result.as_mut_ptr());
        }
    }))
    .map_or(JobOutcome::WorkerPanicked, |()| {
        JobOutcome::Completed(result)
    });
    *lock(&task.completion.outcome) = Some(outcome);
    task.completion.ready.notify_all();
}

fn enqueue_task(pool: &JobPoolState, task: JobTask) {
    let queue = pool.next_queue.fetch_add(1, Ordering::Relaxed) % pool.queues.len();
    lock(&pool.queues[queue]).push_front(task);
    pool.pending.fetch_add(1, Ordering::Release);
}

fn wait_for_job(completion: &JobCompletion) -> JobOutcome {
    let mut outcome = lock(&completion.outcome);
    while outcome.is_none() {
        outcome = wait(&completion.ready, outcome);
    }
    outcome.take().unwrap_or(JobOutcome::WorkerPanicked)
}

fn stop_pool(pool: &JobPoolState) {
    {
        let _lifecycle = lock(&pool.lifecycle);
        pool.shutting_down.store(true, Ordering::Release);
    }
    pool.wake_workers.notify_all();
    let current = std::thread::current().id();
    for worker in std::mem::take(&mut *lock(&pool.workers)) {
        if worker.thread().id() != current {
            let _ = worker.join();
        }
    }
}

/// Creates a job pool with exactly `worker_count` persistent native workers.
///
/// Returns zero for a zero worker count or if an OS worker cannot be created.
#[unsafe(no_mangle)]
pub extern "C" fn job_pool_create(worker_count: usize) -> usize {
    if worker_count == 0 {
        return 0;
    }
    let Some(handle) = next_nonzero(&NEXT_POOL_HANDLE) else {
        return 0;
    };
    let pool = Arc::new(JobPoolState {
        session: active_execution_session(),
        queues: (0..worker_count)
            .map(|_| Mutex::new(VecDeque::new()))
            .collect(),
        pending: AtomicUsize::new(0),
        next_queue: AtomicUsize::new(0),
        shutting_down: AtomicBool::new(false),
        owners: AtomicUsize::new(1),
        lifecycle: Mutex::new(()),
        wake_gate: Mutex::new(()),
        wake_workers: Condvar::new(),
        workers: Mutex::new(Vec::with_capacity(worker_count)),
    });
    for index in 0..worker_count {
        let worker_pool = Arc::clone(&pool);
        let worker = std::thread::Builder::new()
            .name(format!("job-worker-{index}"))
            .spawn(move || worker_loop(&worker_pool, index));
        let Ok(worker) = worker else {
            stop_pool(&pool);
            return 0;
        };
        lock(&pool.workers).push(worker);
    }
    lock(pools()).insert(handle, pool);
    handle
}

/// Clones shared ownership of one fixed-worker pool.
#[unsafe(no_mangle)]
pub extern "C" fn job_pool_clone(handle: usize) -> usize {
    let Some(pool) = lock(pools()).get(&handle).cloned() else {
        return 0;
    };
    let Some(cloned_handle) = next_nonzero(&NEXT_POOL_HANDLE) else {
        return 0;
    };
    pool.owners.fetch_add(1, Ordering::Relaxed);
    lock(pools()).insert(cloned_handle, pool);
    cloned_handle
}

/// Stops a pool after all submitted jobs finish and retires one owner handle.
#[unsafe(no_mangle)]
pub extern "C" fn job_pool_destroy(handle: usize) -> i32 {
    let Some(pool) = lock(pools()).remove(&handle) else {
        return JOB_JOIN_INVALID_HANDLE;
    };
    if pool.owners.fetch_sub(1, Ordering::AcqRel) == 1 {
        stop_pool(&pool);
    }
    JOB_JOIN_OK
}

/// Submits one job to a worker-local queue.
///
/// Returns zero for an invalid or stopped pool, invalid callback addresses, or
/// an invalid argument buffer.
///
/// # Safety
///
/// `thunk` must be a live compiler-generated [`JobThunk`] address and
/// `callback` must match its encoded signature. When `argument_size` is
/// nonzero, `argument` must identify that many readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn job_submit(
    pool_handle: usize,
    thunk: usize,
    callback: usize,
    argument: *const u8,
    argument_size: usize,
    result_size: usize,
) -> usize {
    if thunk == 0 || callback == 0 || (argument_size != 0 && argument.is_null()) {
        return 0;
    }
    let Some(pool) = lock(pools()).get(&pool_handle).cloned() else {
        return 0;
    };
    let _lifecycle = lock(&pool.lifecycle);
    if pool.shutting_down.load(Ordering::Acquire) {
        return 0;
    }
    // SAFETY: Forwarded from this function's ABI contract.
    let Some(argument) = (unsafe { copy_argument(argument, argument_size) }) else {
        return 0;
    };
    let Some(job_handle) = next_nonzero(&NEXT_JOB_HANDLE) else {
        return 0;
    };
    let completion = Arc::new(JobCompletion {
        session: pool.session,
        result_size,
        outcome: Mutex::new(None),
        ready: Condvar::new(),
    });
    lock(jobs()).insert(job_handle, Arc::clone(&completion));
    enqueue_task(
        &pool,
        JobTask {
            thunk,
            callback,
            argument,
            result_size,
            completion,
        },
    );
    pool.wake_workers.notify_one();
    job_handle
}

/// Applies one callback to nonoverlapping mutable slice chunks and waits for
/// every submitted task before returning.
///
/// This is the scoped runtime foundation for slice, array, and tensor
/// `parallel_for_mut` APIs.
///
/// # Safety
///
/// `request` must point to one live [`ParallelForRequest`]. Its code addresses,
/// mutable element region, element stride, and descriptor layout must match the
/// compiler-generated callback thunk. The element region must remain exclusively
/// borrowed until this call returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn job_parallel_for(request: *const ParallelForRequest) -> i32 {
    if request.is_null() {
        return JOB_JOIN_RESULT_MISMATCH;
    }
    // SAFETY: Forwarded from this function's ABI contract.
    let request = unsafe { request.read() };
    if !valid_parallel_request(request) {
        return JOB_JOIN_RESULT_MISMATCH;
    }
    let Some(pool) = lock(pools()).get(&request.pool).cloned() else {
        return JOB_JOIN_INVALID_HANDLE;
    };
    let lifecycle = lock(&pool.lifecycle);
    if pool.shutting_down.load(Ordering::Acquire) {
        return JOB_JOIN_INVALID_HANDLE;
    }
    if request.length == 0 {
        return JOB_JOIN_OK;
    }

    let target_tasks = pool.queues.len().saturating_mul(4).max(1);
    let balanced_chunk = request.length.div_ceil(target_tasks);
    let chunk_size = request.minimum_chunk.max(1).max(balanced_chunk);
    let mut tasks = Vec::with_capacity(request.length.div_ceil(chunk_size));
    let mut completions = Vec::with_capacity(tasks.capacity());
    let mut start = 0;
    while start < request.length {
        let length = (request.length - start).min(chunk_size);
        let Some(argument) = build_slice_descriptor(request, start, length) else {
            return JOB_JOIN_RESULT_MISMATCH;
        };
        let completion = Arc::new(JobCompletion {
            session: pool.session,
            result_size: 0,
            outcome: Mutex::new(None),
            ready: Condvar::new(),
        });
        tasks.push(JobTask {
            thunk: request.thunk,
            callback: request.callback,
            argument,
            result_size: 0,
            completion: Arc::clone(&completion),
        });
        completions.push(completion);
        start += length;
    }
    for task in tasks {
        enqueue_task(&pool, task);
    }
    pool.wake_workers.notify_all();
    drop(lifecycle);

    for completion in completions {
        if matches!(wait_for_job(&completion), JobOutcome::WorkerPanicked) {
            return JOB_JOIN_WORKER_PANICKED;
        }
    }
    JOB_JOIN_OK
}

fn valid_parallel_request(request: ParallelForRequest) -> bool {
    let word_size = std::mem::size_of::<usize>();
    let data_end = request.data_offset.checked_add(word_size);
    let length_end = request.length_offset.checked_add(word_size);
    request.thunk != 0
        && request.callback != 0
        && request.element_size != 0
        && (request.length == 0 || request.data != 0)
        && request
            .length
            .checked_mul(request.element_size)
            .and_then(|bytes| request.data.checked_add(bytes))
            .is_some()
        && data_end.is_some_and(|end| end <= request.descriptor_size)
        && length_end.is_some_and(|end| end <= request.descriptor_size)
}

fn build_slice_descriptor(
    request: ParallelForRequest,
    start: usize,
    length: usize,
) -> Option<AlignedBytes> {
    let byte_offset = start.checked_mul(request.element_size)?;
    let data = request.data.checked_add(byte_offset)?;
    let mut descriptor = AlignedBytes::zeroed(request.descriptor_size);
    write_descriptor_word(&mut descriptor, request.data_offset, data)?;
    write_descriptor_word(&mut descriptor, request.length_offset, length)?;
    Some(descriptor)
}

fn write_descriptor_word(descriptor: &mut AlignedBytes, offset: usize, value: usize) -> Option<()> {
    let end = offset.checked_add(std::mem::size_of::<usize>())?;
    if end > descriptor.length {
        return None;
    }
    // SAFETY: Bounds above constrain the mutable byte view to owned storage.
    let bytes =
        unsafe { std::slice::from_raw_parts_mut(descriptor.as_mut_ptr(), descriptor.length) };
    bytes[offset..end].copy_from_slice(&value.to_ne_bytes());
    Some(())
}

/// Waits for one job and transfers its result into compiler-owned storage.
///
/// # Safety
///
/// When `result_size` is nonzero, `destination` must identify that many
/// writable bytes aligned for the callback result type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn job_wait(handle: usize, destination: *mut u8, result_size: usize) -> i32 {
    let Some(completion) = lock(jobs()).remove(&handle) else {
        return JOB_JOIN_INVALID_HANDLE;
    };
    if result_size != completion.result_size || (result_size != 0 && destination.is_null()) {
        return JOB_JOIN_RESULT_MISMATCH;
    }
    let outcome = wait_for_job(&completion);
    let JobOutcome::Completed(result) = outcome else {
        return JOB_JOIN_WORKER_PANICKED;
    };
    if result.length != result_size {
        return JOB_JOIN_RESULT_MISMATCH;
    }
    if result_size != 0 {
        // SAFETY: The ABI contract supplies exactly `result_size` writable bytes.
        let destination = unsafe { std::slice::from_raw_parts_mut(destination, result_size) };
        // SAFETY: The worker result owns exactly `result_size` initialized bytes.
        let source = unsafe { std::slice::from_raw_parts(result.as_ptr(), result_size) };
        destination.copy_from_slice(source);
    }
    JOB_JOIN_OK
}

/// Stops every live pool before generated executable memory is released.
pub fn shutdown_all_job_pools() {
    let live_pools = std::mem::take(&mut *lock(pools()));
    for pool in live_pools.into_values() {
        stop_pool(&pool);
    }
    lock(jobs()).clear();
}

/// Stops job pools and discards completed jobs owned by one JIT execution.
pub fn shutdown_job_pools(session: usize) {
    let live_pools = {
        let mut registry = lock(pools());
        registry
            .extract_if(|_, pool| pool.session == session)
            .map(|(_, pool)| pool)
            .collect::<Vec<_>>()
    };
    for pool in live_pools {
        stop_pool(&pool);
    }
    lock(jobs()).retain(|_, completion| completion.session != session);
}

#[expect(
    unsafe_code,
    reason = "the job ABI copies from a compiler-validated argument buffer"
)]
unsafe fn copy_argument(source: *const u8, size: usize) -> Option<AlignedBytes> {
    if size != 0 && source.is_null() {
        return None;
    }
    let mut argument = AlignedBytes::zeroed(size);
    if size != 0 {
        // SAFETY: The caller promises `size` readable bytes.
        let source = unsafe { std::slice::from_raw_parts(source, size) };
        // SAFETY: `argument` owns at least `size` writable bytes.
        let destination = unsafe { std::slice::from_raw_parts_mut(argument.as_mut_ptr(), size) };
        destination.copy_from_slice(source);
    }
    Some(argument)
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::{
        JOB_JOIN_OK, ParallelForRequest, job_parallel_for, job_pool_create, job_pool_destroy,
        job_submit, job_wait, shutdown_all_job_pools, shutdown_job_pools,
    };
    use crate::ExecutionSession;

    unsafe extern "C" fn increment_thunk(callback: usize, argument: *const u8, result: *mut u8) {
        // SAFETY: The test submits this exact callback signature.
        let callback = unsafe { std::mem::transmute::<usize, fn(i32) -> i32>(callback) };
        let mut input = [0_u8; size_of::<i32>()];
        // SAFETY: The test supplies four readable argument bytes.
        unsafe { argument.copy_to_nonoverlapping(input.as_mut_ptr(), input.len()) };
        let output = callback(i32::from_ne_bytes(input)).to_ne_bytes();
        // SAFETY: The test supplies four writable result bytes.
        unsafe { output.as_ptr().copy_to_nonoverlapping(result, output.len()) };
    }

    fn increment(value: i32) -> i32 {
        value + 1
    }

    unsafe extern "C" fn increment_slice_thunk(
        _callback: usize,
        argument: *const u8,
        _result: *mut u8,
    ) {
        let mut data_bytes = [0_u8; size_of::<usize>()];
        let mut length_bytes = [0_u8; size_of::<usize>()];
        // SAFETY: The parallel runtime supplies a two-word slice descriptor.
        unsafe {
            argument.copy_to_nonoverlapping(data_bytes.as_mut_ptr(), data_bytes.len());
            argument
                .add(size_of::<usize>())
                .copy_to_nonoverlapping(length_bytes.as_mut_ptr(), length_bytes.len());
        }
        let data = usize::from_ne_bytes(data_bytes) as *mut i32;
        let length = usize::from_ne_bytes(length_bytes);
        // SAFETY: Each descriptor identifies one exclusive nonoverlapping chunk.
        let values = unsafe { std::slice::from_raw_parts_mut(data, length) };
        for value in values {
            *value += 1;
        }
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies exact compiler-style job buffers and callback addresses"
    )]
    fn workers_should_complete_jobs_from_local_and_stolen_queues() {
        let pool = job_pool_create(3);
        let mut handles = Vec::new();
        for value in 0_i32..24 {
            // SAFETY: All code and data addresses match `increment_thunk`.
            let handle = unsafe {
                job_submit(
                    pool,
                    increment_thunk as *const () as usize,
                    increment as *const () as usize,
                    (&raw const value).cast(),
                    size_of::<i32>(),
                    size_of::<i32>(),
                )
            };
            handles.push((handle, value + 1));
        }
        for (handle, expected) in handles {
            let mut result = 0_i32;
            // SAFETY: `result` is a live, aligned four-byte destination.
            let status = unsafe { job_wait(handle, (&raw mut result).cast(), size_of::<i32>()) };
            assert_eq!((status, result), (JOB_JOIN_OK, expected));
        }
        assert_eq!(job_pool_destroy(pool), JOB_JOIN_OK);
        shutdown_all_job_pools();
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies exact compiler-style slice descriptors"
    )]
    fn parallel_for_should_partition_one_exclusive_mutable_slice() {
        let pool = job_pool_create(4);
        let mut values = [1_i32; 64];
        let request = ParallelForRequest {
            pool,
            thunk: increment_slice_thunk as *const () as usize,
            callback: increment as *const () as usize,
            data: values.as_mut_ptr() as usize,
            length: values.len(),
            element_size: size_of::<i32>(),
            minimum_chunk: 3,
            descriptor_size: size_of::<usize>() * 2,
            data_offset: 0,
            length_offset: size_of::<usize>(),
        };
        // SAFETY: `request` describes the live exclusive `values` region.
        let status = unsafe { job_parallel_for(&raw const request) };

        assert_eq!(status, JOB_JOIN_OK);
        assert!(values.iter().all(|value| *value == 2));
        assert_eq!(job_pool_destroy(pool), JOB_JOIN_OK);
        shutdown_all_job_pools();
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies exact compiler-style job buffers and callback addresses"
    )]
    fn session_cleanup_should_leave_other_execution_pools_running() {
        let first_session = ExecutionSession::begin();
        let first_id = first_session.id();
        let first_pool = job_pool_create(1);
        drop(first_session);

        let second_session = ExecutionSession::begin();
        let second_id = second_session.id();
        let second_pool = job_pool_create(1);
        shutdown_job_pools(first_id);
        let value = 41_i32;
        // SAFETY: All code and data addresses match `increment_thunk`.
        let handle = unsafe {
            job_submit(
                second_pool,
                increment_thunk as *const () as usize,
                increment as *const () as usize,
                (&raw const value).cast(),
                size_of::<i32>(),
                size_of::<i32>(),
            )
        };
        let mut result = 0_i32;
        // SAFETY: `result` is a live, aligned four-byte destination.
        let status = unsafe { job_wait(handle, (&raw mut result).cast(), size_of::<i32>()) };
        shutdown_job_pools(second_id);
        drop(second_session);

        assert_eq!((first_pool != 0, status, result), (true, JOB_JOIN_OK, 42));
    }
}
