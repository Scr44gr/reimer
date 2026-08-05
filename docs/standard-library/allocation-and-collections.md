# Collections

Reimer collections use an explicit allocator and expose their growth and
cleanup paths. Read [Allocators](allocators.md) first when choosing between the
general, page, arena, and fixed-buffer strategies.

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
