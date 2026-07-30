//! Opaque synchronization resources used by generated native programs.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{
    Arc, Barrier, Condvar, Mutex, MutexGuard, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard,
};
use std::thread::ThreadId;

use super::AlignedBytes;

/// Synchronization operation completed successfully.
pub const SYNC_OK: i32 = 0;
/// The supplied synchronization handle is not live.
pub const SYNC_INVALID_HANDLE: i32 = 1;
/// A channel is closed.
pub const SYNC_CLOSED: i32 = 2;

/// ABI symbol used to create a byte-backed mutex.
pub const MUTEX_CREATE_SYMBOL: &str = "mutex_create";
/// ABI symbol used to clone a mutex handle.
pub const MUTEX_CLONE_SYMBOL: &str = "mutex_clone";
/// ABI symbol used to load a mutex-protected value.
pub const MUTEX_LOAD_SYMBOL: &str = "mutex_load";
/// ABI symbol used to replace a mutex-protected value.
pub const MUTEX_REPLACE_SYMBOL: &str = "mutex_replace";
/// ABI symbol used to retire a mutex handle.
pub const MUTEX_DESTROY_SYMBOL: &str = "mutex_destroy";
/// ABI symbol used to create a byte-backed reader-writer lock.
pub const RWLOCK_CREATE_SYMBOL: &str = "rwlock_create";
/// ABI symbol used to clone a reader-writer lock handle.
pub const RWLOCK_CLONE_SYMBOL: &str = "rwlock_clone";
/// ABI symbol used to load a reader-writer-lock-protected value.
pub const RWLOCK_LOAD_SYMBOL: &str = "rwlock_load";
/// ABI symbol used to replace a reader-writer-lock-protected value.
pub const RWLOCK_REPLACE_SYMBOL: &str = "rwlock_replace";
/// ABI symbol used to retire a reader-writer lock handle.
pub const RWLOCK_DESTROY_SYMBOL: &str = "rwlock_destroy";
/// ABI symbol used to create a bounded channel.
pub const CHANNEL_CREATE_SYMBOL: &str = "channel_create";
/// ABI symbol used to clone a channel handle.
pub const CHANNEL_CLONE_SYMBOL: &str = "channel_clone";
/// ABI symbol used for a blocking channel send.
pub const CHANNEL_SEND_SYMBOL: &str = "channel_send";
/// ABI symbol used for a blocking channel receive.
pub const CHANNEL_RECEIVE_SYMBOL: &str = "channel_receive";
/// ABI symbol used to close a channel.
pub const CHANNEL_CLOSE_SYMBOL: &str = "channel_close";
/// ABI symbol used to retire a channel handle.
pub const CHANNEL_DESTROY_SYMBOL: &str = "channel_destroy";
/// ABI symbol used to create a reusable barrier.
pub const BARRIER_CREATE_SYMBOL: &str = "barrier_create";
/// ABI symbol used to clone a barrier handle.
pub const BARRIER_CLONE_SYMBOL: &str = "barrier_clone";
/// ABI symbol used to wait at a barrier.
pub const BARRIER_WAIT_SYMBOL: &str = "barrier_wait";
/// ABI symbol used to retire a barrier handle.
pub const BARRIER_DESTROY_SYMBOL: &str = "barrier_destroy";
/// ABI symbol used to create a counting semaphore.
pub const SEMAPHORE_CREATE_SYMBOL: &str = "semaphore_create";
/// ABI symbol used to clone a semaphore handle.
pub const SEMAPHORE_CLONE_SYMBOL: &str = "semaphore_clone";
/// ABI symbol used for a blocking semaphore acquire.
pub const SEMAPHORE_ACQUIRE_SYMBOL: &str = "semaphore_acquire";
/// ABI symbol used for a non-blocking semaphore acquire.
pub const SEMAPHORE_TRY_ACQUIRE_SYMBOL: &str = "semaphore_try_acquire";
/// ABI symbol used to release semaphore permits.
pub const SEMAPHORE_RELEASE_SYMBOL: &str = "semaphore_release";
/// ABI symbol used to retire a semaphore handle.
pub const SEMAPHORE_DESTROY_SYMBOL: &str = "semaphore_destroy";
/// ABI symbol used to create one sequentially consistent atomic cell.
pub const ATOMIC_CREATE_SYMBOL: &str = "atomic_create";
/// ABI symbol used to clone an atomic handle.
pub const ATOMIC_CLONE_SYMBOL: &str = "atomic_clone";
/// ABI symbol used for an atomic load.
pub const ATOMIC_LOAD_SYMBOL: &str = "atomic_load";
/// ABI symbol used for an atomic store.
pub const ATOMIC_STORE_SYMBOL: &str = "atomic_store";
/// ABI symbol used for an atomic swap.
pub const ATOMIC_SWAP_SYMBOL: &str = "atomic_swap";
/// ABI symbol used for an atomic fetch-add.
pub const ATOMIC_FETCH_ADD_SYMBOL: &str = "atomic_fetch_add";
/// ABI symbol used for an atomic compare-exchange.
pub const ATOMIC_COMPARE_EXCHANGE_SYMBOL: &str = "atomic_compare_exchange";
/// ABI symbol used to retire an atomic handle.
pub const ATOMIC_DESTROY_SYMBOL: &str = "atomic_destroy";
/// ABI symbol used to create copy-only thread-local storage.
pub const THREAD_LOCAL_CREATE_SYMBOL: &str = "thread_local_create";
/// ABI symbol used to clone a thread-local handle.
pub const THREAD_LOCAL_CLONE_SYMBOL: &str = "thread_local_clone";
/// ABI symbol used to read the current thread's value.
pub const THREAD_LOCAL_GET_SYMBOL: &str = "thread_local_get";
/// ABI symbol used to replace the current thread's value.
pub const THREAD_LOCAL_SET_SYMBOL: &str = "thread_local_set";
/// ABI symbol used to retire a thread-local handle.
pub const THREAD_LOCAL_DESTROY_SYMBOL: &str = "thread_local_destroy";

