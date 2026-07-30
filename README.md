# Reimer

Reimer is a compiled language initially aimed at games, engines, and content
tools. The compiler is written in Rust, and its first backend generates machine
code through Cranelift.

The compiler implements milestones M0-M10 from the language design document.
In addition to the minimum program, it compiles every scalar and aggregate
type, complete control flow, recoverable errors, moves and references, modules
with `::` paths, C FFI, methods, type and `const` generics, monomorphization,
traits with static dispatch, `comptime` evaluation, attributes, closed derives,
typed reflection, and stable-address `static` storage with `static mut` guarded
by `unsafe`. Runtime `assert` and debug-only `debug_assert` preserve explicit
failure messages, and `std::target::os()` exposes the host as a typed enum. The
standard library includes explicit allocators, safe I/O, owned files with
explicit allocator-backed reads, recoverable slice access, allocation-free
UTF-8 byte and character iteration, scalar and vector math, `String`, `Vec`,
`HashMap`, `HashSet`, `RingBuffer`, contiguous tensors with safe views, threads,
synchronization, atomics, and a work-stealing job pool:

```reimer
fn factorial(value: i32) -> i32 {
    let mut current = value;
    let mut result = 1;
    while current > 1 {
        result *= current;
        current -= 1;
    }
    result
}
```

The pipeline covers `.reim` source -> tokens with spans -> AST -> name
resolution and type checking -> typed HIR -> Cranelift. `run` executes the
program through JIT, while `build` emits a native object.

## Usage

```text
cargo run -p reimer-cli -- check examples/exit_42.reim
cargo run -p reimer-cli -- emit-object examples/exit_42.reim
cargo run -p reimer-cli -- build examples/exit_42.reim
cargo run -p reimer-cli -- run examples/exit_42.reim
cargo run -p reimer-cli -- run examples/m1_language.reim
cargo run -p reimer-cli -- run examples/m2_scalars.reim
cargo run -p reimer-cli -- run examples/m2_composites.reim
cargo run -p reimer-cli -- run examples/m2_control.reim
cargo run -p reimer-cli -- run examples/m3_views.reim
cargo run -p reimer-cli -- run examples/m3_io.reim
cargo run -p reimer-cli -- run examples/m3_string.reim
cargo run -p reimer-cli -- run examples/m3_vec.reim
cargo run -p reimer-cli -- run examples/m3_collections.reim
cargo run -p reimer-cli -- run examples/m3_integer_overflow.reim
cargo run -p reimer-cli -- run examples/m3_slice_access.reim
cargo run -p reimer-cli -- run examples/m3_utf8.reim
cargo run -p reimer-cli -- run examples/m3_assertions.reim
cargo run -p reimer-cli -- run examples/m3_filesystem.reim
cargo run -p reimer-cli -- run examples/m3_math.reim
cargo run -p reimer-cli -- run examples/m5_ffi.reim
cargo run -p reimer-cli -- run examples/m6_generics.reim
cargo run -p reimer-cli -- run examples/m7_tensor.reim
cargo run -p reimer-cli -- run examples/m7_matmul.reim
cargo run -p reimer-cli -- check examples/m8_packages/app --locked
cargo run -p reimer-cli -- run examples/m8_packages/app --release --locked
cargo run -p reimer-cli -- test examples/m8_packages/app --locked
cargo run -p reimer-cli -- run examples/m9_threads/main.reim
cargo run -p reimer-cli -- run examples/m9_synchronization/main.reim
cargo run -p reimer-cli -- run examples/m9_atomics/main.reim
cargo run -p reimer-cli -- run examples/m9_jobs/main.reim
cargo run -p reimer-cli -- run examples/m9_tensor_parallel/main.reim
cargo run -p reimer-cli -- check examples/m10_comptime.reim
cargo run -p reimer-cli -- run examples/m10_comptime.reim
cargo run -p reimer-cli -- test examples/m10_comptime.reim
```

To create a project:

```text
reimer new game
reimer add physics --path ../physics --project game
reimer check game
reimer build game --release --locked
reimer doc game
```

The `reimer.toml` format, lockfile semantics, profiles, and path/Git
dependencies are documented in
[`docs/package-system.md`](docs/package-system.md). Compile-time evaluation,
attributes, and reflection are specified in
[`docs/metaprogramming.md`](docs/metaprogramming.md).
Safe owned file handles, UTF-8 paths, and explicit reads are documented in
[`docs/filesystem.md`](docs/filesystem.md).
Scalar functions and `Vec2`/`Vec3`/`Vec4` are documented in
[`docs/mathematics.md`](docs/mathematics.md).
`reimer doc` validates the complete package and writes its public `///`
documentation to `target/reimer/doc/<package>.md`.

The generated object is not yet a standalone executable. Linking it with the
startup runtime and LLD is the next backend increment.

## VS Code

`editors/vscode` contains TextMate highlighting and an extension connected to
`reimer-lsp`. The server publishes diagnostics, inferred types, hover,
completion, definitions, import organization, quick fixes for typos and
antipatterns, and static allocator reservation estimates. Installation and
packaging are described in
[`editors/vscode/README.md`](editors/vscode/README.md).

## Development

```text
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -D warnings
```

The architecture and exact milestone scope are described in
[`docs/architecture.md`](docs/architecture.md). Full LDD coverage is tracked in
[`docs/implementation-status.md`](docs/implementation-status.md).
