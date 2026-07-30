# Frozen decisions for Reimer v0.1

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
  -> native object for `reimer build`
  -> runtime + LLD for the final executable
```

Object generation is not linking. Each platform needs a startup runtime, ABI,
system libraries, and LLD configuration. M0 provides executable JIT and native
objects; standalone executables are a later backend increment.

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
