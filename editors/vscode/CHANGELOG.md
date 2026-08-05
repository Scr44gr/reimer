# Changelog

## 0.1.2

- Adds specialized generic function values and explicit turbofish calls for
  statically typed engine callbacks.
- Adds alignment-aware owned storage, arena and fixed-buffer lifecycle APIs,
  and allocator-correct collection growth.
- Adds `Result.expect(message)` for invariant failures without requiring error
  formatting or hidden allocation.
- Resolves native libraries and staged runtime files from package manifests
  across supported hosts.
- Improves compiler-backed hover, completion, ownership diagnostics, and
  static allocator estimates for the new language surface.

## 0.1.1

- Ships the extension with the Reimer 0.1.1 compiler and standard library.
- Recognizes variadic type packs, typed registry queries, disjoint field
  borrows, and consuming tuple-field access through the compiler-backed LSP.
- Includes the latest formatter, ownership diagnostics, and vendor API
  documentation.

## 0.1.0

- Syntax highlighting and language configuration for `.reim`.
- LSP client, diagnostics, hover, inferred types, symbols, completion, and
  local go-to-definition.
- Safe import organization, quick fixes, antipattern lints, and static
  allocator estimates.
- Self-contained client bundle so installed VSIX packages start the LSP
  without external Node dependencies.
- Unsaved package-aware type inference and explicit bracket-pair editor
  defaults.
- Source-level type names, type-specific hover documentation, and `///`
  documentation on declarations, calls, imports, and completion items.
- Bracket matching without intrusive active-block guide lines.
- Compiler-linked intradocument rename and dependency-aware incremental
  analysis across all unsaved open modules.
- Typed `Display` and `Debug` interpolation, including `:?` highlighting.
- Ownership diagnostics recognize methods whose `self` receiver consumes and
  transfers the original value.
- Completion and hover documentation for `std::time`, including `Duration`,
  `Instant`, monotonic measurement, and blocking sleep.
