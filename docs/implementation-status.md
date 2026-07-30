# Cobertura de implementación del LDD

Esta matriz usa el roadmap del LDD v0.1 y las decisiones congeladas en
[`language-decisions-v0.1.md`](language-decisions-v0.1.md). Un hito solo se
considera completo cuando tiene especificación, implementación, pruebas
positivas y negativas y un programa `.reim` ejecutable.

| Hito | Estado | Evidencia actual | Trabajo restante |
|---|---|---|---|
| M0 — Esqueleto | Completo | Workspace Rust, CLI, spans, diagnósticos, JIT y objeto nativo; `examples/exit_42.reim` | Enlazado autónomo pertenece al runtime nativo |
| M1 — Lenguaje básico | Completo | Precedencia, `let`/`let mut`, shadowing, asignación, funciones, `if`, `while`, `break`, `continue`, HIR tipada y `examples/m1_language.reim` | Los tipos y controles adicionales pertenecen a M2 |
| M2 — Tipos | Completo | Todos los escalares (`i8`…`i128`, `u8`…`u128`, tamaños de puntero, `f32`/`f64`, `bool`, `char`, `()` y `never`); structs, enums, tuplas, arrays, slices y referencias con layouts nativos; constructores, campos, índices comprobados, mutación de places, ABI de agregados; `match` exhaustivo con guards y patrones, `for` y `loop` con valor; tres programas M2 ejecutables | — |
| M3 — Errores y memoria | Completo | Referencias `&T`/`&mut T`, punteros raw limitados por `unsafe`, slices con bounds checks e iteración, `str` UTF-8 como vista `(pointer, length)`, `Option`, `Result`, `?`, movimientos implícitos, `Copy`, `defer` y `panic`; allocators general, page, arena y fixed-buffer con OOM recuperable; stdin/stdout/stderr seguros con lecturas acotadas, escritura parcial/completa, flush, terminal y conversión UTF-8 sin copia; `String` con crecimiento y `clone_in`; `Vec`, `HashMap`, `HashSet` y `RingBuffer` propietarios; diez programas M3 y pruebas nativas | — |
| M4 — Módulos | Completo | Descubrimiento multarchivo, `package.reim`, imports selectivos y de módulo, aliases, reexports, `self::`/`super::`, acceso absoluto `x::y::z`, privacidad, ciclos completos, ambigüedades y diagnósticos por archivo; `examples/m4_modules/main.reim` ejecutable | — |
| M5 — FFI | Completo | Bloques `@link(...) extern "C"`, símbolos preservados, `@repr(C)`, `cstr` y literales `c"..."` con NUL validado, tipos ABI-safe, llamadas obligatoriamente `unsafe`, wrappers seguros, carga dinámica en JIT y directivas COFF `/DEFAULTLIB`; `examples/m5_ffi.reim` prueba llamadas escalares y `examples/m5_sdl_window.reim` abre y cierra una ventana SDL3 real mediante `defer`; descarga oficial reproducible y verificada por SHA-256 en `scripts/run-sdl-demo.ps1` | El enlazado autónomo completo pertenece al runtime/tooling; la clasificación de agregados C por valor puede ampliarse cuando un binding real la necesite |
| M6 — Generics | Completo | Parámetros de tipo y `const`, defaults, bounds y cláusulas `where`; monomorfización bajo demanda y cacheada de funciones, structs, enums, funciones asociadas y métodos; inferencia desde argumentos y retorno esperado; traits, supertraits, coherencia básica, validación de contratos, markers `Copy` y despacho estático; `examples/m6_methods.reim` y `examples/m6_generics.reim` ejecutables, este último verificado por JIT con resultado `42` | — |
| M7 — Tensor | Completo | `tensor<T, Rank>` move-only con `Vec<T>` contiguo y allocator explícito; shape/strides row-major, overflow de forma recuperable, `TensorView`/`TensorViewMut` scoped, `get` por referencia, indexación multidimensional `[]` comprobada, `fill`, `multiply_scalar`, `add_into` y `matmul_into`; demos `m7_tensor.reim` y `m7_matmul.reim`, pruebas de lifetime y copia real de agregados | — |
| M8 — Paquetes | Completo | `reimer.toml` estricto, SemVer, perfiles, lockfile reproducible y portable, checksums, path/git fijado por commit, unificación, ciclos y visibilidad directa; CLI `new/init/check/build/run/test/fmt/clean/add/remove`; paquetes ejecutables y biblioteca, tests ordenados, ejemplo `m8_packages`, linter/LSP conscientes del manifest | Registry y publicación pertenecen a un hito posterior según el LDD |
| M9 — Concurrencia | Completo | Function pointers tipados; `Send`/`Sync` estructurales sin punteros raw; threads nativos y scoped; `Mutex`, `RwLock`, `Channel`, `Barrier`, `Semaphore`, atomics y `ThreadLocal`; pool fijo con colas locales/work stealing, jobs tipados y `parallel_for_mut` para slices, arrays y tensors; sesiones JIT aisladas; cinco demos positivas y tres pruebas negativas de préstamos/transferencia | Async, fibers, scheduler ECS y ordenamientos atómicos configurables están fuera de v0.1 |
| M10 — Comptime | Completo | Constantes globales tipadas y const generics; funciones/bloques `comptime` deterministas con límites; `@repr`, `@derive`, `@align`, `@inline`, `@test` y `@must_use`; derives cerrados `Copy`, `Clone`, `Debug`, `Eq`, `Hash` y `Default`; reflexión tipada `size_of`, `align_of` y `meta::*`; layout compartido entre reflexión y Cranelift; tests unitarios aislados y `examples/m10_comptime.reim` verificado por JIT | `@inline` es una sugerencia del frontend; una política avanzada de inlining y reflexión runtime quedan fuera de v0.1 |

## Herramientas de editor

`reimer-lint` reutiliza el lexer, parser, resolver, grafo de paquetes y HIR del compilador para
producir diagnósticos, typos con quick fix, tipos inferidos, antipatrones y
estimaciones estáticas de allocator. `reimer-lsp` los publica por LSP junto con
hover, inlay hints, completado, definiciones, símbolos, CodeLens y organización
de imports. Los cambios en `reimer.toml` o `reimer.lock` reanalizan los
documentos abiertos. `editors/vscode` aporta el resaltado TextMate y empaqueta el servidor
para que los archivos `.reim` funcionen sin una extensión de Rust o Pylance.

## Diferencias deliberadas frente al borrador

- El backend C fue sustituido por generación nativa con Cranelift.
- Los paths usan `::`; `.` queda reservado para campos y métodos.
- El único tipo unidad es `()`.
- Los movimientos de valores no `Copy` son implícitos.
- `str` es una vista UTF-8 no propietaria `(pointer, length)`.
- `Clone` derivado solo existe para valores cuya copia es infalible y no asigna;
  las copias propietarias siguen usando APIs que hacen visible el allocator o
  la operación de recurso.

## Gates permanentes

Cada hito debe mantener en verde:

```text
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -D warnings
```

Además, `reimer check` debe ejecutar todo el frontend sin codegen, `reimer run`
debe ejecutar al menos un programa representativo y `reimer build` debe
producir un objeto nativo válido hasta que el runtime de arranque permita
enlazar ejecutables autónomos.
