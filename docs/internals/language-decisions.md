# Language decisions for the experimental release

This document records design decisions accepted before continuing the frontend
and native backend. They take precedence over contradictory proposals in the
v0.1 draft LDD.

## D-001: `::` paths

`::` separates modules, types, and associated symbols. `.` is reserved for
field access and method calls on a value.

```reimer
from game::math import Vec3;
from engine::render import Texture as GpuTexture;
from self::animation import Animator;
from super::shared import AssetId;

let origin = Vec3::zero();
let texture = engine::render::load_texture(path)?;
player.position = origin;
player.update(delta);
```

An absolute path starts with a visible package. Relative paths begin with
`self::` or one or more `super::` segments. `import *` is not supported.

Import forms:

```text
import_item =
    [ "pub" ] , "import" , path , [ "as" , identifier ] , ";"
  | [ "pub" ] , "from" , path , "import" , import_name ,
    { "," , import_name } , [ "," ] , ";" ;

import_name = identifier , [ "as" , identifier ] ;
path = identifier , { "::" , identifier } ;
```

`from x::y import z;` introduces `z` into the current module.
`import x::y;` introduces the final segment (`y`) unless an alias is present.
A complete path such as `x::y::z()` can be used without an import when `x` is
a visible package.

## D-002: unit type

The only unit type is `()`. A function without a written return type returns
`()`. `void` is not a reserved word.

```reimer
fn update() {
    return ();
}

fn save() -> Result<(), SaveError> {
    return Ok(());
}
```

In FFI, a C function with no return value is represented as a Reimer function
returning `()`. No second unit-like type is introduced.

## D-003: implicit moves on consumption

Non-`Copy` values move automatically when assigned, passed by value, returned,
or sent through a channel. The `move` prefix expression is removed from v0.1.

```reimer
let texture = Texture::load(allocator, path)?;
let second = texture;
render(texture); // error: `texture` was already moved
render(second);  // valid; consumes `second`
```

`&T` and `&mut T` borrows do not consume a value. Only types implementing
`Copy` are duplicated implicitly.

## D-004: `str` representation

`str` is an immutable, non-owning, `Copy` UTF-8 view represented conceptually
as `(pointer, length)`. It is passed by value and never allocates. v0.1 does not
spell it `&str`.

```reimer
fn load(path: str) -> Result<Asset, AssetError>;

let name: str = "player";
let owned = String::from(allocator, name)?;
let view: str = owned.as_str();
let bytes: &[u8] = view.bytes();
for character in view.chars() {
    inspect(character);
}
```

`String` is the move-only owned buffer with a length, capacity, and allocator.
A `str` view cannot outlive the storage containing its bytes. The type checker
applies the same conservative scoped rules used for other non-owning views.
`bytes()` returns a zero-copy byte slice. `chars()` decodes Unicode scalar
values without allocating; grapheme segmentation remains an optional Unicode
library concern.

## D-005: explicit copies of owned values

v0.1 supports `Clone` only as a closed derive for values whose copy is
infallible, allocation-free, and allocator-independent. A uniform call cannot
hide allocations, failures, or resource-specific semantics.

- `Copy` means implicit, bitwise, infallible duplication without an allocator.
- Derived `Clone` means explicit duplication with the same restrictions.
- Owned containers provide `clone_in(allocator)`.
- Resources provide semantic operations such as `duplicate_handle()` or
  `retain()`.

```reimer
@derive(Copy, Clone)
struct Point {
    x: f32,
    y: f32,
}

let second = point.clone();
let copy = bytes.clone_in(allocator)?;
let handle = texture.duplicate_handle()?;
```

An owned type, a non-`Copy` field, or any copy that can fail prevents deriving
`Clone`. Once enough real implementations exist, a `CloneIn` trait may be
added while keeping the allocator and possible failure visible in the
signature.

## D-006: native backend

The C backend is removed. The initial backend uses Cranelift to generate
machine code and native objects.

```text
.reim source
  -> lexer and parser
  -> resolution and type checking
  -> typed HIR
  -> Cranelift IR
  -> JIT for `reimer run`
  -> native object for `reimer emit-object`
  -> startup + embedded runtime + LLD for `reimer build`
```

Object generation and linking remain separate stages. Executable builds add a
minimal startup shim that calls `program_main`, cleans up session-owned threads
and job pools, and returns the source program's `i32` exit code. The compiler
embeds its matching runtime archive and invokes the bundled LLD through the
Rust toolchain that built it. The resulting executable has no separate Reimer
runtime dependency. Library packages continue to emit objects because their
archive and shared-library ABI needs a separate decision.

Operations with Reimer semantics are not delegated blindly to the host:

- checked integer overflow;
- checked division and remainder by zero;
- validated shifts;
- active bounds checks;
- traps converted to runtime `panic`.

Lowering must use checked Cranelift instructions or explicit blocks that call
the runtime. It must never depend on undefined behavior from another language.

## D-007: safe standard I/O

`std::io` exposes `stdin()`, `stdout()`, and `stderr()` as safe handles. Public
operations return `Result`; user code does not need `unsafe` to read, write, or
flush.

