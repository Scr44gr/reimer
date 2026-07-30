# Arquitectura inicial de Reimer

## Pipeline implementado

1. `reimer-lexer` transforma UTF-8 en tokens y conserva spans de bytes.
2. `reimer-parser` construye un AST fiel a la sintaxis y recupera errores en
   límites de statements y declaraciones.
3. `reimer-resolver` resuelve funciones, bindings y nombres de tipos; comprueba
   tipos, mutabilidad, aridad, ciclos de almacenamiento y control de flujo.
4. `reimer-hir` representa el programa tipado mediante IDs de funciones,
   locales y tipos compuestos; el backend no depende del AST.
5. `reimer-layout` calcula una única representación nativa compartida por
   reflexión y codegen; `reimer-codegen-native` baja la HIR a Cranelift, con
   overflow y división comprobados.
6. `reimer-project` valida `reimer.toml`, resuelve dependencias path/git,
   sincroniza `reimer.lock` y construye un grafo portable.
7. `reimer-cli` expone el ciclo completo
   `new/init/check/build/run/test/fmt/clean/add/remove` y conserva
   `emit-object` para archivos sueltos.
8. `reimer-diagnostics` renderiza errores con código, ubicación, fragmento de
   fuente y ayuda opcional.
9. `reimer-package` descubre módulos, resuelve imports `::`, limita la
   visibilidad a dependencias directas y reescribe nombres
   canónicos antes del type checker.
10. `reimer-lint` deriva diagnósticos editoriales, inferencia visible,
   antipatrones y estimaciones de allocator desde el frontend real.
11. `reimer-lsp` sirve esas capacidades a VS Code mediante LSP y vuelve a
    analizar al cambiar manifest o lockfile; la gramática
    TextMate queda limitada al resaltado lexical.
12. `std::tensor` construye tensors propietarios sobre `Vec<T>` y expone vistas
    scoped; el resolver propaga el préstamo de agregados que contienen
    referencias y el backend copia agregados con semántica de valor real.

## Decisiones de alcance

- M1 reconoce funciones con parámetros `i32`/`bool`/`()`, retorno explícito u
  omitido, bloques con valor, `let`, `let mut`, shadowing, asignación simple y
  compuesta, operadores aritméticos/comparativos/lógicos, llamadas directas,
  `if`, `while`, `break`, `continue` y `return`.
- Los imports, reexports, paths absolutos, `self::` y `super::` se resuelven
  estáticamente en paquetes multarchivo.
- El único programa enlazable por ahora es `fn main() -> i32`.
- Cranelift es un backend reemplazable. El AST no contiene detalles del backend.
- `run` usa JIT y `build` produce un objeto nativo. El enlazado autónomo se
  añadirá junto al runtime de arranque de cada plataforma.
- El runtime encapsula los límites de FFI, allocator y E/S; los programas usan
  wrappers seguros de la biblioteca estándar.

Las decisiones sintácticas y semánticas congeladas se encuentran en
[`language-decisions-v0.1.md`](language-decisions-v0.1.md).

## M7 Tensor

`tensor<T, Rank>` usa almacenamiento contiguo row-major, conserva
`shape: [usize; Rank]` y `strides: [usize; Rank]`, y no oculta allocations.
`TensorView` y `TensorViewMut` almacenan slices scoped: el type checker impide
que sobrevivan al tensor propietario o queden ocultos dentro de storage raw.
La sintaxis `value[i, j]` se baja al protocolo comprobado de indexación del
tipo, por lo que los fallos terminan en `panic` y nunca en acceso fuera de
bounds. Los kernels que escriben resultados reciben un output explícito.

## M8 Paquetes

El sistema declarativo está descrito en
[`package-system.md`](package-system.md). Las identidades de path son relativas
a la raíz para que el lockfile sobreviva a una reubicación. Git queda fijado a
un commit; `--locked` impide cualquier deriva. Un root con `src/package.reim`
se resuelve como biblioteca sin inventar un `main`.

## M9 Concurrencia

El runtime mantiene threads nativos, workers persistentes, colas locales y
work stealing. `Send` y `Sync` se derivan estructuralmente; los punteros raw no
implementan ninguno. `std::thread` encapsula locks, atomics, channels,
barriers, semaphores y estado local por thread. `std::job` ofrece un pool fijo,
jobs tipados y `parallel_for_mut` para slices, arrays y tensors.

Cada ejecución JIT posee una sesión de recursos. La limpieza espera únicamente
los threads y pools de esa sesión antes de liberar el código generado, por lo
que dos compilaciones ejecutadas en paralelo no interfieren. Las fronteras ABI
privadas conservan bloques `unsafe` documentados; el programa usuario solo ve
wrappers seguros.

La API y sus invariantes se describen en
[`concurrency.md`](concurrency.md).

## M10 Comptime

El resolver evalúa constantes, funciones y bloques `comptime` sin delegar
operaciones al host. Impone presupuestos de pasos, profundidad y memoria, y
rechaza E/S, FFI, threads, punteros, préstamos y llamadas runtime. El mismo
layout validado que consume Cranelift alimenta `size_of<T>()` y `align_of<T>()`,
evitando dos calculadores con resultados distintos.

Los atributos se conservan en AST/HIR y se validan por destino. Los derives son
una lista cerrada y estructural; `Clone` no puede ocultar un allocator. `@test`
registra funciones unitarias y la CLI ejecuta cada una en un proceso JIT
aislado. `@must_use` se publica como diagnóstico del linter/LSP.

La sintaxis, los límites y las garantías se describen en
[`metaprogramming.md`](metaprogramming.md).