const FIRST_RESOURCE_HANDLE: usize = 1;

static NEXT_RESOURCE_HANDLE: AtomicUsize = AtomicUsize::new(FIRST_RESOURCE_HANDLE);
static MUTEXES: OnceLock<Mutex<HashMap<usize, Arc<Mutex<AlignedBytes>>>>> = OnceLock::new();
static RWLOCKS: OnceLock<Mutex<HashMap<usize, Arc<RwLock<AlignedBytes>>>>> = OnceLock::new();
static CHANNELS: OnceLock<Mutex<HashMap<usize, Arc<ChannelState>>>> = OnceLock::new();
static BARRIERS: OnceLock<Mutex<HashMap<usize, Arc<Barrier>>>> = OnceLock::new();
static SEMAPHORES: OnceLock<Mutex<HashMap<usize, Arc<SemaphoreState>>>> = OnceLock::new();
static ATOMICS: OnceLock<Mutex<HashMap<usize, Arc<AtomicU64>>>> = OnceLock::new();
static THREAD_LOCALS: OnceLock<Mutex<HashMap<usize, Arc<ThreadLocalState>>>> = OnceLock::new();

struct ChannelState {
    capacity: usize,
    element_size: usize,
    inner: Mutex<ChannelInner>,
    available: Condvar,
    space: Condvar,
}

struct ChannelInner {
    queue: VecDeque<AlignedBytes>,
    closed: bool,
}

struct SemaphoreState {
    permits: Mutex<usize>,
    available: Condvar,
}

struct ThreadLocalState {
    initial: AlignedBytes,
    values: Mutex<HashMap<ThreadId, AlignedBytes>>,
}

fn next_handle() -> usize {
    NEXT_RESOURCE_HANDLE.fetch_add(1, Ordering::Relaxed)
}

