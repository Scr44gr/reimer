# Implementation status

This matrix follows the v0.1 LDD roadmap and the decisions frozen in
[language decisions](language-decisions.md). A milestone is only
complete when it has a specification, implementation, positive and negative
tests, and an executable `.reim` program.

| Milestone | Status | Current evidence | Remaining work |
|---|---|---|---|
| M0 - Skeleton | Complete | Rust workspace, CLI, spans, diagnostics, JIT, native objects, and LLD-linked standalone executables with an embedded startup/runtime; `examples/exit_42.reim` | Library archive formats remain separate from executable linking |
| M1 - Basic language | Complete | Precedence, `let`/`let mut`, shadowing, assignment, functions, `if`, `while`, `break`, `continue`, typed HIR, and `examples/m1_language.reim` | Additional types and control flow belong to M2 |
| M2 - Types | Complete | All scalars (`i8`...`i128`, `u8`...`u128`, pointer-sized integers, `f32`/`f64`, `bool`, `char`, `()`, and `never`); decimal, hexadecimal, binary, and octal integer literals with checked `_` separators; structs, enums, tuples, arrays, slices, and references with native layouts; constructors, fields, checked indexes, place mutation, aggregate ABI; exhaustive `match` with guards and patterns, `for`, and value-producing `loop`; four executable M2 programs | - |
| M3 - Errors and memory | Complete | `&T`/`&mut T`, raw pointers limited by `unsafe`, stable-address native `static` data and `static mut` access guarded by `unsafe`, checked slices with recoverable `get`/`get_mut`, UTF-8 `str` as `(pointer, length)` with zero-copy bytes and Unicode scalar iteration, `Option`, `Result`, `?`, implicit moves, `Copy`, `defer`, `panic`, always-on `assert`, and profile-controlled `debug_assert`; general, page, arena, and fixed-buffer allocators with recoverable OOM; safe stdin/stdout/stderr with bounded reads, partial/full writes, flush, terminal detection, and zero-copy UTF-8 conversion; owned files and UTF-8 paths with explicit allocator-backed reads and recoverable operations; wall and monotonic clocks, exact `Duration`/`Instant` measurement, and CPU-idle blocking sleep; scalar `f32`/`f64` math and `Vec2`/`Vec3`/`Vec4`; growing `String`, allocator-aware concatenation, primitive formatting through 128 bits, typed `f"..."` interpolation with statically dispatched `Display` and `Debug`, Unicode properties and full scalar case mappings; owned `Vec`, SIMD-grouped flat `HashMap`/`HashSet`, and `RingBuffer`; twenty-one positive M3 programs, missing-`Display` and missing-`Debug` diagnostic fixtures, and native tests | - |
| M4 - Modules | Complete | Multi-file discovery, `package.reim`, selective and module imports, aliases, reexports, `self::`/`super::`, absolute `x::y::z` access, privacy, complete cycle and ambiguity diagnostics by file; executable `examples/m4_modules/main.reim` | - |
| M5 - FFI | Complete | `@link(...) extern "C"` blocks, preserved symbols, `@repr(C)`, `cstr` and validated `c"..."` literals, ABI-safe types, mandatory `unsafe` calls, safe wrappers, JIT dynamic loading, COFF `/DEFAULTLIB` directives, and LLD-linked executables; transparent target-correct aliases for C scalars, typed null pointers, ABI-safe C callbacks, checked `unsafe` casts from loaded addresses to typed functions, and `@repr(C)` pointer/count buffers in `std::c`; `examples/m5_ffi.reim` and `examples/m5_c_types.reim` test scalar calls and ABI aliases, while `examples/m5_sdl_window.reim` opens a real SDL3 window and `examples/m5_sdl_opengl.reim` integrates SDL3, OpenGL, tensor storage, an explicit allocator, `Result`, and `defer`; reproducible official downloads with SHA-256 verification in the SDL scripts | C unions, variadics, and by-value aggregate classification remain for bindings that require them |
| M6 - Generics | Complete | Type and `const` parameters, defaults, bounds, and `where` clauses; demand-driven cached monomorphization of functions, structs, enums, associated functions, and methods; inference from arguments and expected returns; traits, supertraits, basic coherence, contract validation, `Copy` markers, and static dispatch; executable `examples/m6_methods.reim` and `examples/m6_generics.reim`, with the latter verified by JIT result `42` | - |
| M7 - Tensor | Complete | Move-only `tensor<T, Rank>` with contiguous `Vec<T>` and explicit allocator; row-major shape/strides, recoverable shape overflow, scoped `TensorView`/`TensorViewMut`, reference-returning `get`, checked multidimensional `[]`, `fill`, `multiply_scalar`, `add_into`, and `matmul_into`; `m7_tensor.reim` and `m7_matmul.reim` demos, lifetime tests, and real aggregate copies | - |
| M8 - Packages | Complete | Strict `reimer.toml`, SemVer, profiles, reproducible portable lockfile, checksums, path/Git sources pinned by commit, unification, cycles, and direct visibility; CLI `new/init/check/build/run/test/doc/fmt/clean/add/remove`; executable and library packages, ordered tests, `m8_packages` example, manifest-aware linter/LSP | Registry and publishing belong to a later LDD milestone |
| M9 - Concurrency | Complete | Typed function pointers; structural `Send`/`Sync` without raw pointers; native and scoped threads; `Mutex`, `RwLock`, `Channel`, `Barrier`, `Semaphore`, atomics, and `ThreadLocal`; fixed pool with local queues/work stealing, typed jobs, and `parallel_for_mut` for slices, arrays, and tensors; isolated JIT sessions; five positive demos and three negative borrow/transfer tests | Async, fibers, an ECS scheduler, and configurable atomic orderings are outside v0.1 |
| M10 - Comptime | Complete | Typed global constants and const generics; deterministic `comptime` functions/blocks with limits; `@repr`, `@derive`, `@align`, `@inline`, `@test`, and `@must_use`; closed derives `Copy`, `Clone`, `Debug`, `Eq`, `Hash`, and `Default`; typed `size_of`, `align_of`, and `meta::*` reflection; layout shared by reflection and Cranelift; isolated unit tests and JIT-verified `examples/m10_comptime.reim` | `@inline` is a frontend hint; advanced inlining policy and runtime reflection are outside v0.1 |

