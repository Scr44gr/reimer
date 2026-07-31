# Text, formatting, and Unicode

Reimer separates borrowed UTF-8 views from allocator-owned text:

- `str` is an immutable `(pointer, byte length)` view. It never allocates.
- `String` owns an allocation and remains move-only.
- every operation that creates a `String` receives an `&Allocator` and returns
  `Result<String, AllocError>`.
- operations that append to an existing `String` reuse its capacity and return
  `Result<(), AllocError>` because growth may fail.

No public text API requires `unsafe`.

## Concatenation

Concatenation is explicit about both ownership and allocation:

```reimer
from std::alloc import general_allocator;
from std::string import String, concat, concat3, repeat;

let allocator = general_allocator();
let greeting = concat(&allocator, "hello", " world")?;
let label = concat3(&allocator, "score", ": ", "42")?;
let divider = repeat(&allocator, "-", 40)?;
```

`concat` and `concat3` calculate the complete byte length and reserve exactly
once. `repeat` checks the total byte count before reserving. `join_strings`
accepts a borrowed slice of owned strings and does not consume its elements.
For incremental construction, `String::push_str`, `String::push_string`, and
`String::push_char` reuse the destination allocation.

Reimer does not overload `+` for strings. Keeping numeric addition separate
avoids an implicit allocation policy and keeps failure visible in the return
type.

## Primitive formatting

An owned `String` is also the allocation-aware formatting builder:

```reimer
let mut message = String::with_capacity(&allocator, 64)?;
message.push_str("score=")?;
message.push_u32(score)?;
message.push_str(", ready=")?;
message.push_bool(ready)?;
```

All signed and unsigned integer widths, including `i128` and `u128`, use exact
base-ten formatting. Floating-point methods use the shortest round-trippable
display representation. Float formatting and 128-bit integer conversion use
bounded runtime buffers; they do not allocate behind the source program.

## UTF-8 queries

`byte_len` reports storage size, while `char_count` counts Unicode scalar
values. `starts_with`, `ends_with`, `contains`, and `find` compare UTF-8 bytes
without copying. `find` returns a byte offset, matching the representation of
`str`; `is_char_boundary` can validate an offset before a future slicing
operation.

`str.bytes()` is zero-copy. `str.chars()` decodes Unicode scalars lazily and
without allocation.

## Unicode scalar operations

The character predicates `is_alphabetic`, `is_alphanumeric`, `is_numeric`,
`is_whitespace`, `is_lowercase`, `is_uppercase`, and `is_control` use Unicode
properties rather than ASCII-only tables.

`to_lowercase` and `to_uppercase` return allocator-owned strings because a
single scalar can map to multiple scalars. For example, uppercasing `ß`
produces `"SS"`. The implementation reserves at most twelve UTF-8 bytes for
one full scalar mapping.

## Runtime boundary

Generated code passes only scalar values and bounded caller-owned buffers to
the native text helpers. The runtime validates null pointers, capacities, and
Unicode scalar values before writing. Its small `unsafe` boundary is internal,
documented, and covered by direct tests; public Reimer code remains safe.
