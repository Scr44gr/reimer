# Your first project

## Create the package

```text
reimer new clock-demo
cd clock-demo
```

The generated layout is intentionally conventional:

```text
clock-demo/
├── reimer.toml
├── src/
│   └── main.reim
├── tests/
├── examples/
└── assets/
```

Replace `src/main.reim` with:

```reimer
from std::io import eprintln, println;
from std::time import Duration, Instant, sleep;

fn main() -> i32 {
    let started = Instant::now();
    sleep(Duration::from_milliseconds(20));
    let elapsed = started.elapsed();

    if elapsed.as_milliseconds() >= 20 {
        match println("timer completed") {
            Ok(_) => 0,
            Err(_) => 2,
        }
    } else {
        match eprintln("the timer returned too early") {
            Ok(_) => 1,
            Err(_) => 3,
        }
    }
}
```

## Check, run, and build

```text
reimer fmt .
reimer check .
reimer run .
reimer build . --release --locked
```

- `fmt` formats every `.reim` file under `src`, `tests`, and `examples`.
- `check` performs package loading, name resolution, type checking, and lint-relevant semantic analysis without code generation.
- `run` executes the package through the JIT.
- `build --release` emits optimized machine code and links a native executable.
- `--locked` refuses to change `reimer.lock`, making CI and release builds reproducible.

The executable is written under `target/reimer/release/clock-demo` with the platform's executable suffix.

## Add a test

Create `tests/timing.reim`:

```reimer
from std::time import Duration;

fn main() -> i32 {
    if Duration::from_nanoseconds(0).is_zero() { 0 } else { 1 }
}
```

Each file under `tests/` is an independent integration test; returning `0` means success. Run the package tests with:

```text
reimer test . --locked
```

## Add a dependency

Path dependencies are useful while several packages are developed together:

```text
reimer add geometry --path ../geometry
```

Then import its public API with the dependency alias:

```reimer
from geometry::vector import Vec3;

let up = Vec3::new(0.0, 1.0, 0.0);
```

Read [modules and packages](../guide/modules-and-packages.md) for module layouts and the [package reference](../guide/package-system.md) for manifests, Git dependencies, SemVer, and lockfiles.
