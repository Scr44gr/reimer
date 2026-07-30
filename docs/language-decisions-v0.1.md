# Decisiones congeladas para Reimer v0.1

Este documento registra las decisiones de diseño aceptadas antes de continuar
el frontend y el backend nativo. Tienen prioridad sobre las propuestas
contradictorias del LDD v0.1-draft.

## D-001: paths con `::`

`::` separa módulos, tipos y símbolos asociados. `.` se reserva para acceder a
campos y llamar métodos sobre un valor.

```reimer
from game::math import Vec3;
from engine::render import Texture as GpuTexture;
from self::animation import Animator;
from super::shared import AssetId;

let origin = Vec3::zero();
let texture = engine::render::load_texture(path)?;
player.position = origin;
player.update(delta);
```

Un path absoluto empieza por un paquete visible. Los paths relativos empiezan
por `self::` o por uno o más segmentos `super::`. No se admite `import *`.

Formas de importación:

```text
import_item =
    [ "pub" ] , "import" , path , [ "as" , identifier ] , ";"
  | [ "pub" ] , "from" , path , "import" , import_name ,
    { "," , import_name } , [ "," ] , ";" ;

import_name = identifier , [ "as" , identifier ] ;
path = identifier , { "::" , identifier } ;
```

`from x::y import z;` introduce `z` en el módulo actual. `import x::y;`
introduce el último segmento (`y`), salvo que exista un alias. Un path completo,
como `x::y::z()`, puede usarse sin import si `x` es un paquete visible.

## D-002: tipo unidad

El único tipo unidad es `()`. Una función sin retorno escrito devuelve `()`.
`void` no es una palabra reservada.

```reimer
fn update() {
    return ();
}

fn save() -> Result<(), SaveError> {
    return Ok(());
}
```

En FFI, una función C sin valor de retorno se representa como una función Reimer
que devuelve `()`. No se introduce un segundo tipo para este caso.

## D-003: movimiento implícito al consumir

Los valores no `Copy` se mueven automáticamente al asignarlos, pasarlos por
valor, retornarlos o enviarlos por un channel. La expresión prefija `move` se
elimina de v0.1.

```reimer
let texture = Texture::load(allocator, path)?;
let second = texture;
render(texture); // error: `texture` ya fue movido
render(second);  // válido; consume `second`
```

Los préstamos `&T` y `&mut T` no consumen el valor. Solo los tipos que
implementan `Copy` se duplican implícitamente.

## D-004: representación de `str`

`str` es una vista UTF-8 inmutable, no propietaria y `Copy`, representada
conceptualmente por `(pointer, length)`. Se pasa por valor y nunca asigna
memoria. No se escribe `&str` en v0.1.

```reimer
fn load(path: str) -> Result<Asset, AssetError>;

let name: str = "player";
let owned = String::from(allocator, name)?;
let view: str = owned.as_str();
```

`String` es el buffer propietario, move-only, con longitud, capacidad y
allocator. Una vista `str` no puede sobrevivir al almacenamiento que contiene
sus bytes. El type checker aplicará las mismas reglas scoped conservadoras que
para otras vistas no propietarias.

## D-005: copia explícita de valores propietarios

v0.1 admite `Clone` únicamente como derive cerrado para valores cuya copia sea
infalible, no asigne y no necesite un allocator. Esto evita que una llamada
uniforme oculte asignaciones, fallos o semántica específica de recursos.

- `Copy` significa duplicación implícita, bit a bit, infalible y sin allocator.
- `Clone` derivado significa duplicación explícita con esas mismas restricciones.
- Los contenedores propietarios ofrecen `clone_in(allocator)`.
- Los recursos ofrecen operaciones con nombre semántico, como
  `duplicate_handle()` o `retain()`.

```reimer
@derive(Copy, Clone)
struct Point {
    x: f32,
    y: f32,
}

let second = point.clone();
let copy = bytes.clone_in(allocator)?;
let handle = texture.duplicate_handle()?;
```

Un tipo propietario, un campo no `Copy` o cualquier clon que pueda fallar
impide derivar `Clone`. Cuando existan suficientes implementaciones reales se
podrá añadir un trait `CloneIn`, manteniendo el allocator y el posible error
visibles en la firma.

## D-006: backend nativo

El backend C se elimina. El backend inicial usa Cranelift para generar código
máquina y objetos nativos.

```text
Fuente .reim
  -> lexer y parser
  -> resolución y type checking
  -> HIR tipado
  -> IR de Cranelift
  -> JIT para `reimer run`
  -> objeto nativo para `reimer build`
  -> runtime + LLD para el ejecutable final
```

La generación de objeto no equivale al enlazado. Cada plataforma necesita un
runtime de arranque, ABI, librerías del sistema y configuración de LLD. M0
entrega JIT ejecutable y objeto nativo; el ejecutable autónomo se añade como el
siguiente incremento del backend.

