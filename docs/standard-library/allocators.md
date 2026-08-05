# Allocators

Reimer makes allocation policy part of the API. An allocator-backed function
receives `&Allocator` and returns an owner such as `String`, `Vec<T>`, or
`OwnedBytes`. Low-level allocation APIs report `AllocError`; higher-level APIs
may translate allocation failure into their domain error, such as
`FileError::OutOfMemory`. This keeps memory strategy, ownership, and failure
visible at the call site.

Most programs should begin with `general_allocator()`. Choose another
allocator only when its lifetime or layout policy solves a specific problem.

## Choose by lifetime

| Requirement | Allocator | Use it for | Avoid it when |
|---|---|---|---|
| Independent owners with unrelated lifetimes | `general_allocator()` | application state, strings, collections, assets, returned owners | a tighter lifetime or fixed budget is required |
| Page-aligned, page-rounded regions | `page_allocator()` | large transfer buffers and native APIs requiring page alignment | small strings or ordinary collections |
| Many owners discarded as one phase | `ArenaAllocator` | parsing, level loading, frame preparation, temporary graphs | any child must outlive the arena or be reclaimed individually |
| A strict caller-provided byte budget | `FixedBufferAllocator` | bounded scratch work, real-time paths, tests for allocation budgets | unknown or unbounded growth |
| No owned memory is needed | no allocator | borrowed `str`, slices, iterators, and fixed arrays | the result must outlive the borrowed source |

The allocator handle is lightweight and `Copy`, but dynamic handles are not
immortal. An arena or fixed-buffer handle becomes invalid when its allocator
owner is released.

## The default: `general_allocator`

Use the general allocator unless you can name a stronger requirement. Every
allocation has an independent lifetime, so an owner may be returned, moved
into application state, or destroyed without coordinating with neighboring
owners.

```reimer
from std::alloc import AllocError, Allocator, general_allocator;
from std::string import String;

fn greeting(allocator: &Allocator) -> Result<String, AllocError> {
    String::from(allocator, "hello")
}

fn main() -> i32 {
    let allocator = general_allocator();
    let message = greeting(&allocator)
        .expect("the greeting allocation must succeed");
    defer message.deinit();
    message.len() as i32
}
```

Passing the allocator into `greeting` keeps the function reusable. A library
should not silently choose the general allocator when it returns an owned
value whose memory policy belongs to its caller.

## Page allocations

`page_allocator()` rounds each physical reservation to a multiple of 4 KiB and
returns memory aligned to at least 4 KiB. The logical `OwnedBytes.len()` remains
the requested length.

```reimer
from std::alloc import AllocError, allocate_bytes, page_allocator;

fn reserve_transfer_region() -> Result<usize, AllocError> {
    let allocator = page_allocator();
    let bytes = allocate_bytes(&allocator, 1_000_000)?;
    defer bytes.deinit();
    Ok(bytes.len())
}
```

This policy is useful when a native API or a large coarse-grained buffer
benefits from page alignment. It is wasteful for many small allocations: a
one-byte request still consumes a page-sized physical reservation.

## Arena allocation

An arena gives many child owners one shared lifetime. Its parent must currently
be either the general allocator or the page allocator. Nested dynamic
allocators are deliberately rejected.

```reimer
from std::alloc import AllocError, ArenaAllocator, general_allocator;
from std::collections import Vec;

fn process_batch() -> Result<usize, AllocError> {
    let parent = general_allocator();
    let arena = ArenaAllocator::init(&parent)?;
    defer arena.deinit();

    let mut values: Vec<u32> =
        Vec::with_capacity(arena.allocator(), 128)?;
    defer values.deinit();

    values.push(20)?;
    values.push(22)?;
    Ok(values.len())
}
```

`defer` runs in reverse registration order, so child cleanup runs before arena
cleanup in this example. Releasing the arena then frees every backing
allocation together.

Arena child cleanup fulfills the owner's cleanup contract, but it does not
make that allocation reusable during the arena lifetime. Use an arena when the
group is discarded together, not as a replacement for independently managed
long-lived storage.

An owner allocated from the arena must never escape the arena's lifetime. This
also applies to an `Allocator` copied with `allocator_value()`: the copy is a
handle to the same arena, not a new allocator.

