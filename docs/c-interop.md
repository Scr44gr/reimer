# C interoperability helpers

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

Pass these records through raw pointers in current bindings. By-value C
aggregate argument classification is intentionally deferred until a supported
binding requires and tests each platform ABI case.

`int_from_bool` and `bool_from_int` convert conventional zero/nonzero integer
booleans. Use `Bool` only when the C declaration specifically uses `_Bool` or
`bool`.

## Safety boundary

Declaring and calling an external function still requires `unsafe`. A signature
describes an ABI, but the compiler cannot prove that a foreign pointer is live,
aligned, initialized, or retained for the correct lifetime. A binding should
keep its `extern "C"` block private and expose a safe source-language wrapper
after validating every native contract.

`Str` is a borrowed NUL-terminated pointer. A `c"..."` literal is valid only for
the native call in which it is used; it cannot escape as an owned string.
