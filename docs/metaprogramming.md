# Compile-time metaprogramming

M10 adds deterministic frontend evaluation without introducing a runtime
virtual machine or allowing external effects during compilation.

## Constants and `comptime` functions

A global constant must have an explicit type. Its initializer is evaluated
during resolution, and the resulting value is integrated into typed HIR:

```reimer
comptime fn factorial(value: usize) -> usize {
    if value <= 1 {
        1
    } else {
        value * factorial(value - 1)
    }
}

const TABLE_SIZE: usize = factorial(5);

struct Table<T, const N: usize> {
    values: [T; N],
}
```

`comptime` blocks run while checking the program and are suitable for
invariants:

```reimer
comptime {
    assert(TABLE_SIZE == 120);
}
```

Evaluation supports scalars, strings, tuples, arrays, structs, pure calls,
local variables, branches, `match`, loops, and checked casts. A failing `panic`
or `assert` becomes a compiler diagnostic. Both `assert` and `debug_assert`
accept an optional UTF-8 failure message. `debug_assert` is always checked
during compile-time evaluation because this validation does not represent a
runtime profile branch.

## Static initializers

A static uses the same deterministic evaluator as a constant initializer, but
the result is serialized into stable native storage instead of being
substituted at each use:

```reimer
static ORIGIN: (i32, i32) = (0, 0);
static mut FRAME_COUNT: u64 = 0;
```

The initializer may use previously evaluated constants and `comptime fn`
calls. It cannot perform runtime I/O, allocation, FFI, or pointer work.
Immutable static storage can be read and borrowed safely. Every access to a
mutable static requires `unsafe`, and a non-`Copy` value cannot be moved out of
either form.

Each compilation unit enforces these limits:

- 1,000,000 steps;
- 16 MiB of retained values;
- 128 nested calls.

Network access, clocks, randomness, threads, I/O, filesystem access, FFI, raw
pointers, borrows, `unsafe`, `defer`, and runtime calls are unavailable. These
rules are also validated in `comptime` functions that have not been called.

## Attributes

Valid attributes form a closed list and are rejected on incompatible targets:

```reimer
@repr(C)
@align(16)
@derive(Copy, Clone, Debug, Eq, Hash, Default)
@must_use
struct Header {
    kind: u32,
    length: u32,
}

@inline
fn compact(value: Header) -> u64 {
    (value.kind as u64) << 32 | value.length as u64
}

@test
fn header_default_should_be_zeroed() {
    assert(Header::default() == Header { kind: 0, length: 0 });
}
```

- `@repr(C)` preserves the interoperable representation of an FFI struct.
- `@align(N)` raises alignment to a valid power of two.
- `@derive(...)` requests known structural implementations.
- `@inline` is an optimization hint and does not change semantics.
- `@test` requires a function with no parameters, generics, or return value.
- `@must_use` warns through the linter/LSP when a result is discarded.

`Copy`, `Eq`, `Hash`, `Debug`, and `Default` are accepted only when every field
supports the operation. An enum's `Default` uses its first variant. `Clone` is
only derived for `Copy` fields: `value.clone()` never allocates, fails, or
requires an allocator. Owned containers retain `clone_in`.

## Typed reflection

Descriptors only exist during compilation:

```reimer
const HEADER_SIZE: usize = size_of<Header>();

comptime {
    assert(align_of<Header>() == 16);
    assert(meta::name<Header>() == "Header");
    assert(meta::fields<Header>()[0].name == "kind");
    assert(meta::variants<Option<i32>>()[0] == "Some");
    assert(meta::traits<Header>()[0] == "Clone");
}
```

- `size_of<T>()` and `align_of<T>()` query the same layout calculation used by
  the native backend.
- `meta::name<T>()` returns the canonical name.
- `meta::fields<T>()` returns `{ name, type }` descriptors.
- `meta::variants<T>()` returns variant names.
- `meta::traits<T>()` returns satisfied traits in deterministic order.

These functions require explicit generic arguments and cannot be called from
runtime code. Const generics use the same values evaluated and checked by the
frontend.

## Running the example

```text
cargo run -p reimer-cli -- check examples/m10_comptime.reim
cargo run -p reimer-cli -- run examples/m10_comptime.reim
cargo run -p reimer-cli -- test examples/m10_comptime.reim
```
