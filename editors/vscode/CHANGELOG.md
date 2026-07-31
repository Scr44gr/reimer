# Changelog

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
