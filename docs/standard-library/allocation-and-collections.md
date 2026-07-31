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

`HashMap<K, V>` is a flat, open-addressed table with 16-byte grouped control metadata. It uses per-map randomized structural hashing, an `H2` fingerprint in each occupied control byte, quadratic group probing, and a maximum load of 7/8. Control matching uses SSE2 on x86-64, NEON on AArch64, and a scalar fallback elsewhere.

Lookup, containment, insertion, replacement, and removal have expected O(1) cost. `HashSet<T>` provides the corresponding key-only operations. Keys currently need `Copy + Eq + Hash`, and values need `Copy`; a borrowed scoped `str` therefore cannot be stored as a key. Equal keys must produce equal structural hashes.

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

Use `with_capacity` when the approximate collection size is known. `reserve(additional)` ensures room for that many extra distinct entries and can also compact deleted slots. `capacity` reports the number of entries that fit before another growth allocation, not the raw number of control slots.

```reimer
let created: Result<HashSet<u64>, AllocError> =
    HashSet::with_capacity(&allocator, 1_000);
let mut seen = created?;
defer seen.deinit();

seen.reserve(500)?;
seen.insert(42)?;
assert(seen.contains(42), "the inserted value should be present");
```

## Ring buffers

`RingBuffer<T>` is useful for bounded queues, recent-history windows, and producer/consumer staging where overwriting the oldest element is acceptable. `push` returns `false` only when the configured capacity is zero; a full nonzero buffer overwrites its oldest value.

## Static allocator estimates

The linter and LSP recognize constant capacities and bounded reads. An editor hint such as `static allocator reservation: 4096 bytes per call` is a compile-time estimate, not a memory profile or peak-memory claim. Dynamic sizes remain labeled dynamic.
