# LDD implementation coverage

This matrix follows the v0.1 LDD roadmap and the decisions frozen in
[`language-decisions-v0.1.md`](language-decisions-v0.1.md). A milestone is only
complete when it has a specification, implementation, positive and negative
tests, and an executable `.reim` program.

| Milestone | Status | Current evidence | Remaining work |
|---|---|---|---|
| M0 - Skeleton | Complete | Rust workspace, CLI, spans, diagnostics, JIT, and native object; `examples/exit_42.reim` | Standalone linking belongs to the native runtime |
| M1 - Basic language | Complete | Precedence, `let`/`let mut`, shadowing, assignment, functions, `if`, `while`, `break`, `continue`, typed HIR, and `examples/m1_language.reim` | Additional types and control flow belong to M2 |
| M2 - Types | Complete | All scalars (`i8`...`i128`, `u8`...`u128`, pointer-sized integers, `f32`/`f64`, `bool`, `char`, `()`, and `never`); structs, enums, tuples, arrays, slices, and references with native layouts; constructors, fields, checked indexes, place mutation, aggregate ABI; exhaustive `match` with guards and patterns, `for`, and value-producing `loop`; three executable M2 programs | - |
| M3 - Errors and memory | Complete | `&T`/`&mut T`, raw pointers limited by `unsafe`, stable-address `static` data and `static mut` access guarded by `unsafe`, checked slices with recoverable `get`/`get_mut`, UTF-8 `str` as `(pointer, length)` with zero-copy bytes and Unicode scalar iteration, `Option`, `Result`, `?`, implicit moves, `Copy`, `defer`, `panic`, always-on `assert`, and profile-controlled `debug_assert`; general, page, arena, and fixed-buffer allocators with recoverable OOM; safe stdin/stdout/stderr with bounded reads, partial/full writes, flush, terminal detection, and zero-copy UTF-8 conversion; owned files and UTF-8 paths with explicit allocator-backed reads and recoverable operations; growing `String` with `clone_in`; owned `Vec`, `HashMap`, `HashSet`, and `RingBuffer`; sixteen M3 programs and native tests | - |
| M4 - Modules | Complete | Multi-file discovery, `package.reim`, selective and module imports, aliases, reexports, `self::`/`super::`, absolute `x::y::z` access, privacy, complete cycle and ambiguity diagnostics by file; executable `examples/m4_modules/main.reim` | - |
| M5 - FFI | Complete | `@link(...) extern "C"` blocks, preserved symbols, `@repr(C)`, `cstr` and validated `c"..."` literals, ABI-safe types, mandatory `unsafe` calls, safe wrappers, JIT dynamic loading, and COFF `/DEFAULTLIB` directives; `examples/m5_ffi.reim` tests scalar calls and `examples/m5_sdl_window.reim` opens and closes a real SDL3 window through `defer`; reproducible official download with SHA-256 verification in `scripts/run-sdl-demo.ps1` | Full standalone linking belongs to runtime/tooling; by-value C aggregate classification can grow when a real binding requires it |
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
organization. Changes to `reimer.toml` or `reimer.lock` reanalyze open
documents. `editors/vscode` provides TextMate highlighting and bundles the
server and client so `.reim` files work without a Rust or Pylance extension.
The active in-memory document participates in full package resolution before
it is saved.

## Full LDD surface audit

The milestone table proves the vertical v0.1 roadmap, not every API proposed
elsewhere in the draft. The following items remain open and must not be
reported as implemented:

| LDD area | Implemented | Remaining |
|---|---|---|
| Constants and stable storage | Typed compile-time `const` values; stable-address native `static` data in JIT and objects; compile-time initializers; safe immutable borrows; `unsafe` required for every `static mut` access; non-`Copy` moves rejected; package imports, generated docs, hover, completion, and highlighting | - |
| Assertions and native target | `assert` with an optional failure message in every profile; `debug_assert` omitted with its operands in optimized profiles; both supported by `comptime`; typed, safe `std::target::os()` host inspection | - |
| Integer overflow APIs | Checked operators panic in every profile; constant overflow is rejected; every integer type provides `wrapping_add`, `checked_add`, and `saturating_add` | - |
| Slices and UTF-8 views | Checked `[]`; recoverable `get`/`get_mut`; slice iteration; validated `str`; zero-copy `bytes()`; allocation-free `chars()` with `next()` and `for` | - |
| Standard library plan | `alloc`, `collections`, `fs`, `io`, `string`, `target`, `thread`, `job`, and `tensor` | Planned `c` and `math` modules plus broader formatting/Unicode APIs |
| Tooling | Formatter, checker, tests, generated `///` Markdown, hover, completion, definitions, CodeLens, and diagnostics | Rename and dependency-aware incremental analysis |
| First integrated demo | SDL/OpenGL, tensor, allocator, `Result`, and `defer` are each exercised | One program combining the complete LDD section 22.1 scenario |

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
`reimer run` must execute at least one representative program, and
`reimer build` must produce a valid native object until the startup runtime can
link standalone executables.
