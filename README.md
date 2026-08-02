<p align="center">
  <img src="docs/assets/reimer-mascot.jpg" width="320" alt="The Reimer language mascot, drawn by Reimer">
</p>

<h1 align="center">Reimer</h1>

<p align="center">
  A native programming language for games, engines, and content tools.
</p>

<p align="center">
  <a href="https://github.com/Scr44gr/reimer/actions/workflows/ci.yml"><img src="https://github.com/Scr44gr/reimer/actions/workflows/ci.yml/badge.svg" alt="CI status"></a>
  <a href="https://scr44gr.github.io/reimer/"><img src="https://github.com/Scr44gr/reimer/actions/workflows/docs.yml/badge.svg" alt="Documentation status"></a>
</p>

Reimer is an experimental compiled language with static types, explicit
ownership, allocator-aware APIs, native interoperability, and tooling designed
to support complete applications. The compiler is written in Rust and lowers
typed HIR directly to native machine code through Cranelift.

The current v0.1 surface includes static types, implicit moves, references, explicit allocators, recoverable errors, modules with `::` paths, packages and lockfiles, C FFI, traits and generics, compile-time evaluation, Unicode text, collections, filesystem and process APIs, tensors, concurrency, a linter/LSP, and standalone native executable linking.

```reimer
from std::io import println;

fn main() -> i32 {
    match println("Hello from Reimer") {
        Ok(_) => 0,
        Err(_) => 1,
    }
}
```

## Dedication

Reimer carries the name of a friend of mine named Reimer. He was an incredible
and talented person, and the best friend I could have had during that part of
my life. He drew the bird that became the language's mascot. When he sent the
drawing to me, he wrote: *"mira el pupu q hice"* -- *"look at the pupu I
made."*

This language keeps his name, his drawing, and that small moment alive.

> **"Mientras existas aquí jamas seras olvidado"**
>
> *"As long as you exist here, you will never be forgotten."*

## Start here

The complete, searchable guide is published at [scr44gr.github.io/reimer](https://scr44gr.github.io/reimer/).

- [Installation](docs/getting-started/installation.md)
- [Your first project](docs/getting-started/first-project.md)
- [Language tour](docs/guide/tour.md)
- [Standard library](docs/standard-library/overview.md)
- [Command-line reference](docs/tools/cli-reference.md)
- [Implementation status](docs/internals/implementation-status.md)

## Build from source

The Rust version and components are pinned in `rust-toolchain.toml`.

```text
cargo install --path crates/reimer-cli --locked --force
reimer --version
reimer check examples/exit_42.reim
reimer run examples/exit_42.reim --release
reimer build examples/exit_42.reim --release -o answer
```

Downloaded release archives keep `reimer`, `reimer-lsp`, `reimer-lint`, and `std` together. Source builds can use the checkout's standard library; `REIMER_STD_PATH` explicitly selects another standard-library directory when needed.

## VS Code

`editors/vscode` contains the original TextMate grammar and compiler-backed language client. It provides canonical inferred types, Markdown hovers, completion, diagnostics, rename, import organization, quick fixes, antipattern detection, and static allocator estimates.

See the [editor setup guide](docs/getting-started/editor.md).

## Development

```text
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -D warnings
```

Build the documentation locally with:

```text
pwsh ./scripts/docs/check-links.ps1
mdbook serve --open
```

See [Contributing](docs/contributing.md) for the repository map and the required vertical compiler workflow.

## License

Reimer is available under the [MIT License](LICENSE).
