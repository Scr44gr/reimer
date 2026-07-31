# A guided tour

This chapter shows the shape of a Reimer program before the later chapters explain each rule precisely.

## Entry point and output

Executable source files define `fn main() -> i32`. The returned integer becomes the native process exit code after a standalone build. Standard output is fallible, so printing returns `Result<(), IoError>`.

```reimer
from std::io import println;

fn main() -> i32 {
    match println("Hello from Reimer") {
        Ok(_) => 0,
        Err(_) => 1,
    }
}
```

Run a single source file directly:

```text
reimer run hello.reim
```

## Bindings and inference

Bindings are immutable unless declared with `let mut`. Types are inferred from the initializer when no annotation is present.

```reimer
let lives = 3;
let title: str = "Orbit Runner";
let mut score: u64 = 0;
score += 250;
```

Shadowing creates a new binding. Assignment updates an existing mutable place.

## Functions are expressions at the boundary

The final expression of a block is its value when it has no semicolon.

```reimer
fn clamp_score(value: i32) -> i32 {
    if value < 0 {
        0
    } else if value > 100 {
        100
    } else {
        value
    }
}
```

Use `return` for an early exit. A function with no explicit return type returns `()`.

## Structs, methods, and enums

```reimer
struct Player {
    name: str,
    score: u32,
}

impl Player {
    fn add_score(&mut self, amount: u32) {
        self.score += amount;
    }
}

enum LoadState {
    Pending,
    Ready(Player),
    Failed { code: i32 },
}
```

`match` must cover every possible enum variant:

```reimer
fn score_or_zero(state: LoadState) -> u32 {
    match state {
        LoadState::Pending => 0,
        LoadState::Ready(player) => player.score,
        LoadState::Failed { code: _ } => 0,
    }
}
```

## Recoverable failure

Use `Option<T>` when a value may be absent and `Result<T, E>` when failure has a reason. `?` propagates the missing or error case from the current function.

```reimer
fn first(values: &[i32]) -> Option<&i32> {
    values.get(0)
}

fn require_first(values: &[i32]) -> Option<i32> {
    let value = first(values)?;
    Some(*value)
}
```

Indexing with `values[index]` is checked and panics when the index is invalid. `get` is the recoverable alternative.

## Explicit ownership and cleanup

Non-`Copy` values move implicitly. Allocator-backed owners expose `deinit`, normally paired with `defer` immediately after construction.

```reimer
from std::alloc import AllocError, general_allocator;
from std::string import String;

fn make_label() -> Result<i32, AllocError> {
    let allocator = general_allocator();
    let mut label = String::from(&allocator, "frame")?;
    defer label.deinit();

    label.push_str("-001")?;
    if label.matches("frame-001") { Ok(42) } else { Ok(0) }
}
```

The compiler and linter track obvious use-after-move and missing-cleanup cases. There is no garbage collector and no hidden allocator in owned standard-library collections.

## Modules use `::`

```reimer
from game::math import length;
import game::types as types;

let actor: types::Actor = types::Actor::new();
let distance = game::math::length(actor.position);
```

`.` is reserved for fields and methods. `::` identifies packages, modules, types, variants, and associated functions.

## Native interop is isolated

Raw pointers and direct C calls require `unsafe`, but safe wrappers can keep the rest of an application safe.

```reimer
@link("SDL3")
extern "C" {
    fn SDL_Init(flags: u32) -> bool;
}

fn initialize_video() -> bool {
    unsafe { SDL_Init(0x0000_0020) }
}
```

Standard-library public APIs contain their own private runtime boundaries, so ordinary file, time, process, allocation, and concurrency code does not require `unsafe`.

## Where to go next

- [Types and values](types-and-values.md) lists the complete type surface.
- [Ownership and errors](ownership-and-errors.md) explains moves, borrows, `defer`, and allocators.
- [Text and formatting](text-and-formatting.md) covers Unicode and `f"..."` interpolation.
- [C interoperability](../interoperability/c.md) covers ABI-safe aliases and wrappers.
