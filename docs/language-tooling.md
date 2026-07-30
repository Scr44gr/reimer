# Language tooling and VS Code

The tooling has one source of truth: `reimer-lint` consumes the public lexer,
parser, and resolver APIs, while `reimer-lsp` translates that analysis to LSP.
The TextMate grammar follows real tokens but only controls color; it never
decides whether a program is valid.

## Components

- `crates/reimer-lint`: compiler diagnostics, antipattern lints, quick fixes,
  import organization, resolved-type indexes, local definition links, and
  allocator estimates.
- `crates/reimer-lsp`: stdin/stdout server with full synchronization, pushed
  diagnostics, hover, intradocument go-to-definition, symbols, completion,
  code actions, inlay hints, and CodeLens.
- `editors/vscode`: `.reim` recognition, original TextMate grammar,
  pair/indentation configuration, snippets, and the LSP client.

The LSP protocol remains separate from analysis logic. UTF-8/UTF-16 offsets,
actions, and estimates can therefore be tested without launching VS Code.

## Editor rules

The `source.organizeImports` action:

1. places `std` imports first;
2. sorts paths with the canonical `::` separator;
3. sorts and deduplicates names in selective imports;
4. declines to edit when comments occur inside the section, avoiding lost or
   reassociated documentation.

Typo detection compares symbols, bindings, fields, imports, primitive types,
and core names visible in the document. A replacement is offered only for a
small edit distance; final validation still belongs to the resolver.

Built-in integer methods are completed and carry semantic hover information.
For example, hovering `checked_add` shows its exact integer signature, explains
the overflow behavior, and the result binding is displayed as
`Option<integer>` rather than an internal enum name.

Language-specific lints include:

- `L1001`: noncanonical imports;
- `L2001`: `mut` that never receives an assignment;
- `L2002`: redundant comparison with a boolean;
- `L2003`: `while true` instead of `loop`;
- `L2004`: empty `unsafe` block;
- `L2010`: allocation/string/input-buffer owner without visible cleanup or
  transfer;
- `L2020`: discarded `@must_use` function or value.

`reimer-lint` can also run directly:

```text
cargo run -p reimer-lint -- examples/exit_42.reim
cargo run -p reimer-lint -- --deny-warnings examples/exit_42.reim
```

## Type information and allocators

When the resolver produces HIR, the server indexes expression types and
unannotated bindings. Hover and inlay hints display canonical source-level
types from the compiler, not a parallel heuristic. Internal package symbols
are never part of the editor presentation.

Memory figures always say **static estimate**. The analyzer currently
recognizes explicit byte reservations, bounded input buffers, fixed buffers,
`String::from`, dynamic `clone_in`, and allocator-backed capacity operations.
It evaluates checked constant arithmetic and distinguishes:

- exact bytes per call;
- maximum bounds;
- bytes per iteration when an operation is inside a loop;
- dynamic size when it depends on runtime data.

The sum does not claim to be peak memory: it does not invent iteration counts,
branch exclusion, or lifetimes that analysis has not proved.

## Documentation comments

Consecutive `///` comments immediately before a declaration are Markdown
documentation:

```reimer
/// Adds two signed integers.
///
/// # Arguments
/// - `left`: first value
/// - `right`: second value
fn add(left: i32, right: i32) -> i32 {
    left + right
}
```

The language server shows this text with the resolved signature when hovering
over a function declaration or any direct call. Documented structs and enums
also retain their Markdown when hovering over values of those types, including
generic instances such as `Container<i32>`. It attaches local declaration
documentation to completion items as well. Documentation from imported source
modules is retained during package resolution, and internal compiler symbol
names are never exposed.

The same comments generate package documentation:

```text
reimer doc [path]
reimer doc [path] -o public-api.md
```

The command first runs the complete semantic analysis. It then generates
Markdown for the root package's public functions, types, constants, traits,
fields, variants, and methods. Private items and dependency implementation
details are not exposed. Without `-o`, the output is written to
`target/reimer/doc/<package>.md`.

## Packages and snapshots

The active document is always resolved from its in-memory snapshot. When it has
imports, the package loader overlays that snapshot onto the package graph and
reads dependency modules from disk before rebuilding the canonical program.
Changes in another open module become visible after that module is saved.

## Installation and verification

Generate the self-contained Windows VSIX with:

```text
cd editors/vscode
npm install
npm test
npm run package
```

`reimer-language-win32-x64-0.1.0.vsix` includes
`extension/server/reimer-lsp.exe`, and the extension client is bundled into one
JavaScript entry point without external runtime dependencies.
`scripts/test-grammar.mjs` loads Oniguruma and tokenizes real cases, including
the contextual distinction between:

```reimer
from std::string import String;
let text = String::from(&allocator, "data")?;
fn from(value: str) -> str { value }
```

Targeted Rust gates are:

```text
cargo test -p reimer-lint -p reimer-lsp --locked
cargo clippy -p reimer-lint -p reimer-lsp --all-targets --all-features --locked -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -D warnings
```
