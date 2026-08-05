# Command-line reference

The `reimer` executable handles single `.reim` files and declarative projects. A project path may point to its root directory or `reimer.toml`.

## General information

```text
reimer --help
reimer --version
```

The process exits with `0` when the requested compiler operation succeeds and a nonzero status for invalid arguments, diagnostics, package failures, or backend failures. Programs launched through `reimer run` report their returned value; standalone executables use that value as their native exit code.

## Create projects

```text
reimer new <path>
reimer init [path]
```

`new` requires a destination that does not exist. `init` creates missing project files in an existing or new directory. Both create `src`, `tests`, `examples`, and `assets` plus a strict manifest.

## Analyze source

```text
reimer check [path] [--locked|--refresh]
```

Runs lexing, parsing, package loading, manifest-native path validation, name resolution, type checking, borrow and move analysis, and semantic validation without machine-code generation.

For CI, use:

```text
reimer check . --locked
```

## Run a program

```text
reimer run [path] [--release] [--locked|--refresh] [-- <arguments>...]
```

Execution uses the Cranelift JIT. Arguments after `--` belong to the Reimer program rather than the compiler:

```text
reimer run . --release --locked -- --level arena-01
```

The program reads them through `std::env::args()`. Native libraries declared by
the package graph are resolved without requiring global `PATH`,
`LD_LIBRARY_PATH`, or `DYLD_LIBRARY_PATH` changes.

## Build a native executable

```text
reimer build [path] [--release] [--locked|--refresh] [-o <executable>]
```

For executable packages and source files, `build` emits a native object and links it with the matching startup and runtime. Manifest-declared `.dll`, `.so`, or `.dylib` files are copied beside the executable. The resulting program does not need the Reimer compiler or a separate Reimer runtime when launched, although it still needs those explicitly declared third-party shared libraries.

```text
reimer build . --release --locked -o game.exe
```

Library packages currently emit a native object. A stable library archive format is not part of the experimental release.

## Run tests

```text
reimer test [path] [--release] [--locked|--refresh]
```

Each `.reim` file under `tests/` is an independent integration test with `fn main() -> i32`; returning `0` means success. Functions marked `@test` are also registered and run in isolated JIT executions. Test ordering is deterministic.

## Format source

```text
reimer fmt [path]
reimer fmt [path] --check
```

Formatting normalizes source layout and import order. `--check` reports drift without writing files and is suitable for CI.

## Generate API documentation

```text
reimer doc [path] [--locked|--refresh] [-o <file.md>]
```

Semantic analysis runs before generation. Public `///` Markdown, functions, constants, types, traits, variants, fields, and methods from the root package are written to Markdown. The default destination is `target/reimer/doc/<package>.md`.

## Manage dependencies

```text
reimer add <alias> --path <path> [--package <name>] [--version <requirement>]
reimer add <alias> --git <url> [--package <name>] [--version <requirement>]
reimer remove <alias> [--project <path>]
```

The command updates `reimer.toml`, validates the complete graph, refreshes the lockfile, and restores the original manifest if validation fails.

## Clean compiler artifacts

```text
reimer clean [path]
```

Only the validated project's `target/reimer` directory is removed. Cargo artifacts and unrelated directories are not touched.

## Emit a raw object

```text
reimer emit-object <file.reim> [-o <file.obj>]
```

This low-level command writes a target-native object without linking an executable. Most application users should prefer `build`.

## Lock modes and profiles

`--locked`
: Refuses missing or stale lockfiles and never changes dependency resolution.

`--refresh`
: Re-resolves Git selectors and updates the lockfile intentionally.

`--release`
: Uses the manifest's release optimization level. Without it, commands use the debug profile.