## Fixed-buffer allocation

`FixedBufferAllocator` is a bump allocator over memory owned by the caller. It
never allocates backing storage and returns `AllocError::OutOfMemory` when the
remaining bytes, including alignment padding, cannot satisfy a request.

```reimer
from std::alloc import AllocError, FixedBufferAllocator;
from std::collections import Vec;

fn bounded_work() -> Result<usize, AllocError> {
    let mut storage: [u8; 4_096] = [0; 4_096];
    let fixed = FixedBufferAllocator::init(
        &mut storage as *mut u8,
        4_096,
    )?;
    defer fixed.deinit();

    let mut values: Vec<u32> =
        Vec::with_capacity(fixed.allocator(), 128)?;
    defer values.deinit();

    if !values.push_within_capacity(42) {
        return Err(AllocError::OutOfMemory);
    }
    Ok(values.allocated_bytes())
}
```

Prefer pre-sized collections and non-growing operations such as
`push_within_capacity` on a fixed budget. A growing operation may reserve a
second region while the original region still occupies the bump buffer.
Individual child cleanup does not rewind the bump offset. To reuse the whole
buffer, finish every child, retire the allocator, and create a new
`FixedBufferAllocator` over the storage.

The fixed allocator does not own or free `storage`. The backing array must stay
alive and unmoved until every child and the fixed allocator have been cleaned
up. Keep large buffers off small thread stacks; use an appropriately owned
backing region when the budget is too large for a local array.

## Cleanup and ownership transfer

Reimer does not run an implicit destructor for allocator-backed owners. After
construction succeeds, register cleanup immediately:

```reimer
let mut values = Vec::with_capacity(&allocator, 64)?;
defer values.deinit();
```

Use these rules:

1. Call `deinit()` once when the value is consumed at cleanup.
2. Use `release()` when the owner exposes it and an aggregate needs idempotent
   in-place cleanup.
3. If a function returns or moves the owner, ownership transfer replaces local
   cleanup.
4. Clean dynamic-allocator children before the arena or fixed allocator.
5. Keep the allocator owner and caller-provided backing storage alive for as
   long as any child can use their handle.

Register each `defer` only after its construction succeeds. This naturally
cleans partially initialized functions in reverse order when a later `?`
propagates an error.

Use `?` for ordinary allocation failure. Use `expect(message)` only when
failure means a genuine program invariant has been violated, such as a small
mandatory startup allocation on a supported machine.

## Raw owned bytes and alignment

Most code should prefer typed owners. Use `OwnedBytes` for binary buffers,
native interoperability, or implementing allocator-aware containers.

```reimer
from std::alloc import AllocError, allocate_aligned_bytes, general_allocator;

fn aligned_region() -> Result<usize, AllocError> {
    let allocator = general_allocator();
    let bytes = allocate_aligned_bytes(&allocator, 1_024, 64)?;
    defer bytes.deinit();
    Ok(bytes.alignment())
}
```

The alignment must be a nonzero power of two. Invalid values return
`AllocError::InvalidAlignment`; an unsatisfied or overflowing reservation
returns `AllocError::OutOfMemory`.

`OwnedBytes` provides safe bounded access through `as_bytes()` and
`as_mut_bytes()`. Keep `as_ptr()` and `as_mut_ptr()` inside narrow native
boundaries. A raw pointer must not outlive its owner. Assume `grow`,
`grow_aligned`, and `replace` can invalidate existing pointers; `release` and
`deinit` always end their validity.

`grow` and `grow_aligned` preserve the existing region on allocation failure.
On success they may replace the backing address, so previously borrowed slices
and pointers are no longer valid.

## API at a glance

