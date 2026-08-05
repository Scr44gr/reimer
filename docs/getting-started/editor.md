# VS Code setup

The Reimer extension is backed by the real parser, resolver, and package graph. It does not infer a second approximation of the language in TypeScript.

## Install a release

Download the `.vsix` attached to the matching GitHub Release, then run:

```text
code --install-extension reimer-language-win32-x64-0.1.1.vsix --force
```

You can also use **Extensions: Install from VSIX...** from the VS Code command palette. Reload VS Code after replacing an existing development build.

The Windows VSIX contains `reimer-lsp.exe` and the matching `std` sources. Other platforms can point the extension at the `reimer-lsp` binary included in their release archive:

```json
{
  "reimer.server.path": "/absolute/path/to/reimer-lsp"
}
```

## What the extension provides

- syntax highlighting and reliable bracket matching for `.reim` files;
- compiler diagnostics and typo-aware quick fixes;
- canonical inferred types in hover and inlay hints;
- Markdown from `///` comments on declarations, calls, and completion items;
- completion for local, imported, generic, and standard-library symbols;
- go to definition, document symbols, and compiler-linked rename;
- import organization using `std`-first and `::` path rules;
- allocator reservation estimates labeled as static estimates;
- antipattern and ownership-cleanup diagnostics.

## Recommended settings

The extension enables bracket-pair colorization, inlay hints, and matching-bracket navigation for Reimer by default. To reduce visual noise, it does not add active-block guide lines.

Use these commands from the command palette:

- **Reimer: Organize Imports**
- **Reimer: Restart Language Server**
- **Reimer: Show Allocator Estimate**

## Documentation hovers

Document a declaration with consecutive Markdown `///` comments:

```reimer
/// Computes the squared length without taking a square root.
///
/// This is useful for distance comparisons in hot loops.
fn length_squared(x: f32, y: f32) -> f32 {
    x * x + y * y
}
```

Hovering either the declaration or a direct call shows the resolved signature and this documentation. Generic nominal types remain source-level, for example `TensorViewMut<f32, 2>`, instead of exposing internal compiler symbols.

See [language tooling](../tools/editor-tooling.md) for linter codes, import rules, allocator-estimate limits, and development packaging.