```reimer
from std::alloc import general_allocator;
from std::io import println, stdin;

let allocator = general_allocator();
println("Enter a line:")?;
let input = stdin();
let line = input.read_line(&allocator, 4096)?;
defer line.deinit();
```

Reads always receive an explicit capacity and return an owned `InputBuffer`
whose initialized length is separate from allocated capacity. `read_line`
retains the newline, `read_exact` distinguishes early EOF, and `read_to_end`
stops at EOF or capacity.

The private boundary between `std::io` and the runtime uses pointers bounded by
`(pointer, length)`. All `unsafe` is concentrated in these adapters,
documented with preconditions, and covered by tests; it is not part of the
program's safe public API. Locks, line iterators, `read_to_string`, vectored
I/O, and generic adapters are completed alongside traits, `String`, and owned
containers.

## D-008: tensors and scoped views

`tensor<T, Rank>` is owned and move-only. `Rank` is a const generic, shape is a
runtime value, and initial storage is contiguous row-major. `TensorView` and
`TensorViewMut` are scoped aggregates: they may contain slices, but the
resolver propagates their borrow and prevents them from being hidden in
raw-pointer-backed owned storage.

```reimer
let shape: [usize; 2] = [1000, 3];
let mut positions: tensor<f32, 2> =
    tensor::zeros(allocator, shape)?;
defer positions.deinit();

positions[10, 2] = 42.0;
let position = positions.get([10, 2]); // Option<&f32>
let view = positions.view();
```

An operation that creates storage receives an allocator. Nonallocating kernels
such as `add_into` and `matmul_into` receive an explicit mutable output.
`[]` access retains bounds checks in every profile.

## D-009: declarative packages and portable lockfile

`reimer.toml` describes SemVer identity, edition, dependencies, and profiles.
It permits neither build scripts nor executable plugins. A package may only
import direct dependencies, and cycles are rejected before the frontend.

```toml
[package]
name = "game"
version = "0.1.0"
edition = "2026"

[dependencies]
physics = { path = "../physics", version = "^0.1" }
assets = { git = "https://example.com/assets.git", tag = "0.4.0" }
```

`reimer.lock` pins the exact graph, path-source checksums, and Git commits.
Paths are serialized relative to the root: relocating the complete tree does
not change the lockfile. `--locked` fails when the lockfile is missing or has
drifted; `--refresh` resolves Git references again.

`src/main.reim` defines an executable and `src/package.reim` a library.
Integration tests under `tests/` are independent programs whose `main` returns
`0` on success. `debug` and `release` profiles control real Cranelift
optimization strategy.

## D-010: structured concurrency and isolated runtime

`Send` and `Sync` are structural capabilities known to the compiler. An
aggregate satisfies one only when every field satisfies it; raw pointers are
neither `Send` nor `Sync`. Native threads receive only owned data, and local
borrows are limited to `scope`, which guarantees a join before returning.

The job system uses a fixed number of persistent workers, local queues, and
work stealing. `parallel_for_mut` creates nonoverlapping mutable slices and
waits for every chunk in the same call. The original array, slice, or tensor
remains exclusively borrowed until that point.

Locks, channels, atomics, barriers, semaphores, and thread-local storage are
safe `std::thread` APIs. v0.1 atomics are sequentially consistent. ABI calls
and function thunks remain private and document every use of `unsafe`.

Each JIT program receives a runtime session. Its threads and pools preserve
that identity, and the backend waits for or destroys only the resources of the
session being terminated. Concurrent compilations cannot interfere, and
generated code cannot continue after its module is released.

## D-011: constants and stable static storage

`const` declares a typed compile-time value and does not allocate runtime
storage. `static` declares one native data object whose address remains stable
for the program's lifetime. Both forms require an explicit type and a
deterministic compile-time initializer.

```reimer
const BUFFER_SIZE: usize = 4096;
static ANSWER: i32 = 42;
static mut COUNTER: i32 = 0;
```

Immutable statics are safe to read and borrow. Every access to `static mut`
requires `unsafe`, including reads and borrows, because the compiler cannot
prove that unsynchronized global mutation is race-free. Concurrent state must
prefer atomics, locks, or safe encapsulated APIs. A non-`Copy` value cannot be
moved out of static storage.

Static storage accepts owned scalar and aggregate values. Borrowed references,
slices, UTF-8 views, raw pointers, and function values are rejected in v0.1.
The native backend emits real Cranelift data objects for JIT and object builds;
it does not emulate stable storage with a function stack slot.

## D-012: assertions and native target identity

`assert(condition)` and `assert(condition, message)` are language intrinsics.
They require a `bool` condition, accept an optional `str` message, return `()`,
and remain active in every profile. The default message is
`"assertion failed"`.

`debug_assert` has the same signature. It is enabled exactly when the selected
profile has `optimization = 0`. Optimized builds do not evaluate either
operand. During deterministic `comptime` evaluation, both assertion forms are
checked because compile-time validation has no runtime build branch.