## Editor tooling

`reimer-lint` reuses the compiler's lexer, parser, resolver, package graph, and
HIR to produce diagnostics, typo quick fixes, inferred types, antipatterns, and
static allocator estimates. `reimer-lsp` publishes them through LSP together
with hover, inlay hints, completion, definitions, symbols, CodeLens, and import
organization. Compiler-linked rename follows declaration identity without
touching shadowed names. Changes to an open module rebuild only that module and
open importers while resolving every unsaved buffer as an overlay; changes to
`reimer.toml` or `reimer.lock` reanalyze all open documents.
`editors/vscode` provides TextMate highlighting and bundles the server and
client so `.reim` files work without a Rust or Pylance extension.

## Full LDD surface audit

The milestone table proves the vertical v0.1 roadmap, not every API proposed
elsewhere in the draft. The following items remain open and must not be
reported as implemented:

| LDD area | Implemented | Remaining |
|---|---|---|
| Constants and stable storage | Typed compile-time `const` values; stable-address native `static` data in JIT and objects; compile-time initializers; safe immutable borrows; `unsafe` required for every `static mut` access; non-`Copy` moves rejected; package imports, generated docs, hover, completion, and highlighting | - |
| Assertions and native target | `assert` with an optional failure message in every profile; `debug_assert` omitted with its operands in optimized profiles; both supported by `comptime`; typed, safe `std::target::os()` host inspection | - |
| Integer literals and overflow APIs | Decimal, hexadecimal, binary, and octal forms; checked `_` separators; contextual range validation through `u128`; checked operators panic in every profile; constant overflow is rejected; every integer type provides `wrapping_add`, `checked_add`, and `saturating_add` | - |
| Slices and UTF-8 views | Checked `[]`; recoverable `get`/`get_mut`; slice iteration; validated `str`; zero-copy `bytes()`; allocation-free `chars()` with `next()` and `for` | - |
| Standard library plan | `alloc`, `c`, `collections`, `env`, `fmt`, `fs`, `io`, `math`, `process`, `string`, `target`, `time`, `thread`, `job`, and `tensor`; UTF-8 command-line arguments, environment and current-path inspection, process identity, direct scoped child processes with child-only environment configuration, wall and monotonic clocks, `Duration`, `Instant`, CPU-idle blocking sleep, allocator-aware concatenation, primitive formatting, typed interpolated strings with distinct `Display` (`{value}`) and `Debug` (`{value:?}`) contracts, Unicode queries, and full scalar case mappings | - |
| Tooling | Formatter, checker, tests, generated `///` Markdown, hover, completion, definitions, compiler-linked intradocument rename, dependency-aware incremental snapshots, CodeLens, and diagnostics | - |
| First integrated demo | `examples/m5_sdl_opengl.reim` combines SDL3/OpenGL setup, allocator-backed RGBA tensor data, recoverable `Result` paths, two ordered `defer` cleanups, drawing, presentation, and native teardown; `scripts/demos/run-sdl-opengl.ps1` supplies a verified SDL3 runtime | - |

## Deliberate differences from the draft

- Native Cranelift generation replaced the C backend.
- Paths use `::`; `.` is reserved for fields and methods.
- The only unit type is `()`.
- Moves of non-`Copy` values are implicit.
- `str` is a non-owning UTF-8 `(pointer, length)` view.
- Derived `Clone` only exists for values whose copy is infallible and
  allocation-free; owned copies still expose their allocator or resource
  operation.

## Permanent gates

Every milestone must keep these commands green:

```text
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -D warnings
```

In addition, `reimer check` must run the complete frontend without codegen,
`reimer run` must execute at least one representative program, `emit-object`
must produce a valid native object, and `reimer build` must produce a runnable
standalone executable for executable packages.
