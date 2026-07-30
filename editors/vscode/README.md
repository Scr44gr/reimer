# Reimer Language Support for VS Code

Editor support backed by the repository's real grammar and compiler. The
extension does not copy Rust grammars or Pylance components; it provides a
similar quality of experience through an original TextMate grammar and LSP.

## Capabilities

- `.reim` recognition and highlighting for nested comments, UTF-8/C literals,
  declarations, generics, attributes, `::` paths, operators, and control flow;
- diagnostics from the lexer, parser, manifest/lockfile, package loader, and
  resolver;
- hover and inlay hints with canonical source-level types, useful primitive and
  standard-library descriptions, and no internal compiler symbol names;
- Markdown documentation from `///` comments on declarations, direct calls,
  imported functions, and local completion items;
- local go-to-definition, document symbols, and completion;
- quick fixes for close typos and antipatterns;
- `source.organizeImports` to place `std` first and normalize selective imports
  without deleting comments;
- explicitly labeled static allocator reservation estimates through inlay hints
  and CodeLens;
- path/Git dependency resolution from `reimer.toml`, with reanalysis when a
  manifest, lockfile, or `.reim` source changes;
- highlighting and completion for `comptime`, constants, reflection, and M10
  attributes; `*` and `->` share the same operator scope;
- HIR-backed `@must_use` warnings, including saved multi-file packages;
- package-aware inference for the active in-memory document, including unsaved
  changes in files with imports;
- reliable matching and colorization for `{}`, `[]`, and `()` without adding
  active-block guide lines. Angle brackets remain operators so `->` and
  comparisons are not mistaken for bracket pairs.

## Development installation

From the repository root:

```powershell
cargo build --release -p reimer-lsp
cd editors\vscode
npm install
npm run compile
```

In VS Code, run **Developer: Install Extension from Location...** and choose
`editors/vscode`. During development, configure `reimer.server.path` with the
absolute path to `target/release/reimer-lsp.exe`.

To generate an installable package:

```powershell
npm run package
```

The command compiles and bundles the server and produces a self-contained
`win32-x64` VSIX. That installation does not require a server path. On other
platforms, compile the server locally and set `reimer.server.path`.

## Memory-estimate precision

Displayed figures are static estimates, not memory profiles. Constant sizes in
`allocate_bytes`, bounded input, or `String::from` can be calculated exactly;
runtime values are reported as dynamic. Reservations inside loops are shown
per iteration. The extension does not claim a total peak when control flow or
lifetime cannot be proved.

## Documentation comments

Place consecutive `///` comments immediately before a declaration. Markdown is
preserved in hover and completion details:

```reimer
/// Writes a greeting to standard output.
///
/// Returns `IoError` if the complete message cannot be written.
fn greet() -> Result<(), IoError> {
    println("hello")
}
```

## Current limitations

Imported dependency files are read from disk while the active document uses its
latest in-memory snapshot. Changes in another open module become visible after
that module is saved. Go-to-definition is currently intradocument.
