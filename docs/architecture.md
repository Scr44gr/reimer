# Initial Reimer architecture

## Implemented pipeline

1. `reimer-lexer` transforms UTF-8 into tokens and preserves byte spans.
2. `reimer-parser` builds an AST faithful to the syntax and recovers from
   errors at statement and declaration boundaries.
3. `reimer-resolver` resolves functions, bindings, and type names; it checks
   types, mutability, arity, storage cycles, and control flow.
4. `reimer-hir` represents the typed program through function, local, and
   composite-type IDs; the backend does not depend on the AST.
5. `reimer-layout` computes one native representation shared by reflection and
   codegen; `reimer-codegen-native` lowers HIR to Cranelift with checked
   overflow and division.
6. `reimer-project` validates `reimer.toml`, resolves path/Git dependencies,
   synchronizes `reimer.lock`, and builds a portable graph.
7. `reimer-cli` exposes the complete
   `new/init/check/build/run/test/doc/fmt/clean/add/remove` cycle and retains
   `emit-object` for standalone files.
8. `reimer-diagnostics` renders errors with a code, location, source excerpt,
   and optional help.
9. `reimer-package` discovers modules, resolves `::` imports, limits visibility
   to direct dependencies, and rewrites canonical names before type checking.
10. `reimer-lint` derives editor diagnostics, visible inference, antipatterns,
    and allocator estimates from the real frontend.
11. `reimer-lsp` serves those capabilities to VS Code through LSP and
    reanalyzes changes to a manifest or lockfile; the TextMate grammar is
    limited to lexical highlighting.
12. `std::tensor` builds owned tensors over `Vec<T>` and exposes scoped views;
    the resolver propagates borrows from aggregates containing references, and
    the backend copies aggregates with real value semantics.

## Scope decisions

- M1 recognizes functions with `i32`/`bool`/`()` parameters, explicit or
  omitted returns, value blocks, `let`, `let mut`, shadowing, simple and
  compound assignment, arithmetic/comparison/logical operators, direct calls,
  `if`, `while`, `break`, `continue`, and `return`.
- Imports, reexports, absolute paths, `self::`, and `super::` are resolved
  statically in multi-file packages.
- The only currently linkable entry point is `fn main() -> i32`.
- Cranelift is a replaceable backend. The AST contains no backend details.
- `run` uses JIT. For executable packages, `build` links the Cranelift object,
  a minimal startup shim, and the embedded runtime through LLD. The result has
  no separate Reimer runtime dependency. `emit-object` and library-package
  builds retain native object output.
- The runtime encapsulates FFI, allocator, and I/O boundaries; programs use
  safe standard-library wrappers.
- `std::c` aliases are resolved to the native target's actual scalar ABI before
  HIR lowering. They are transparent and introduce no wrapper layout. See
  [`c-interop.md`](c-interop.md).

`std::fs` follows the same boundary. Generated code passes bounded UTF-8 paths
and byte regions to a small runtime registry of opaque native file handles.
Files remain move-only source values, reads receive an explicit allocator, and
all public operations return recoverable errors. The detailed ownership and
buffer contracts are documented in [`filesystem.md`](filesystem.md).

`std::math` uses a smaller private ABI for scalar floating-point operations
that are not source-language operators. The ABI receives and returns only
`f32` or `f64`; safe wrappers provide explicit precision and the vector layer
is implemented entirely in source. See [`mathematics.md`](mathematics.md).

`std::time` keeps wall-clock timestamps separate from monotonic interval
measurement. Three scalar runtime symbols read Unix seconds, read a
process-local nanosecond counter, and park the current native thread through
the operating system. `Duration` and `Instant` remain safe source-level value
types. See [`time.md`](time.md).

`std::string` keeps allocation decisions in safe source APIs. Concatenation
precomputes its UTF-8 capacity, incremental formatting reuses an owned
`String`, typed interpolation selects statically dispatched `Display` or
`Debug`, and Unicode queries operate on borrowed views. Bounded runtime helpers
cover float formatting, full-width integer conversion, scalar encoding, and
Unicode case properties without hidden allocation. See
[`text-and-formatting.md`](text-and-formatting.md).

## Integer overflow

Ordinary integer `+`, `-`, and `*` operations are checked in every profile and
call the runtime panic boundary on overflow. Programs can select a different
addition policy explicitly:

```reimer
let value: u8 = 255;
let wrapped: u8 = value.wrapping_add(1);
let checked: Option<u8> = value.checked_add(1);
let saturated: u8 = value.saturating_add(1);
```

The resolver accepts these methods on every signed and unsigned integer type.
Cranelift lowering evaluates each operand once and uses native overflow flags:
`wrapping_add` keeps the low bits, `checked_add` constructs `Some(sum)` or
`None`, and `saturating_add` selects the correct minimum or maximum for the
integer width. No operation relies on host-language overflow behavior.

