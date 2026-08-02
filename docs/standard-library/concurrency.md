# Concurrency and jobs

M9 provides native concurrency without expanding the public `unsafe` surface.
Programs use `std::thread` and `std::job`; pointers, ABI buffers, and function
thunks remain encapsulated between the backend and runtime.

## Threads and type safety

`Thread::spawn` transfers an argument to a worker and returns a `Thread<T>` that
delivers its result exactly once through `join`. `scope` creates a structured
worker and waits for completion before returning, so it may receive an
aggregate containing local borrows.

```reimer
from std::thread import Thread, ThreadError, scope;

fn increment(value: i32) -> i32 { value + 1 }

fn increment_borrowed(value: &mut i32) -> i32 {
    *value += 1;
    *value
}
```

`Send` and `Sync` are built-in structural capabilities:

- a value is `Send` when every field can be transferred;
- a value is `Sync` when every field permits shared access;
- raw pointers never receive either capability;
- a scoped reference cannot escape into a native thread;
- channels and jobs move non-`Copy` values.

Creation errors, retired handles, worker panics, and incompatible layouts are
represented by `ThreadError` or `JobError`.

## Synchronization

`std::thread` exposes:

- `Mutex<T>` and `RwLock<T>`;
- bounded `Channel<T>`;
- `Barrier` and `Semaphore`;
- `ThreadLocal<T>` for `Copy` values;
- `AtomicBool`, `AtomicI64`, `AtomicU64`, `AtomicIsize`, and `AtomicUsize`.

Atomics use sequential consistency. Cloning preserves shared resource
ownership; `deinit` retires each handle explicitly. Access after a handle is
retired violates an invariant and ends in `panic`, never an invalid
dereference.

## Jobs and data parallelism

A `JobPool` receives an allocator and a fixed worker count. Workers persist,
each owns a local queue, and each can steal work from the opposite end of
another queue. `submit` moves its argument into the pool, and `Job::wait`
transfers the result to the caller.

```reimer
from std::alloc import general_allocator;
from std::job import JobPool, JobPoolConfig;
from std::thread import available_parallelism;

let allocator = general_allocator();
let workers = available_parallelism();
let pool = JobPool::init(
    &allocator,
    JobPoolConfig::fixed(workers),
)?;
```

`available_parallelism()` reports the operating system's current estimate of
useful worker threads and always returns at least one. Capture it when creating
a long-lived scheduler; the estimate can change when CPU quotas or topology
change.

`parallel_for_mut` splits a slice into nonoverlapping exclusive regions. The
array variant preserves static length, while `tensor::parallel_for_mut`
operates over contiguous tensor storage. The call waits for every chunk before
returning, so the exclusive borrow ends structurally. `minimum_chunk` is a
preferred minimum; the runtime also balances task count against worker count.

The type checker rejects:

- two overlapping mutable borrows of the same array;
- raw pointers sent as job arguments;
- local references escaping to a native thread;
- `parallel_for_mut` callbacks that do not receive `&mut [T]`.

## Runtime isolation

Each JIT execution receives a session used to scope arguments and to group the
cleanup of threads, jobs, and pools, including resources created by a worker.
This is lifecycle bookkeeping, not an authorization boundary or sandbox:
runtime handles and the native process are shared, and Reimer programs can use
FFI and process APIs. Do not run mutually untrusted programs in one compiler
process. Isolate such executions in separate operating-system processes or
containers with dedicated permissions, environment, network policy, resource
limits, and timeouts.

Async/await, fibers, an ECS scheduler, and configurable atomic orderings remain
outside Reimer v0.1.

## Manual tests

```text
cargo run -p reimer-cli -- run examples/m9_threads/main.reim
cargo run -p reimer-cli -- run examples/m9_synchronization/main.reim
cargo run -p reimer-cli -- run examples/m9_atomics/main.reim
cargo run -p reimer-cli -- run examples/m9_jobs/main.reim
cargo run -p reimer-cli -- run examples/m9_tensor_parallel/main.reim
```
