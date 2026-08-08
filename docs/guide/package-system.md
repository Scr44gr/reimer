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

[native.windows-x86_64]
library-paths = ["native/windows-x86_64"]
link-libraries = ["physics_native", "physics_bridge"]
runtime-files = [
    "native/windows-x86_64/physics_native.dll",
    "native/windows-x86_64/physics_bridge.dll",
]

[profile.debug]
optimization = 0

[profile.release]
optimization = 3
windows-subsystem = "windows"
```

Versions and requirements follow SemVer. A dependency may rename a package
with `package = "real-name"`. Supported sources are:

- `path`, relative to the manifest declaring the dependency;
- `git`, pinned to a commit and optionally selected with `rev`, `branch`, or
  `tag`.

Only one Git selector may appear. Registry dependencies are recognized so the
compiler can produce a precise error, but registry support and publishing are
reserved for a later milestone.

## Native runtime dependencies

Packages that expose `@link` functions can describe their native artifacts in
the manifest. The supported target names are `windows-x86_64`,
`windows-aarch64`, `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, and
`macos-aarch64`.

- `library-paths` contains directories searched before system library paths;
- `link-libraries` contains the names used by the native linker and is ordered
  dependency-first when one shared library depends on another;
- `runtime-files` contains `.dll`, `.so`, or `.dylib` files copied beside a
  built executable.

Every path is relative to the manifest that declares it. Absolute paths,
parent traversal, symbolic links, missing files, and target/extension
mismatches are rejected during project resolution. Native declarations from
direct and transitive dependencies are combined for the current host. Two
packages cannot stage different runtime files under the same file name.

`reimer run` and `reimer test` load declared libraries by their resolved paths
without modifying the process-wide `PATH`, `LD_LIBRARY_PATH`, or
`DYLD_LIBRARY_PATH`. `reimer build` passes the same search paths to the linker
and stages the declared runtime files beside the executable. Native inputs are
included in the package checksum, so `--locked` detects binary drift as well as
source drift.

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
`target/reimer/release`. Required native runtime files are placed beside the
generated executable. Manifest levels map to Cranelift strategies: `0`
disables optimization, `1-2` optimize for speed, and `3` balances speed and
size. `run` executes the same graph through JIT.

Each `.reim` file under `tests/` is an independent integration test. It must
define `fn main() -> i32`; returning `0` means success. Selection and execution
are sorted by path for deterministic results.

`doc` runs semantic analysis before generating Markdown. It includes public
functions, extern functions, constants, structs, enums, type aliases, traits,
and public inherent methods from the root package, preserves Markdown written
in `///` comments, and excludes private items and dependency implementation
details.
The default path is `target/reimer/doc/<package>.md`.

`fmt` normalizes trailing spaces, the final newline, and import order;
`--check` only verifies. `clean` removes only `target/reimer` after resolving
and validating the project root.

## Build profiles and Windows applications

Each profile accepts `optimization` and an optional `windows-subsystem`:

```toml
[profile.debug]
optimization = 0
windows-subsystem = "console"

[profile.release]
optimization = 3
windows-subsystem = "windows"
```

- `console` is the default and preserves a terminal plus standard input,
  output, and error streams.
- `windows` produces a graphical Windows executable without opening a console
  window. It has no effect on Linux or macOS.

The setting is profile-specific because optimization does not imply an
application type. Command-line tools should keep `console` in release builds;
games can select `windows` only for release while retaining the debug console.

## Deliberate restriction

The manifest does not execute code, plugins, or Turing-complete scripts. Future
generation must be expressed through declarative inputs and outputs so the
graph remains reproducible, inspectable by the LSP, and safe for tooling.