## Stable static storage

`const` names are substituted with values evaluated by the deterministic
compile-time engine. A `static` declaration instead creates one native
Cranelift data object with a stable address in both JIT and object output:

```reimer
static ANSWER: i32 = 42;
static mut COUNTER: i32 = 0;
```

Static initializers are evaluated at compile time and serialized through the
same native layouts used by generated code. Immutable statics can be read or
borrowed safely. Every read, write, or borrow of `static mut` requires an
`unsafe` block, and moving a non-`Copy` value out of any static is rejected.
Statics cannot contain borrowed views, strings, raw pointers, or function
values. Concurrent mutable state should use atomics, locks, or an encapsulated
synchronization API.

## Assertions and native target inspection

`assert(condition)` and `assert(condition, message)` check their condition in
every build profile. `debug_assert` has the same type rules but is enabled only
when profile optimization is `0`. When disabled, neither its condition nor its
message is evaluated, so debug checks cannot introduce release-build side
effects. Failed checks use the same bounded UTF-8 panic boundary as
`panic(message)`.

```reimer
assert(index < length, "index must be inside the buffer");
debug_assert(cached_len == length, "cached length is stale");
```

The native backend receives the optimization policy explicitly while lowering
HIR; assertion behavior does not depend on Rust's own build profile.

`std::target::os()` returns `OperatingSystem`, whose variants are `Windows`,
`Linux`, `MacOs`, `FreeBsd`, and `Other`. The safe public wrapper contains the
standard library's private runtime ABI call. Application code therefore does
not require `unsafe` to inspect the target.

## Slices and UTF-8

Checked `slice[index]` remains the concise access form and raises a bounds
panic for an invalid index. `slice.get(index)` and `slice.get_mut(index)`
instead return `Option<&T>` and `Option<&mut T>` without calling the panic
boundary. Their Cranelift lowering evaluates the view and index once, compares
against the descriptor length, and constructs `Some(reference)` or `None`.

`str.bytes()` creates an immutable `&[u8]` descriptor over the exact same UTF-8
storage without copying. `str.chars()` creates a scoped, allocation-free
`Chars` iterator. Its `next()` method and `for character in text.chars()` decode
Unicode scalar values through one bounded runtime ABI. The runtime validates
the byte boundary before creating a Rust slice; generated code owns the cursor
and never exposes raw pointers to source programs.

Frozen syntax and semantic decisions are recorded in
[`language-decisions-v0.1.md`](language-decisions-v0.1.md).

## M7 Tensor

`tensor<T, Rank>` uses contiguous row-major storage and preserves
`shape: [usize; Rank]` and `strides: [usize; Rank]`; allocations are never
hidden. `TensorView` and `TensorViewMut` store scoped slices: the type checker
prevents them from outliving the owned tensor or being hidden inside raw
storage. The `value[i, j]` syntax lowers to the type's checked indexing
protocol, so failures end in `panic`, never an out-of-bounds access. Kernels
that write results receive an explicit output.

## M8 Packages

The declarative system is described in
[`package-system.md`](package-system.md). Path identities are relative to the
root so the lockfile survives relocation. Git is pinned to a commit;
`--locked` prevents drift. A root with `src/package.reim` resolves as a library
without inventing a `main`.

## M9 Concurrency

The runtime maintains native threads, persistent workers, local queues, and
work stealing. `Send` and `Sync` are derived structurally; raw pointers
implement neither. `std::thread` encapsulates locks, atomics, channels,
barriers, semaphores, and thread-local state. `std::job` provides a fixed pool,
typed jobs, and `parallel_for_mut` for slices, arrays, and tensors.

Each JIT execution owns a resource session. Cleanup waits only for that
session's threads and pools before releasing generated code, so two concurrent
compilations do not interfere. Private ABI boundaries keep documented `unsafe`
blocks; user code sees only safe wrappers.

The API and its invariants are described in
[`concurrency.md`](concurrency.md).

## M10 Comptime

The resolver evaluates constants, functions, and `comptime` blocks without
delegating operations to the host. It enforces step, depth, and memory budgets
and rejects I/O, FFI, threads, pointers, borrows, and runtime calls. The same
validated layout consumed by Cranelift powers `size_of<T>()` and
`align_of<T>()`, avoiding divergent layout calculators.

Attributes are preserved in AST/HIR and validated for their targets. Derives
form a closed, structural list; `Clone` cannot hide an allocator. `@test`
registers unit functions, and the CLI executes each one in an isolated JIT
process. `@must_use` is published as a linter/LSP diagnostic.

Syntax, limits, and guarantees are described in
[`metaprogramming.md`](metaprogramming.md).
