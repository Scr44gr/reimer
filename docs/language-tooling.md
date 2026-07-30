# Tooling de lenguaje y VS Code

El tooling usa una sola fuente de verdad: `reimer-lint` consume las APIs
públicas del lexer, parser y resolver, y `reimer-lsp` transforma ese análisis a
LSP. La gramática TextMate se mantiene conforme a los tokens reales, pero solo
se ocupa de color; nunca decide si un programa es válido.

## Componentes

- `crates/reimer-lint`: diagnósticos del compilador, lints de antipatrones,
  quick fixes, organización de imports, índice de tipos inferidos, enlaces
  locales de definición y estimación de allocators.
- `crates/reimer-lsp`: servidor por stdin/stdout con sincronización completa,
  diagnósticos push, hover, go-to-definition intradocumento, símbolos,
  completado, code actions, inlay hints y CodeLens.
- `editors/vscode`: reconocimiento `.reim`, gramática TextMate original,
  configuración de pares/indentación, snippets y cliente LSP.

El protocolo LSP se mantiene separado de la lógica de análisis. Esto permite
probar offsets UTF-8/UTF-16, acciones y estimaciones sin levantar VS Code.

## Reglas editoriales

La acción `source.organizeImports`:

1. coloca los imports de `std` primero;
2. ordena paths con el separador canónico `::`;
3. ordena y deduplica nombres de imports selectivos;
4. no produce un edit si encuentra comentarios dentro de la sección, evitando
   perder o reasociar documentación.

Los typos se comparan con símbolos, bindings, campos, imports, tipos primitivos
y nombres core visibles en el documento. Solo se ofrece un reemplazo cuando la
distancia de edición es pequeña; la validación final sigue perteneciendo al
resolver.

Los lints propios incluyen, entre otros:

- `L1001`: imports no canónicos;
- `L2001`: `mut` que nunca recibe una asignación;
- `L2002`: comparación redundante con booleanos;
- `L2003`: `while true` en lugar de `loop`;
- `L2004`: bloque `unsafe` vacío;
- `L2010`: owner de allocation/string/input buffer sin cleanup o transferencia
  visible.

`reimer-lint` también puede ejecutarse directamente:

```text
cargo run -p reimer-lint -- examples/exit_42.reim
cargo run -p reimer-lint -- --deny-warnings examples/exit_42.reim
```

## Inferencia y allocators

Cuando el resolver produce HIR, el servidor indexa el tipo de expresiones y
bindings sin anotación. Los hovers e inlay hints muestran esa inferencia, no una
heurística paralela.

Las cifras de memoria siempre dicen **static estimate**. Actualmente se
reconocen reservas explícitas de bytes, buffers de entrada acotados, fixed
buffers, `String::from`, `clone_in` dinámico y operaciones de capacidad con
allocator. Se evalúa aritmética constante checked y se distinguen:

- bytes exactos por llamada;
- límite máximo;
- bytes por iteración cuando la operación está dentro de un loop;
- tamaño dinámico cuando depende del runtime.

La suma no pretende representar el pico de memoria: no inventa conteos de
iteraciones, exclusión entre ramas ni lifetimes que el análisis no ha probado.

## Paquetes y snapshots

Un archivo sin imports se resuelve directamente desde el buffer abierto. Para
paquetes multiarchivo guardados, el servidor usa el package loader y vuelve a
resolver el programa canónico. Mientras hay cambios intermodulares sin guardar,
se mantienen lexer/parser/lints locales y la semántica intermodular vuelve a
actualizarse al guardar.

## Instalación y verificación

El VSIX autocontenido para Windows se genera con:

```text
cd editors/vscode
npm install
npm test
npm run package
```

El paquete `reimer-language-win32-x64-0.1.0.vsix` incluye
`extension/server/reimer-lsp.exe`. `scripts/test-grammar.mjs` carga Oniguruma y
tokeniza casos reales, incluyendo la diferencia contextual entre:

```reimer
from std::string import String;
let text = String::from(&allocator, "dato")?;
fn from(value: str) -> str { value }
```

Los gates Rust dirigidos son:

```text
cargo test -p reimer-lint -p reimer-lsp --locked
cargo clippy -p reimer-lint -p reimer-lsp --all-targets --all-features --locked -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -D warnings
```
