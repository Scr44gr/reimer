# Modules and packages

Reimer uses `::` for paths and `.` for fields and methods.

## A multi-file module

```text
src/
├── main.reim
└── game/
    ├── package.reim
    ├── math.reim
    └── types.reim
```

`game/package.reim` is the facade for the `game` module:

```reimer
pub from self::types import Pair;
```

`game/types.reim`:

```reimer
pub struct Pair {
    pub left: i32,
    pub right: i32,
}
```

`game/math.reim`:

```reimer
from super::types import Pair;

pub fn sum(pair: Pair) -> i32 {
    pair.left + pair.right
}
```

`main.reim`:

```reimer
from game import Pair;

fn main() -> i32 {
    let pair = Pair { left: 20, right: 22 };
    game::math::sum(pair)
}
```

## Import forms

```reimer
import game::math;
import game::types as model;
from game::types import Pair, Transform;
pub from self::types import Pair;
```

- `import` binds a module, optionally under an alias.
- `from ... import ...` selects declarations.
- `pub` reexports a declaration through the current facade.
- `self::` starts in the current module.
- `super::` starts in the parent module.
- absolute dependency and standard-library paths begin with their package name, such as `std::time::Instant`.

Imports are private unless reexported. The formatter and editor's organize-imports action place `std` first, sort `::` paths, and normalize selective names.

## Packages

A declarative `reimer.toml` identifies a package, dependencies, and debug/release profiles. `src/main.reim` creates an executable package. `src/package.reim` creates a library package.

Dependencies may come from paths or Git. The lockfile records exact source identities, checksums, and Git commits. A package may import only its direct dependencies.

```toml
[package]
name = "game"
version = "0.1.0"
edition = "2026"

[dependencies]
physics = { path = "../physics", version = "^0.1" }
assets = { git = "https://example.com/assets.git", tag = "0.4.0" }
```

Use `--locked` in CI and releases. Use `--refresh` only when intentionally resolving Git selectors and updating the lockfile.

The complete format is in the [package and build reference](package-system.md).