Las operaciones con semántica Reimer no se delegan ciegamente al host:

- overflow entero comprobado;
- división y resto por cero comprobados;
- shifts validados;
- bounds checks activos;
- traps convertidos en `panic` del runtime.

El lowering debe usar instrucciones checked de Cranelift o bloques explícitos
que llamen al runtime. Nunca dependerá del comportamiento indefinido de otro
lenguaje.

## D-007: E/S estándar segura

`std::io` ofrece `stdin()`, `stdout()` y `stderr()` como handles seguros. Las
operaciones públicas devuelven `Result`; un programa usuario no necesita
`unsafe` para leer, escribir o hacer flush.

```reimer
from std::alloc import general_allocator;
from std::io import println, stdin;

let allocator = general_allocator();
println("Escribe una línea:")?;
let input = stdin();
let line = input.read_line(&allocator, 4096)?;
defer line.deinit();
```

Las lecturas siempre reciben una capacidad explícita y devuelven un
`InputBuffer` propietario con longitud inicializada separada de la capacidad de
la asignación. `read_line` conserva el salto de línea, `read_exact` distingue
el EOF anticipado y `read_to_end` termina al alcanzar EOF o la capacidad.

La frontera privada entre `std::io` y el runtime usa punteros acotados por
`(pointer, length)`. Todo `unsafe` queda concentrado en estos adaptadores,
documentado con sus precondiciones y cubierto por pruebas; no forma parte de la
API segura del programa. Los locks, iteradores de líneas, `read_to_string`,
vectored I/O y los adapters genéricos se completan junto con traits, `String` y
los contenedores propietarios de M6.

## D-008: tensors y vistas scoped

`tensor<T, Rank>` es propietario y move-only. Su `Rank` es un const generic,
su forma es runtime y su almacenamiento inicial es contiguo row-major.
`TensorView` y `TensorViewMut` son agregados scoped: pueden contener slices,
pero el resolver propaga su préstamo y prohíbe ocultarlos dentro de storage
propietario basado en punteros raw.

```reimer
let shape: [usize; 2] = [1000, 3];
let mut positions: tensor<f32, 2> =
    tensor::zeros(allocator, shape)?;
defer positions.deinit();

positions[10, 2] = 42.0;
let position = positions.get([10, 2]); // Option<&f32>
let view = positions.view();
```

Una operación que crea almacenamiento recibe allocator. Los kernels que no
asignan, como `add_into` y `matmul_into`, reciben un output mutable explícito.
Los accesos `[]` conservan bounds checks en todos los perfiles.

## D-009: paquetes declarativos y lockfile portable

`reimer.toml` describe identidad SemVer, edición, dependencias y perfiles. No
admite scripts de build ni plugins ejecutables. Un paquete solo puede importar
dependencias directas y los ciclos se rechazan antes del frontend.

```toml
[package]
name = "game"
version = "0.1.0"
edition = "2026"

[dependencies]
physics = { path = "../physics", version = "^0.1" }
assets = { git = "https://example.com/assets.git", tag = "0.4.0" }
```

`reimer.lock` fija el grafo exacto, checksums de fuentes path y commits Git.
Los paths se serializan relativos a la raíz: reubicar el árbol completo no
cambia el lockfile. `--locked` falla si falta o deriva; `--refresh` vuelve a
resolver referencias Git.

`src/main.reim` define un ejecutable y `src/package.reim` una biblioteca. Las
pruebas de integración bajo `tests/` son programas independientes cuyo `main`
retorna `0` al pasar. Los perfiles `debug` y `release` controlan realmente la
estrategia de optimización de Cranelift.

## D-010: concurrencia estructurada y runtime aislado

`Send` y `Sync` son capacidades estructurales conocidas por el compilador. Un
agregado las satisface únicamente si todos sus campos las satisfacen; los
punteros raw no son `Send` ni `Sync`. Los threads nativos solo reciben datos
propietarios y los préstamos locales se limitan a `scope`, que garantiza el
join antes de devolver.

El job system usa un número fijo de workers persistentes, colas locales y work
stealing. `parallel_for_mut` crea slices mutables no solapados y espera todos
los chunks dentro de la misma llamada. El préstamo del array, slice o tensor
original permanece exclusivo hasta ese punto.

Locks, channels, atomics, barriers, semaphores y thread-local storage son APIs
seguras de `std::thread`. Los atomics de v0.1 son secuencialmente consistentes.
Las llamadas ABI y los function thunks permanecen privados y documentan cada
uso de `unsafe`.

Cada programa JIT obtiene una sesión de runtime. Sus threads y pools conservan
esa identidad, y el backend solo espera o destruye los recursos de la sesión
que está terminando. Esto evita interferencias entre compilaciones concurrentes
sin permitir que código generado siga ejecutándose después de liberar su
módulo.
