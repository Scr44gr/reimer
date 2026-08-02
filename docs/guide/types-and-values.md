# Types and values

Reimer uses static types and native layouts. Type inference removes many local annotations, but it never changes the resolved type at runtime.

## Scalar types

| Family | Types | Notes |
|---|---|---|
| Signed integers | `i8`, `i16`, `i32`, `i64`, `i128`, `isize` | `isize` follows pointer width. |
| Unsigned integers | `u8`, `u16`, `u32`, `u64`, `u128`, `usize` | `usize` is used for lengths and indexes. |
| Floating point | `f32`, `f64` | IEEE native scalar operations. |
| Boolean | `bool` | Values are `true` and `false`. |
| Unicode scalar | `char` | One Unicode scalar value, not one UTF-8 byte. |
| Unit | `()` | The only no-value type. There is no separate `void`. |
| Diverging | `never` | Used by functions such as `std::process::exit`. |

Integer literals are contextually checked through `u128`; see [numeric literals](numeric-literals.md). Casts use `as`.

```reimer
let byte: u8 = 0x2A;
let address_width: usize = byte as usize;
let ratio: f64 = 0.5;
let scalar: char = 'λ';
```

Explicit `f32`/`f64` casts to integer types up to 64 bits truncate toward zero
and saturate at the destination range. `NaN` converts to zero. These semantics
are defined by Reimer and do not depend on a platform's native conversion
instruction:

```reimer
let frame: u32 = 41.9 as u32;       // 41
let channel: u8 = 300.0 as u8;      // 255
let unsigned: u16 = -2.0 as u16;    // 0
```

Float-to-`i128` and float-to-`u128` casts are intentionally rejected until the
native backend can preserve the same saturating contract at those widths.

## Tuples and arrays

Tuples combine values with fixed, possibly different types. Arrays contain a compile-time number of values of one type.

```reimer
let status: (i32, bool) = (42, true);
let samples: [f32; 4] = [0.0, 0.25, 0.5, 1.0];
let cleared: [u32; 256] = [0; 256];

let code = status.0;
let sample = samples[2];
```

`[value; N]` evaluates `value` exactly once and copies it into all `N`
elements. `N` is a compile-time non-negative integer, including a const generic,
and the element type must satisfy `Copy`:

```reimer
fn filled<T: Copy, const N: usize>(value: T) -> [T; N] {
    [value; N]
}
```

Use an explicit element list when each element must be constructed separately
or owns resources.

Array and slice indexing is always bounds checked. `get` and `get_mut` return `Option` when an invalid index is expected rather than exceptional.

## Slices and strings

`&[T]` and `&mut [T]` are borrowed views over contiguous elements. A `str` is a non-owning, validated UTF-8 view represented by a pointer and byte length.

```reimer
let label: str = "café";
let bytes: &[u8] = label.bytes();
let characters = label.chars();
```

`bytes()` is zero-copy. `chars()` decodes Unicode scalar values without allocation. Owned, growing UTF-8 text uses `std::string::String` and an explicit allocator.

## References and raw pointers

- `&T` shares immutable access.
- `&mut T` grants exclusive mutable access for the borrow.
- `*const T` and `*mut T` are raw pointers used at FFI or low-level boundaries.

Dereferencing a reference is safe. Raw pointer operations require `unsafe`. Raw pointers are neither `Send` nor `Sync`.

```reimer
fn increment(value: &mut i32) {
    *value += 1;
}
```

## Structs

Structs define named fields and nominal identity.

```reimer
@derive(Copy, Clone, Debug, Default)
struct Point {
    x: f32,
    y: f32,
}

let origin = Point::default();
let point = Point { x: 3.0, y: 4.0 };
```

`@repr(C)` requests C-compatible field layout for supported FFI types. `@align(N)` can raise a type's alignment.

## Enums and patterns

Enums can have unit, tuple, or named-field variants.

```reimer
enum Packet {
    Empty,
    Position(f32, f32),
    Message { bytes: usize },
}
```

Patterns can bind fields, ignore values with `_`, and include guards. Matches over enums must be exhaustive.

## Functions and function pointers

Function signatures name parameter and return types:

```reimer
fn combine(left: i32, right: i32) -> i32 {
    left + right
}
```

Typed function pointers participate in threads and jobs. Capturing closures are not part of the experimental language surface.

## Generic and standard nominal types

`Option<T>` and `Result<T, E>` are built-in generic enums. Standard-library owners such as `String`, `Vec<T>`, `HashMap<K, V>`, and `tensor<T, Rank>` preserve nominal source names in diagnostics and editor hovers.

The [methods, traits, and generics](generics-and-traits.md) chapter explains constraints, monomorphization, and derive behavior.