fn registry<T>(
    storage: &'static OnceLock<Mutex<HashMap<usize, Arc<T>>>>,
) -> &'static Mutex<HashMap<usize, Arc<T>>> {
    storage.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read<T>(rwlock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    rwlock
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write<T>(rwlock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    rwlock
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wait<'guard, T>(condition: &Condvar, guard: MutexGuard<'guard, T>) -> MutexGuard<'guard, T> {
    condition
        .wait(guard)
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn insert<T>(storage: &'static OnceLock<Mutex<HashMap<usize, Arc<T>>>>, value: Arc<T>) -> usize {
    let handle = next_handle();
    if handle != 0 {
        lock(registry(storage)).insert(handle, value);
    }
    handle
}

fn get<T>(
    storage: &'static OnceLock<Mutex<HashMap<usize, Arc<T>>>>,
    handle: usize,
) -> Option<Arc<T>> {
    lock(registry(storage)).get(&handle).cloned()
}

fn clone_handle<T>(
    storage: &'static OnceLock<Mutex<HashMap<usize, Arc<T>>>>,
    handle: usize,
) -> usize {
    get(storage, handle).map_or(0, |value| insert(storage, value))
}

fn destroy<T>(storage: &'static OnceLock<Mutex<HashMap<usize, Arc<T>>>>, handle: usize) -> i32 {
    if lock(registry(storage)).remove(&handle).is_some() {
        SYNC_OK
    } else {
        SYNC_INVALID_HANDLE
    }
}

#[expect(
    unsafe_code,
    reason = "the synchronization ABI receives compiler-validated value storage"
)]
unsafe fn copy_from_source(source: *const u8, size: usize) -> Option<AlignedBytes> {
    if size != 0 && source.is_null() {
        return None;
    }
    let mut value = AlignedBytes::zeroed(size);
    if size != 0 {
        // SAFETY: The caller promises `size` readable bytes.
        let source = unsafe { std::slice::from_raw_parts(source, size) };
        // SAFETY: `value` owns at least `size` writable bytes.
        let destination = unsafe { std::slice::from_raw_parts_mut(value.as_mut_ptr(), size) };
        destination.copy_from_slice(source);
    }
    Some(value)
}

#[expect(
    unsafe_code,
    reason = "the synchronization ABI writes into compiler-validated value storage"
)]
unsafe fn copy_to_destination(value: &AlignedBytes, destination: *mut u8, size: usize) -> bool {
    if value.length != size || (size != 0 && destination.is_null()) {
        return false;
    }
    if size != 0 {
        // SAFETY: The caller promises `size` writable bytes.
        let destination = unsafe { std::slice::from_raw_parts_mut(destination, size) };
        // SAFETY: `value` owns `size` readable bytes.
        let source = unsafe { std::slice::from_raw_parts(value.as_ptr(), size) };
        destination.copy_from_slice(source);
    }
    true
}

/// Creates a mutex by moving one compiler-owned value into runtime storage.
///
/// # Safety
///
/// `source` must identify `size` readable bytes when `size` is nonzero.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable ABI accepts a validated value pointer"
)]
pub unsafe extern "C" fn mutex_create(source: *const u8, size: usize) -> usize {
    // SAFETY: Forwarded from this function's ABI contract.
    let Some(value) = (unsafe { copy_from_source(source, size) }) else {
        return 0;
    };
    insert(&MUTEXES, Arc::new(Mutex::new(value)))
}

/// Clones the shared ownership represented by a mutex handle.
#[unsafe(no_mangle)]
pub extern "C" fn mutex_clone(handle: usize) -> usize {
    clone_handle(&MUTEXES, handle)
}

/// Copies a mutex-protected value into compiler-owned storage.
///
/// # Safety
///
/// `destination` must identify `size` writable bytes when `size` is nonzero.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable ABI writes through a validated result pointer"
)]
pub unsafe extern "C" fn mutex_load(handle: usize, destination: *mut u8, size: usize) -> i32 {
    let Some(mutex) = get(&MUTEXES, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    let value = lock(&mutex);
    // SAFETY: Forwarded from this function's ABI contract.
    if unsafe { copy_to_destination(&value, destination, size) } {
        SYNC_OK
    } else {
        SYNC_INVALID_HANDLE
    }
}

/// Atomically replaces a mutex-protected value and returns the previous value.
///
/// # Safety
///
/// `source` and `destination` must identify `size` readable and writable bytes.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "the stable ABI transfers validated value bytes")]
pub unsafe extern "C" fn mutex_replace(
    handle: usize,
    source: *const u8,
    destination: *mut u8,
    size: usize,
) -> i32 {
    let Some(mutex) = get(&MUTEXES, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    // SAFETY: Forwarded from this function's ABI contract.
    let Some(replacement) = (unsafe { copy_from_source(source, size) }) else {
        return SYNC_INVALID_HANDLE;
    };
    let mut value = lock(&mutex);
    // SAFETY: Forwarded from this function's ABI contract.
    if !unsafe { copy_to_destination(&value, destination, size) } {
        return SYNC_INVALID_HANDLE;
    }
    *value = replacement;
    SYNC_OK
}

/// Retires one mutex handle.
#[unsafe(no_mangle)]
pub extern "C" fn mutex_destroy(handle: usize) -> i32 {
    destroy(&MUTEXES, handle)
}

/// Creates a reader-writer lock by moving one value into runtime storage.
///
/// # Safety
///
/// `source` must identify `size` readable bytes when `size` is nonzero.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable ABI accepts a validated value pointer"
)]
pub unsafe extern "C" fn rwlock_create(source: *const u8, size: usize) -> usize {
    // SAFETY: Forwarded from this function's ABI contract.
    let Some(value) = (unsafe { copy_from_source(source, size) }) else {
        return 0;
    };
    insert(&RWLOCKS, Arc::new(RwLock::new(value)))
}

