# Metaprogramación de compilación

M10 añade evaluación determinista al frontend sin introducir una máquina
virtual runtime ni permitir efectos externos durante la compilación.

## Constantes y funciones `comptime`

Una constante global debe tener tipo explícito. Su inicializador se evalúa
durante la resolución y el valor resultante se integra en la HIR tipada:

```reimer
comptime fn factorial(value: usize) -> usize {
    if value <= 1 {
        1
    } else {
        value * factorial(value - 1)
    }
}

const TABLE_SIZE: usize = factorial(5);

struct Table<T, const N: usize> {
    values: [T; N],
}
```

Los bloques `comptime` se ejecutan al comprobar el programa y son apropiados
para invariantes:

```reimer
comptime {
    assert(TABLE_SIZE == 120);
}
```

La evaluación admite escalares, strings, tuplas, arrays y structs, llamadas
puras, variables locales, ramas, `match`, loops y casts comprobados. Un
`panic` o `assert` fallido se convierte en un diagnóstico del compilador.

Cada unidad de compilación aplica los siguientes límites:

- 1 000 000 de pasos;
- 16 MiB de valores conservados;
- 128 llamadas anidadas.

No están disponibles red, reloj, aleatoriedad, threads, E/S, filesystem, FFI,
punteros raw, préstamos, `unsafe`, `defer` ni llamadas a funciones runtime.
Estas reglas también se validan en funciones `comptime` que todavía no hayan
sido invocadas.

## Atributos

Los atributos válidos forman una lista cerrada y se rechazan si aparecen en un
destino incompatible:

```reimer
@repr(C)
@align(16)
@derive(Copy, Clone, Debug, Eq, Hash, Default)
@must_use
struct Header {
    kind: u32,
    length: u32,
}

@inline
fn compact(value: Header) -> u64 {
    (value.kind as u64) << 32 | value.length as u64
}

@test
fn header_default_should_be_zeroed() {
    assert(Header::default() == Header { kind: 0, length: 0 });
}
```

- `@repr(C)` conserva la representación interoperable de un struct FFI.
- `@align(N)` aumenta el alineamiento a una potencia de dos válida.
- `@derive(...)` solicita implementaciones estructurales conocidas.
- `@inline` es una sugerencia de optimización; no cambia la semántica.
- `@test` exige una función sin parámetros, genéricos ni retorno.
- `@must_use` advierte en el linter/LSP cuando se descarta el resultado.

`Copy`, `Eq`, `Hash`, `Debug` y `Default` solo se aceptan si todos los campos
permiten la operación. `Default` de un enum usa su primera variante. `Clone`
solo se deriva para campos `Copy`: `value.clone()` nunca asigna, falla ni
requiere allocator. Los contenedores propietarios mantienen `clone_in`.

## Reflexión tipada

Los descriptores existen únicamente durante la compilación:

```reimer
const HEADER_SIZE: usize = size_of<Header>();

comptime {
    assert(align_of<Header>() == 16);
    assert(meta::name<Header>() == "Header");
    assert(meta::fields<Header>()[0].name == "kind");
    assert(meta::variants<Option<i32>>()[0] == "Some");
    assert(meta::traits<Header>()[0] == "Clone");
}
```

- `size_of<T>()` y `align_of<T>()` consultan el mismo cálculo de layout usado
  por el backend nativo.
- `meta::name<T>()` devuelve el nombre canónico.
- `meta::fields<T>()` devuelve descriptores `{ name, type }`.
- `meta::variants<T>()` devuelve los nombres de las variantes.
- `meta::traits<T>()` devuelve traits satisfechos en orden determinista.

Estas funciones requieren argumentos genéricos explícitos y no pueden llamarse
desde código runtime. Los const generics usan los mismos valores evaluados y
comprobados por el frontend.

## Ejecución

```text
cargo run -p reimer-cli -- check examples/m10_comptime.reim
cargo run -p reimer-cli -- run examples/m10_comptime.reim
cargo run -p reimer-cli -- test examples/m10_comptime.reim
```