| API | Purpose |
|---|---|
| `AllocError::OutOfMemory` | A request could not be satisfied, its size overflowed, an arena parent was unsupported, or fixed-buffer initialization received no usable storage. |
| `AllocError::InvalidAlignment` | An alignment was zero or not a power of two. |
| `Allocator` | A lightweight allocation-strategy handle. |
| `Allocator::opaque_handle()` | Exposes the handle only for a native ABI that explicitly accepts a Reimer allocator. Never fabricate or persist one. |
| `general_allocator()` | Returns the stable general-purpose allocator. |
| `page_allocator()` | Returns the stable page-rounded allocator. |
| `allocate_bytes()` | Creates byte-aligned `OwnedBytes`. |
| `allocate_aligned_bytes()` | Creates `OwnedBytes` with an explicit power-of-two alignment. |
| `deinit_bytes()` and `bytes_len()` | Free-function equivalents of `OwnedBytes::deinit()` and `OwnedBytes::len()`. |
| `OwnedBytes::empty()` | Creates a zero-length owner tied to an allocator without reserving storage. |
| `len()` and `alignment()` | Report the logical byte length and retained alignment. |
| `as_bytes()` and `as_mut_bytes()` | Borrow the bounded byte region safely. |
| `as_ptr()` and `as_mut_ptr()` | Borrow raw pointers for narrow native boundaries. |
| `grow()` and `grow_aligned()` | Replace storage when needed while preserving existing bytes and failure atomicity. |
| `replace()` | Release the old region and take ownership of a supplied replacement. |
| `deinit()` and `release()` | `deinit()` consumes the owner once; `release()` frees it idempotently in place. |
| `ArenaAllocator::init()` | Creates a grouped-lifetime allocator from a stable parent. |
| `FixedBufferAllocator::init()` | Creates a bounded bump allocator over caller-owned bytes. |
| `allocator()` | Borrows the child allocator handle for immediate construction. |
| `allocator_value()` | Copies the same lifetime-bound handle for storage in an aggregate. |
| allocator `deinit()` and `release()` | `deinit()` consumes the allocator once; `release()` retires it idempotently in place. |

## Designing allocator-aware APIs

Accept `&Allocator` when a function creates memory that the caller will own:

```reimer
from std::alloc import Allocator;
from std::fs import FileError, read_to_string;
from std::string import String;

fn read_document(
    allocator: &Allocator,
    path: str,
) -> Result<String, FileError> {
    read_to_string(path, allocator)
}
```

Return a borrowed view instead when no new ownership is required. Do not add an
allocator parameter to a function that only returns a slice, iterator, scalar,
or fixed-size value.

Owners such as `String`, `Vec<T>`, `HashMap<K, V>`, file buffers, tensors, and
environment strings retain the allocator needed for their later growth or
cleanup. With an arena or fixed buffer, that retained handle means the parent
allocator lifetime is part of the owner's contract even though the owner does
not store a source-level borrow.

## Capacity and performance

- Use `with_capacity` or `reserve` when a useful upper bound is known.
- Reuse long-lived owners with `clear` when their capacity is still useful.
- Avoid allocating inside fixed-step, audio, or render loops when storage can
  be prepared during loading.
- Prefer borrowed `str` and slices when ownership is unnecessary.
- Measure release builds before replacing the general allocator with a more
  restrictive strategy.
- Read capacity metrics according to the owner. `OwnedBytes::len()` is its
  logical reservation. `String` reports bytes. `Vec<T>` reports elements and
  additionally exposes `allocated_bytes()` for element storage. Hash
  collection capacities report entries. No one metric includes allocator
  metadata, alignment padding, or page rounding.

See [Collections](allocation-and-collections.md) for collection growth rules
and [Language tooling](../tools/editor-tooling.md#type-information-and-allocators)
for static allocator estimates. Editor estimates are capacity-planning hints,
not measured peak-memory profiles.

## Common mistakes

- Returning an arena-backed owner after the arena has been destroyed.
- Releasing an arena or fixed buffer before its child owners.
- Using `page_allocator()` for many small values.
- Assuming a fixed-buffer child cleanup rewinds the bump allocator.
- Allowing a fixed-budget collection to grow without reserving its maximum.
- Keeping a raw pointer across owner growth or replacement.
- Adding `defer owner.deinit()` after an operation that already consumes and
  transfers that owner.
- Treating a static LSP estimate as the process's peak resident memory.

When in doubt, use `general_allocator()`, propagate the API's allocation
failure, register cleanup immediately, and optimize the allocator only after
the lifetime and budget are explicit.
