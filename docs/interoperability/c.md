# C interoperability

`std::c` provides target-correct aliases and small helpers for declaring native
ABIs. The aliases are transparent: they use the matching scalar layout and call
convention without wrapping values in a structure.

## Type mapping

Use the module-qualified names in bindings:

```reimer
import std::c;

@link("native")
extern "C" {
    fn seek(handle: *mut c::Void, offset: c::Long) -> c::Int;
}
```

The module exports `Char`, `SignedChar`, `UnsignedChar`, `Short`,
`UnsignedShort`, `Int`, `UnsignedInt`, `Long`, `UnsignedLong`, `LongLong`,
`UnsignedLongLong`, `Float`, `Double`, `Size`, `PtrDiff`, `Bool`, `Str`, and
`Void`.

`Long`, `UnsignedLong`, and plain `Char` are selected from the compiler's native
target. In particular, `Long` is 32-bit on 64-bit Windows and 64-bit on the
usual 64-bit Linux and macOS targets. `Size` and `PtrDiff` follow pointer width.
The remaining integer aliases have the widths required by the supported native
C ABIs.

Because aliases are transparent, arithmetic and diagnostics use the underlying
scalar rules. A `std::c::Int` is ABI-identical to the target's C `int`, not an
allocation-owning wrapper.

## Pointers and buffers

`null<T>` and `null_const<T>` create typed null pointers. `is_null` and
`is_null_mut` inspect pointers without dereferencing them.

`ConstBuffer<T>` and `Buffer<T>` are `@repr(C)` pointer/count records. They do
not own or validate the pointed-to memory. Constructing or checking the record
is safe; dereferencing its pointer remains an `unsafe` operation governed by
the original native API's lifetime and alignment contract.

External declarations may pass and return ABI-safe `@repr(C)` structs by value
on the supported 64-bit targets. The backend implements Microsoft x64, System V
AMD64, generic AAPCS64, and Apple's AArch64 variant. This includes mixed
integer/SSE eightbytes, register exhaustion rollback, stack aggregates,
homogeneous floating-point aggregates, indirect large values, and hidden
structure-return pointers. Padded temporary storage is zero-initialized so the
lowering never reads beyond a source value or exposes uninitialized padding.

The backend tests each classifier independently and exercises Microsoft x64
against real `extern "C"` functions on Windows. Big-endian AArch64, 32-bit
targets, and other unimplemented aggregate ABIs are rejected with a backend
diagnostic instead of using guessed rules. Raw-pointer FFI remains available on
every currently supported native target.

`int_from_bool` and `bool_from_int` convert conventional zero/nonzero integer
booleans. Use `Bool` only when the C declaration specifically uses `_Bool` or
`bool`.

## Callbacks and loaded functions

An external signature may accept or return a typed function pointer when every
parameter and return value is ABI-safe:

```reimer
extern "C" fn install(callback: fn(i32) -> i32);
```

This supports C callbacks and APIs such as Vulkan that load commands at
runtime. Reinterpreting a raw address or one function signature as another
requires an explicit `unsafe` block:

```reimer
let loaded = unsafe { get_proc_address(c"draw") };
let draw = unsafe { loaded as fn(u32, u32) -> () };
```

The caller must prove that the address is non-null, remains valid for every
call, uses the platform C calling convention, and exactly matches the target
signature. Passing references, slices, `str`, or owning Reimer values through
a C callback remains rejected.

## Safety boundary

Declaring and calling an external function still requires `unsafe`. A signature
describes an ABI, but the compiler cannot prove that a foreign pointer is live,
aligned, initialized, or retained for the correct lifetime. A binding should
keep its `extern "C"` block private and expose a safe source-language wrapper
after validating every native contract.

`Str` is a borrowed NUL-terminated pointer. A `c"..."` literal is valid only for
the native call in which it is used; it cannot escape as an owned string.
