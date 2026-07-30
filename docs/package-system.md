# Package and build system

M8 introduces declarative projects without arbitrary build scripts. The
compiler resolves the complete graph before loading modules, but each package
may only import its direct dependencies.

## Layout

```text
game/
├── reimer.toml
├── reimer.lock
├── src/
│   ├── main.reim
│   └── world.reim
├── tests/
│   └── loading.reim
├── examples/
└── assets/
```

`src/main.reim` defines an executable. A library package uses
`src/package.reim` as its public facade and does not need `main`. Dependencies
must always provide that facade.

## Manifest

```toml
[package]
name = "game"
version = "0.1.0"
edition = "2026"

[dependencies]
physics = { path = "../physics", version = "^0.1" }
assets = { git = "https://example.com/assets.git", tag = "0.4.0" }

[profile.debug]
optimization = 0

[profile.release]
optimization = 3
```

Versions and requirements follow SemVer. A dependency may rename a package
with `package = "real-name"`. Supported sources are:

- `path`, relative to the manifest declaring the dependency;
- `git`, pinned to a commit and optionally selected with `rev`, `branch`, or
  `tag`.

Only one Git selector may appear. Registry dependencies are recognized so the
compiler can produce a precise error, but registry support and publishing are
reserved for a later milestone.

## Resolution and lockfile

`reimer.lock` contains exact identities, versions, sources, checksums, and
edges. Paths are stored relative to the root, so moving the complete tree
produces the same lockfile. Git commits remain pinned until an update is
requested.

- normal mode reuses pinned commits and updates stale checksums;
- `--locked` rejects a missing lockfile or any drift;
- `--refresh` resolves Git references again and replaces the lockfile.

Versions are unified when name, version, and source identify the same package.
Cycles are rejected with the complete chain. A transitive dependency is not
visible from the root package unless it is also declared directly.

## Commands

```text
reimer new <path>
reimer init [path]
reimer check [path] [--locked|--refresh]
reimer build [path] [--release] [--locked|--refresh]
reimer run [path] [--release] [--locked|--refresh]
reimer test [path] [--release] [--locked|--refresh]
reimer doc [path] [--locked|--refresh] [-o <file.md>]
reimer fmt [path] [--check]
reimer clean [path]
reimer add <alias> (--path <path>|--git <url>)
reimer remove <alias>
```

`build` emits an object under `target/reimer/debug` or
`target/reimer/release`. Manifest levels map to Cranelift strategies: `0`
disables optimization, `1-2` optimize for speed, and `3` balances speed and
size. `run` executes the same graph through JIT.

Each `.reim` file under `tests/` is an independent integration test. It must
define `fn main() -> i32`; returning `0` means success. Selection and execution
are sorted by path for deterministic results.

`doc` runs semantic analysis before generating Markdown. It includes public
functions, extern functions, constants, structs, enums, traits, and public
inherent methods from the root package, preserves Markdown written in `///`
comments, and excludes private items and dependency implementation details.
The default path is `target/reimer/doc/<package>.md`.

`fmt` normalizes trailing spaces, the final newline, and import order;
`--check` only verifies. `clean` removes only `target/reimer` after resolving
and validating the project root.

## Deliberate restriction

The manifest does not execute code, plugins, or Turing-complete scripts. Future
generation must be expressed through declarative inputs and outputs so the
graph remains reproducible, inspectable by the LSP, and safe for tooling.