/// Clones the shared ownership represented by a reader-writer lock handle.
#[unsafe(no_mangle)]
pub extern "C" fn rwlock_clone(handle: usize) -> usize {
    clone_handle(&RWLOCKS, handle)
}

/// Copies a reader-writer-lock-protected value into compiler-owned storage.
///
/// # Safety
///
/// `destination` must identify `size` writable bytes when `size` is nonzero.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable ABI writes through a validated result pointer"
)]
pub unsafe extern "C" fn rwlock_load(handle: usize, destination: *mut u8, size: usize) -> i32 {
    let Some(rwlock) = get(&RWLOCKS, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    let value = read(&rwlock);
    // SAFETY: Forwarded from this function's ABI contract.
    if unsafe { copy_to_destination(&value, destination, size) } {
        SYNC_OK
    } else {
        SYNC_INVALID_HANDLE
    }
}

/// Atomically replaces a writer-locked value and returns the previous value.
///
/// # Safety
///
/// `source` and `destination` must identify `size` readable and writable bytes.
#[unsafe(no_mangle)]
#[expect(unsafe_code, reason = "the stable ABI transfers validated value bytes")]
pub unsafe extern "C" fn rwlock_replace(
    handle: usize,
    source: *const u8,
    destination: *mut u8,
    size: usize,
) -> i32 {
    let Some(rwlock) = get(&RWLOCKS, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    // SAFETY: Forwarded from this function's ABI contract.
    let Some(replacement) = (unsafe { copy_from_source(source, size) }) else {
        return SYNC_INVALID_HANDLE;
    };
    let mut value = write(&rwlock);
    // SAFETY: Forwarded from this function's ABI contract.
    if !unsafe { copy_to_destination(&value, destination, size) } {
        return SYNC_INVALID_HANDLE;
    }
    *value = replacement;
    SYNC_OK
}

/// Retires one reader-writer lock handle.
#[unsafe(no_mangle)]
pub extern "C" fn rwlock_destroy(handle: usize) -> i32 {
    destroy(&RWLOCKS, handle)
}

/// Creates a bounded multi-producer, multi-consumer channel.
#[unsafe(no_mangle)]
pub extern "C" fn channel_create(capacity: usize, element_size: usize) -> usize {
    insert(
        &CHANNELS,
        Arc::new(ChannelState {
            capacity: capacity.max(1),
            element_size,
            inner: Mutex::new(ChannelInner {
                queue: VecDeque::new(),
                closed: false,
            }),
            available: Condvar::new(),
            space: Condvar::new(),
        }),
    )
}

/// Clones one channel endpoint.
#[unsafe(no_mangle)]
pub extern "C" fn channel_clone(handle: usize) -> usize {
    clone_handle(&CHANNELS, handle)
}

/// Sends one moved value, blocking while the bounded channel is full.
///
/// # Safety
///
/// `source` must identify `size` readable bytes when `size` is nonzero.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable ABI accepts a validated value pointer"
)]
pub unsafe extern "C" fn channel_send(handle: usize, source: *const u8, size: usize) -> i32 {
    let Some(channel) = get(&CHANNELS, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    if size != channel.element_size {
        return SYNC_INVALID_HANDLE;
    }
    // SAFETY: Forwarded from this function's ABI contract.
    let Some(value) = (unsafe { copy_from_source(source, size) }) else {
        return SYNC_INVALID_HANDLE;
    };
    let mut inner = lock(&channel.inner);
    while inner.queue.len() >= channel.capacity && !inner.closed {
        inner = wait(&channel.space, inner);
    }
    if inner.closed {
        return SYNC_CLOSED;
    }
    inner.queue.push_back(value);
    channel.available.notify_one();
    SYNC_OK
}

/// Receives one value, blocking while an open channel is empty.
///
/// # Safety
///
/// `destination` must identify `size` writable bytes when `size` is nonzero.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable ABI writes through a validated result pointer"
)]
pub unsafe extern "C" fn channel_receive(handle: usize, destination: *mut u8, size: usize) -> i32 {
    let Some(channel) = get(&CHANNELS, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    if size != channel.element_size {
        return SYNC_INVALID_HANDLE;
    }
    let mut inner = lock(&channel.inner);
    while inner.queue.is_empty() && !inner.closed {
        inner = wait(&channel.available, inner);
    }
    let Some(value) = inner.queue.pop_front() else {
        return SYNC_CLOSED;
    };
    channel.space.notify_one();
    drop(inner);
    // SAFETY: Forwarded from this function's ABI contract.
    if unsafe { copy_to_destination(&value, destination, size) } {
        SYNC_OK
    } else {
        SYNC_INVALID_HANDLE
    }
}

/// Closes a channel and wakes all blocked senders and receivers.
#[unsafe(no_mangle)]
pub extern "C" fn channel_close(handle: usize) -> i32 {
    let Some(channel) = get(&CHANNELS, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    let mut inner = lock(&channel.inner);
    inner.closed = true;
    channel.available.notify_all();
    channel.space.notify_all();
    SYNC_OK
}

/// Retires one channel handle and closes the channel after its last endpoint.
#[unsafe(no_mangle)]
pub extern "C" fn channel_destroy(handle: usize) -> i32 {
    let Some(channel) = lock(registry(&CHANNELS)).remove(&handle) else {
        return SYNC_INVALID_HANDLE;
    };
    if Arc::strong_count(&channel) == 1 {
        let mut inner = lock(&channel.inner);
        inner.closed = true;
        channel.available.notify_all();
        channel.space.notify_all();
    }
    SYNC_OK
}

/// Creates a reusable barrier for `participants` waiters.
#[unsafe(no_mangle)]
pub extern "C" fn barrier_create(participants: usize) -> usize {
    if participants == 0 {
        0
    } else {
        insert(&BARRIERS, Arc::new(Barrier::new(participants)))
    }
}

/// Clones one barrier handle.
#[unsafe(no_mangle)]
pub extern "C" fn barrier_clone(handle: usize) -> usize {
    clone_handle(&BARRIERS, handle)
}

/// Waits at a barrier and returns `1` to the elected leader, `0` otherwise.
///
/// Returns `-1` for an invalid handle.
#[unsafe(no_mangle)]
pub extern "C" fn barrier_wait(handle: usize) -> i32 {
    get(&BARRIERS, handle).map_or(-1, |barrier| i32::from(barrier.wait().is_leader()))
}

/// Retires one barrier handle.
#[unsafe(no_mangle)]
pub extern "C" fn barrier_destroy(handle: usize) -> i32 {
    destroy(&BARRIERS, handle)
}

/// Creates a counting semaphore with an initial permit count.
#[unsafe(no_mangle)]
pub extern "C" fn semaphore_create(permits: usize) -> usize {
    insert(
        &SEMAPHORES,
        Arc::new(SemaphoreState {
            permits: Mutex::new(permits),
            available: Condvar::new(),
        }),
    )
}

/// Clones one semaphore handle.
#[unsafe(no_mangle)]
pub extern "C" fn semaphore_clone(handle: usize) -> usize {
    clone_handle(&SEMAPHORES, handle)
}

/// Acquires one permit, blocking until it is available.
#[unsafe(no_mangle)]
pub extern "C" fn semaphore_acquire(handle: usize) -> i32 {
    let Some(semaphore) = get(&SEMAPHORES, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    let mut permits = lock(&semaphore.permits);
    while *permits == 0 {
        permits = wait(&semaphore.available, permits);
    }
    *permits -= 1;
    SYNC_OK
}

/// Attempts to acquire one permit without blocking.
///
/// Returns `0` when acquired, `1` for an invalid handle, and `2` when empty.
#[unsafe(no_mangle)]
pub extern "C" fn semaphore_try_acquire(handle: usize) -> i32 {
    let Some(semaphore) = get(&SEMAPHORES, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    let mut permits = lock(&semaphore.permits);
    if *permits == 0 {
        SYNC_CLOSED
    } else {
        *permits -= 1;
        SYNC_OK
    }
}

/// Releases `count` semaphore permits.
#[unsafe(no_mangle)]
pub extern "C" fn semaphore_release(handle: usize, count: usize) -> i32 {
    let Some(semaphore) = get(&SEMAPHORES, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    let mut permits = lock(&semaphore.permits);
    let Some(updated) = permits.checked_add(count) else {
        return SYNC_INVALID_HANDLE;
    };
    *permits = updated;
    semaphore.available.notify_all();
    SYNC_OK
}

/// Retires one semaphore handle.
#[unsafe(no_mangle)]
pub extern "C" fn semaphore_destroy(handle: usize) -> i32 {
    destroy(&SEMAPHORES, handle)
}

/// Creates a sequentially consistent 64-bit atomic cell.
#[unsafe(no_mangle)]
pub extern "C" fn atomic_create(value: u64) -> usize {
    insert(&ATOMICS, Arc::new(AtomicU64::new(value)))
}

/// Clones one atomic handle.
#[unsafe(no_mangle)]
pub extern "C" fn atomic_clone(handle: usize) -> usize {
    clone_handle(&ATOMICS, handle)
}

/// Loads an atomic value, returning zero for an invalid handle.
#[unsafe(no_mangle)]
pub extern "C" fn atomic_load(handle: usize) -> u64 {
    get(&ATOMICS, handle).map_or(0, |atomic| atomic.load(Ordering::SeqCst))
}

/// Stores an atomic value and reports whether the handle was valid.
#[unsafe(no_mangle)]
pub extern "C" fn atomic_store(handle: usize, value: u64) -> i32 {
    let Some(atomic) = get(&ATOMICS, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    atomic.store(value, Ordering::SeqCst);
    SYNC_OK
}

/// Swaps an atomic value, returning zero for an invalid handle.
#[unsafe(no_mangle)]
pub extern "C" fn atomic_swap(handle: usize, value: u64) -> u64 {
    get(&ATOMICS, handle).map_or(0, |atomic| atomic.swap(value, Ordering::SeqCst))
}

/// Adds to an atomic value, returning zero for an invalid handle.
#[unsafe(no_mangle)]
pub extern "C" fn atomic_fetch_add(handle: usize, value: u64) -> u64 {
    get(&ATOMICS, handle).map_or(0, |atomic| atomic.fetch_add(value, Ordering::SeqCst))
}

/// Performs compare-exchange and returns whether the exchange succeeded.
#[unsafe(no_mangle)]
pub extern "C" fn atomic_compare_exchange(handle: usize, current: u64, replacement: u64) -> bool {
    get(&ATOMICS, handle).is_some_and(|atomic| {
        atomic
            .compare_exchange(current, replacement, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    })
}

/// Retires one atomic handle.
#[unsafe(no_mangle)]
pub extern "C" fn atomic_destroy(handle: usize) -> i32 {
    destroy(&ATOMICS, handle)
}

/// Creates copy-only thread-local storage from an initial value.
///
/// # Safety
///
/// `source` must identify `size` readable bytes when `size` is nonzero.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable ABI accepts a validated value pointer"
)]
pub unsafe extern "C" fn thread_local_create(source: *const u8, size: usize) -> usize {
    // SAFETY: Forwarded from this function's ABI contract.
    let Some(initial) = (unsafe { copy_from_source(source, size) }) else {
        return 0;
    };
    insert(
        &THREAD_LOCALS,
        Arc::new(ThreadLocalState {
            initial,
            values: Mutex::new(HashMap::new()),
        }),
    )
}

/// Clones one thread-local storage handle.
#[unsafe(no_mangle)]
pub extern "C" fn thread_local_clone(handle: usize) -> usize {
    clone_handle(&THREAD_LOCALS, handle)
}

/// Copies the current thread's local value into compiler-owned storage.
///
/// # Safety
///
/// `destination` must identify `size` writable bytes when `size` is nonzero.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable ABI writes through a validated result pointer"
)]
pub unsafe extern "C" fn thread_local_get(handle: usize, destination: *mut u8, size: usize) -> i32 {
    let Some(local) = get(&THREAD_LOCALS, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    let thread = std::thread::current().id();
    let mut values = lock(&local.values);
    let value = values
        .entry(thread)
        .or_insert_with(|| local.initial.clone());
    // SAFETY: Forwarded from this function's ABI contract.
    if unsafe { copy_to_destination(value, destination, size) } {
        SYNC_OK
    } else {
        SYNC_INVALID_HANDLE
    }
}

/// Replaces the current thread's local value.
///
/// # Safety
///
/// `source` must identify `size` readable bytes when `size` is nonzero.
#[unsafe(no_mangle)]
#[expect(
    unsafe_code,
    reason = "the stable ABI accepts a validated value pointer"
)]
pub unsafe extern "C" fn thread_local_set(handle: usize, source: *const u8, size: usize) -> i32 {
    let Some(local) = get(&THREAD_LOCALS, handle) else {
        return SYNC_INVALID_HANDLE;
    };
    // SAFETY: Forwarded from this function's ABI contract.
    let Some(value) = (unsafe { copy_from_source(source, size) }) else {
        return SYNC_INVALID_HANDLE;
    };
    lock(&local.values).insert(std::thread::current().id(), value);
    SYNC_OK
}

/// Retires one thread-local storage handle.
#[unsafe(no_mangle)]
pub extern "C" fn thread_local_destroy(handle: usize) -> i32 {
    destroy(&THREAD_LOCALS, handle)
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::{
        SYNC_CLOSED, SYNC_OK, channel_clone, channel_close, channel_create, channel_destroy,
        channel_receive, channel_send, mutex_clone, mutex_create, mutex_destroy, mutex_load,
        mutex_replace,
    };

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies exact compiler-style input and result buffers"
    )]
    fn cloned_mutex_handles_should_share_replaced_values() {
        let initial = 20_i32;
        // SAFETY: `initial` is a live four-byte value.
        let first = unsafe { mutex_create((&raw const initial).cast(), size_of::<i32>()) };
        let second = mutex_clone(first);
        let replacement = 42_i32;
        let mut previous = 0_i32;
        // SAFETY: All pointers identify live four-byte values.
        let replace_status = unsafe {
            mutex_replace(
                second,
                (&raw const replacement).cast(),
                (&raw mut previous).cast(),
                size_of::<i32>(),
            )
        };
        let mut observed = 0_i32;
        // SAFETY: `observed` is a live four-byte result.
        let load_status =
            unsafe { mutex_load(first, (&raw mut observed).cast(), size_of::<i32>()) };
        let _ = mutex_destroy(first);
        let _ = mutex_destroy(second);

        assert_eq!(
            (replace_status, load_status, previous, observed),
            (SYNC_OK, SYNC_OK, 20, 42)
        );
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "the test supplies exact compiler-style channel buffers"
    )]
    fn bounded_channel_should_transfer_and_report_close() {
        let sender = channel_create(1, size_of::<i32>());
        let receiver = channel_clone(sender);
        let sent = 42_i32;
        // SAFETY: `sent` is a live four-byte value.
        let send_status =
            unsafe { channel_send(sender, (&raw const sent).cast(), size_of::<i32>()) };
        let mut observed = 0_i32;
        // SAFETY: `observed` is a live four-byte result.
        let receive_status =
            unsafe { channel_receive(receiver, (&raw mut observed).cast(), size_of::<i32>()) };
        let _ = channel_close(sender);
        // SAFETY: `observed` remains a live four-byte result.
        let closed_status =
            unsafe { channel_receive(receiver, (&raw mut observed).cast(), size_of::<i32>()) };
        let _ = channel_destroy(sender);
        let _ = channel_destroy(receiver);

        assert_eq!(
            (send_status, receive_status, observed, closed_status),
            (SYNC_OK, SYNC_OK, 42, SYNC_CLOSED)
        );
    }
}
