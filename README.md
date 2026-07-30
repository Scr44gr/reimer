# Reimer

Reimer es un lenguaje compilado orientado inicialmente al desarrollo de
videojuegos, motores y herramientas de contenido. El compilador está escrito en
Rust y su primer backend genera código máquina mediante Cranelift.

El compilador implementa los hitos M0–M10 del documento de diseño. Además del
programa mínimo, ya compila todos los tipos
escalares y agregados, control de flujo completo, errores recuperables,
movimientos y referencias, módulos con paths `::`, FFI C, métodos, genéricos de
tipo y `const`, monomorfización, traits con despacho estático, evaluación
`comptime`, atributos, derives cerrados y reflexión tipada. La biblioteca
estándar incluye allocators explícitos, E/S segura, `String`, `Vec`, `HashMap`,
`HashSet`, `RingBuffer`, tensors contiguos con vistas seguras, threads,
sincronización, atomics y un job pool con work stealing:

```reimer
fn factorial(value: i32) -> i32 {
    let mut current = value;
    let mut result = 1;
    while current > 1 {
        result *= current;
        current -= 1;
    }
    result
}
```

El recorrido cubre fuente `.reim` → tokens con spans → AST → resolución y
comprobación de tipos → HIR tipada → Cranelift. `run` ejecuta el programa por
JIT y `build` emite un objeto nativo.

## Uso

```text
cargo run -p reimer-cli -- check examples/exit_42.reim
cargo run -p reimer-cli -- emit-object examples/exit_42.reim
cargo run -p reimer-cli -- build examples/exit_42.reim
cargo run -p reimer-cli -- run examples/exit_42.reim
cargo run -p reimer-cli -- run examples/m1_language.reim
cargo run -p reimer-cli -- run examples/m2_scalars.reim
cargo run -p reimer-cli -- run examples/m2_composites.reim
cargo run -p reimer-cli -- run examples/m2_control.reim
cargo run -p reimer-cli -- run examples/m3_views.reim
cargo run -p reimer-cli -- run examples/m3_io.reim
cargo run -p reimer-cli -- run examples/m3_string.reim
cargo run -p reimer-cli -- run examples/m3_vec.reim
cargo run -p reimer-cli -- run examples/m3_collections.reim
cargo run -p reimer-cli -- run examples/m5_ffi.reim
cargo run -p reimer-cli -- run examples/m6_generics.reim
cargo run -p reimer-cli -- run examples/m7_tensor.reim
cargo run -p reimer-cli -- run examples/m7_matmul.reim
cargo run -p reimer-cli -- check examples/m8_packages/app --locked
cargo run -p reimer-cli -- run examples/m8_packages/app --release --locked
cargo run -p reimer-cli -- test examples/m8_packages/app --locked
cargo run -p reimer-cli -- run examples/m9_threads/main.reim
cargo run -p reimer-cli -- run examples/m9_synchronization/main.reim
cargo run -p reimer-cli -- run examples/m9_atomics/main.reim
cargo run -p reimer-cli -- run examples/m9_jobs/main.reim
cargo run -p reimer-cli -- run examples/m9_tensor_parallel/main.reim
cargo run -p reimer-cli -- check examples/m10_comptime.reim
cargo run -p reimer-cli -- run examples/m10_comptime.reim
cargo run -p reimer-cli -- test examples/m10_comptime.reim
```

Para crear un proyecto:

```text
reimer new game
reimer add physics --path ../physics --project game
reimer check game
reimer build game --release --locked
```

El formato de `reimer.toml`, la semántica del lockfile, los perfiles y las
dependencias path/git están documentados en
[`docs/package-system.md`](docs/package-system.md).
La evaluación de compilación, los atributos y la reflexión se especifican en
[`docs/metaprogramming.md`](docs/metaprogramming.md).

El objeto generado todavía no es un ejecutable autónomo. El enlazado con el
runtime de arranque y LLD es el siguiente incremento del backend.

## VS Code

`editors/vscode` contiene resaltado TextMate y una extensión conectada a
`reimer-lsp`. El servidor publica diagnósticos, tipos inferidos, hover,
completado, definiciones, organización de imports, quick fixes para typos y
antipatrones, y estimaciones estáticas de reservas por allocator. La instalación
y el empaquetado se describen en
[`editors/vscode/README.md`](editors/vscode/README.md).

## Desarrollo

```text
cargo fmt --all --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -D warnings
```

La arquitectura y el alcance exacto del hito están descritos en
[`docs/architecture.md`](docs/architecture.md). La cobertura completa del LDD
se mantiene en [`docs/implementation-status.md`](docs/implementation-status.md).
