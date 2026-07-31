# Contributing

Reimer is organized so the compiler, language tools, standard library, documentation, and distribution logic remain independently understandable.

## Repository map

```text
reimer/
├── .github/workflows/      # CI, Pages, and tag releases
├── crates/                 # Rust compiler, runtime, linter, and LSP crates
├── docs/
│   ├── getting-started/    # First-use documentation
│   ├── guide/              # Language guide
│   ├── standard-library/   # Safe source APIs and ownership contracts
│   ├── interoperability/   # C, SDL3, and native integrations
│   ├── tools/              # CLI, editor, and release workflows
│   ├── internals/          # Architecture, decisions, and coverage
│   └── theme/              # mdBook visual identity and highlighting
├── editors/vscode/         # VS Code client, grammar, snippets, and packaging
├── examples/               # Executable positive and negative language cases
├── scripts/
│   ├── demos/              # Reproducible native demos
│   ├── docs/               # Documentation validation
│   └── release/            # Version validation and native packaging
└── std/                    # Reimer standard-library source
```

## Rust quality gates

```text
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -D warnings
```

Use `Result` for fallible production paths, keep ownership visible, and do not silence Clippy without a narrow documented reason. Public Rust APIs should explain behavior and failure conditions through doc comments.

## Language changes

A language change normally touches more than one crate. Keep the vertical path complete:

1. tokens and spans in the lexer;
2. AST construction and parser recovery;
3. package and name resolution;
4. semantic typing and diagnostics;
5. typed HIR;
6. native layout and Cranelift lowering;
7. positive and negative tests;
8. formatter, linter, LSP, grammar, and documentation when observable.

Never document a designed feature as implemented until an executable `.reim` example and the permanent gates prove it.

## Documentation

Install mdBook 0.5.4 and run:

```text
pwsh ./scripts/docs/check-links.ps1
mdbook serve --open
```

The site uses a text wordmark while the permanent logo is being designed. A future logo can replace the `.menu-title::before` rule in `docs/theme/reimer.css` without changing chapter content or navigation.

Use `reimer` fences for complete language snippets. Prefer examples already covered by files under `examples/`, and state explicitly when a snippet is abbreviated.

## Commit structure

Use conventional commits with one coherent concern per commit, for example:

```text
feat(language): add recoverable slice access
fix(editor): preserve source-level generic names
docs(guide): explain allocator ownership
ci(release): package native tag artifacts
```

Do not commit generated `target`, mdBook output, VSIX files, downloaded SDL archives, or editor dependencies.
