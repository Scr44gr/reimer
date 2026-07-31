# Allocation and collections

Owned memory always has an allocator and an explicit cleanup path.

## Choosing an allocator

`general_allocator()`
: General-purpose allocation for owners with independent lifetimes.

`page_allocator()`
: Page-backed storage for coarse allocations.

`ArenaAllocator`
: Groups many allocations under one arena lifetime. Individual owners still follow their documented cleanup contract; releasing the arena finishes the backing allocation.

`FixedBufferAllocator`
: Allocates from caller-provided storage and returns `AllocError` when capacity is exhausted.

## Raw owned bytes

```reimer
from std::alloc import AllocError, OwnedBytes, allocate_bytes, page_allocator;

fn reserve() -> Result<usize, AllocError> {
    let allocator = page_allocator();
    let bytes: OwnedBytes = allocate_bytes(&allocator, 4_096)?;
    defer bytes.deinit();
    Ok(bytes.len())
}
```

`OwnedBytes` exposes length and raw pointer access for low-level integrations. Keep pointer use inside a narrow, reviewed boundary.

## `Vec<T>`

```reimer
from std::alloc import AllocError, general_allocator;
from std::collections import Vec;

fn total() -> Result<i32, AllocError> {
    let allocator = general_allocator();
    let created: Result<Vec<i32>, AllocError> =
        Vec::with_capacity(&allocator, 4);
    let mut values = created?;
    defer values.deinit();

    values.push(20)?;
    values.push(22)?;

    let left = match values.get(0) { Some(value) => value, None => 0 };
    let right = match values.get(1) { Some(value) => value, None => 0 };
    Ok(left + right)
}
```

Useful operations include `len`, `capacity`, `is_empty`, `as_slice`, `as_mut_slice`, `push`, `push_within_capacity`, `get`, `set`, `pop`, and `deinit`.

## Hash collections

`HashMap<K, V>` supports lookup, containment, insertion, replacement, removal, and explicit cleanup. `HashSet<T>` provides the corresponding key-only operations. Keys must satisfy the required equality and hash contracts.

```reimer
let created: Result<HashMap<u32, i32>, AllocError> =
    HashMap::new(&allocator);
let mut scores = created?;
defer scores.deinit();

scores.insert(7, 42)?;
let score = match scores.get(7) {
    Some(value) => value,
    None => 0,
};
```

## Ring buffers

`RingBuffer<T>` is useful for bounded queues, recent-history windows, and producer/consumer staging where overwriting the oldest element is acceptable. `push` returns `false` only when the configured capacity is zero; a full nonzero buffer overwrites its oldest value.

## Static allocator estimates

The linter and LSP recognize constant capacities and bounded reads. An editor hint such as `static allocator reservation: 4096 bytes per call` is a compile-time estimate, not a memory profile or peak-memory claim. Dynamic sizes remain labeled dynamic.
