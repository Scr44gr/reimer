# Errors, ownership, and cleanup

Reimer makes failure and resource lifetime visible in function signatures and control flow.

## `Option` and `Result`

Use `Option<T>` when absence is expected:

```reimer
fn find_origin(points: &[i32]) -> Option<&i32> {
    points.get(0)
}
```

Use `Result<T, E>` when callers need a failure reason:

```reimer
enum ParseError {
    Empty,
    Invalid,
}

fn parse_count(text: str) -> Result<u32, ParseError> {
    if text == "" {
        Err(ParseError::Empty)
    } else {
        Err(ParseError::Invalid)
    }
}
```

The `?` operator returns `None` or propagates `Err(error)` from the current function.

## Choose `?`, `match`, or `expect`

Use `?` for ordinary failures that the caller can handle. Use `match` when the current function can recover differently from `Ok` and `Err`.

Use `Result.expect(message)` only when an `Err` means an invariant has been violated, such as a required startup resource being unavailable:

```reimer
let configuration =
    load_configuration().expect("the bundled configuration must be readable");
```

`expect` consumes `Result<T, E>`, moves the `Ok` value out as `T`, and panics with the supplied `str` on `Err`. It does not format the error payload, so `E` does not need to implement `Display` or `Debug` and no hidden allocator is involved.

## Moves are implicit

Assigning or passing a non-`Copy` value transfers ownership without a `move` keyword.

```reimer
struct Resource {
    handle: usize,
}

fn close(resource: Resource) {
    // `resource` is owned here.
}

let resource = Resource { handle: 7 };
close(resource);
// `resource` cannot be used here.
```

Small plain values may implement `Copy`. Derived `Clone` is deliberately restricted to infallible, allocation-free copies. Allocator-backed duplication remains an explicit operation such as `clone_in`.

## Borrow instead of transferring ownership

```reimer
fn read(resource: &Resource) -> usize {
    resource.handle
}

fn update(resource: &mut Resource, handle: usize) {
    resource.handle = handle;
}
```

Many immutable borrows can coexist. A mutable borrow is exclusive. Views such as slices and `TensorViewMut` cannot outlive their owner.

## Allocators are explicit

The standard library provides:

- `general_allocator()` for general-purpose allocations;
- `page_allocator()` for page-backed allocations;
- `ArenaAllocator` for grouped lifetime cleanup;
- `FixedBufferAllocator` for caller-provided storage.

Allocation returns `Result<Owner, AllocError>` rather than aborting silently.

```reimer
from std::alloc import AllocError, general_allocator;
from std::collections import Vec;

fn collect() -> Result<i32, AllocError> {
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

If an API consumes and returns an owner, that transfer can replace cleanup. Otherwise call `.deinit()`, preferably through `defer`.

See [Allocators](../standard-library/allocators.md) for the allocator decision
table, lifetime rules, fixed budgets, page alignment, and raw owned bytes.

## The cleanup linter

Diagnostic `L2010` reports visible owned-byte allocations, named input
buffers, owned strings and string-producing helpers, arenas, and fixed-buffer
allocators with no cleanup or ownership transfer. It is intentionally
conservative: it points at recognized local leaks without claiming
whole-program lifetime proof or complete coverage of every resource type.

For a file buffer converted into an owned string, `into_string()` is the transfer:

```reimer
let buffer = file.read_to_end(allocator)?;
buffer.into_string()
```

Adding `defer buffer.deinit()` would be wrong here because the successful conversion consumes the buffer.

## Panic versus recoverable errors

Use `Result` for filesystem, I/O, process, allocation, and validation failures that a caller can handle. Use `expect`, `panic`, or `assert` for violated program invariants. Checked arithmetic, division, shifts, and indexing never rely on host-language undefined behavior.