The host-native backend exposes target identity through the safe standard
library API `std::target::os() -> OperatingSystem`. The enum distinguishes
Windows, Linux, macOS, FreeBSD, and other hosts. Its private ABI boundary is
implemented by the standard library; user code does not need `unsafe`.

## D-013: owned files and UTF-8 paths

`std::fs::File` owns an opaque native handle and is move-only. Programs close
it explicitly with `deinit`, normally registered through `defer`. Opening,
creating, appending, reading, writing, flushing, renaming, and removing return
recoverable errors.

File reads receive an allocator. `read` and `read_exact` use caller-selected
bounds; `read_to_end` allocates the exact unread regular-file length through
that allocator. Initialized byte length remains distinct from allocation
capacity, and UTF-8 validation is required before a file buffer becomes
`String`.

Paths use bounded UTF-8 `str` views in v0.1. The standard library encapsulates
the native ABI, so application code performs ordinary file and path operations
without `unsafe`.

## D-014: explicit scalar precision and nominal vectors

`std::math` uses unsuffixed `f32` functions and explicit `_f64` variants.
Rendering, simulation, and tensor code therefore remain concise without
silently changing precision. Constants follow the same convention.

`Vec2`, `Vec3`, and `Vec4` are nominal, `Copy`, single-precision structures.
They expose named arithmetic methods instead of operator overloads. Normalizing
an exactly zero vector returns `Option` rather than producing a hidden
division-by-zero result. Vector operations allocate no memory.

## D-015: transparent target-correct C aliases

The language supports non-generic transparent aliases with
`pub type Name = ExistingType;`. Aliases are resolved before HIR lowering and
therefore do not create wrapper layouts, conversions, or runtime costs.

`std::c` uses aliases selected from the native compiler target. C `long` is
therefore 32-bit on 64-bit Windows and 64-bit on the usual 64-bit Unix targets.
Bindings should use `std::c` names rather than assuming that C `long` matches
`isize`. Typed null pointers and pointer/count records remain non-owning; native
calls and pointer dereferences still require `unsafe`.

## D-016: explicit text allocation and concatenation

`str` remains a borrowed UTF-8 view, while every operation that creates an
owned `String` receives an allocator and returns `Result`. String `+` is not
overloaded because an infallible arithmetic spelling would hide both the
allocation strategy and possible failure.

```reimer
let label = concat(&allocator, "score: ", "42")?;
let divider = repeat(&allocator, "-", 40)?;
builder.push_string(&label)?;
```

`concat`, `concat3`, `repeat`, and `join_strings` precompute their required
capacity. Incremental `push_*` methods reuse an existing `String`. Integer
formatting covers the full 128-bit range, float formatting uses a shortest
round-trippable representation, and Unicode case conversion returns an owned
string because one scalar can map to multiple scalars.

Typed interpolation distinguishes user-facing `Display` (`{value}`) from
developer-facing `Debug` (`{value:?}`). Nominal values implement the selected
trait explicitly, and both contracts write into the caller's existing
`String` through a safe `Formatter`.

Small bounded runtime helpers implement conversions not directly available in
Cranelift. They write only into caller-owned capacity and perform no hidden
allocation. The public standard-library surface remains safe.

## D-017: separate wall and monotonic time

`std::time::time()` and `unix_time()` expose wall-clock seconds relative to the
Unix epoch. They are timestamps and may move when the operating system adjusts
its civil clock. Elapsed-time measurements instead use the process-local
monotonic clock through `Instant`, `monotonic`, or `perf_counter`.

`Duration` is a small `Copy` value containing whole seconds and normalized
subsecond nanoseconds. Integer constructors avoid normalization overflow, and
total-unit conversions saturate at `u64::MAX`. `Instant` stores exact monotonic
nanoseconds and computes saturating differences so an invalid ordering cannot
underflow.

`sleep(Duration)` parks the current native thread through the host operating
system. It is a blocking operation, not an active polling loop, and therefore
does not consume a CPU core while waiting. Async timers remain outside v0.1.
The safe standard-library wrapper contains the private runtime ABI call.

## D-018: UTF-8 process context and scoped children

`std::env` exposes command-line arguments, environment lookup, the current
directory, and the current executable through explicit UTF-8 conversion.
Native strings that cannot be represented as Reimer `String` return
`EnvError::NotUnicode`. JIT execution receives arguments after the CLI's `--`
separator; standalone executables use their native argument list.

Process-global environment mutation is not part of the safe API because it is
not safely portable in multithreaded programs. `std::process::Command` instead
supports child-specific `env`, `env_remove`, and `env_clear` configuration.

Commands invoke executables directly and do not pass arguments through a
shell. Windows `.bat` and `.cmd` scripts are rejected because their escaping
rules require a command interpreter. Standard streams are inherited by
default.

`Command::status` consumes and waits for a command. `Command::spawn` transfers
ownership to a move-only `Child`; `wait` consumes and collects it, while
`deinit` terminates and collects a still-running child for deterministic scoped
cleanup. Exit codes are optional because signal-style termination does not
always have a numeric code. All public APIs are safe; raw buffers and native
handles remain private to the runtime boundary.
