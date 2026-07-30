# Concurrencia en Reimer

M9 ofrece concurrencia nativa sin ampliar la superficie pública de `unsafe`.
Los programas usan `std::thread` y `std::job`; los punteros, buffers ABI y
function thunks quedan encapsulados entre el backend y el runtime.

## Threads y seguridad de tipos

`Thread::spawn` transfiere el argumento al worker y devuelve un `Thread<T>` que
entrega el resultado una sola vez mediante `join`. `scope` crea un worker
estructurado, espera su finalización antes de retornar y por eso puede recibir
un agregado con préstamos locales.

```reimer
from std::thread import Thread, ThreadError, scope;

fn increment(value: i32) -> i32 { value + 1 }

fn increment_borrowed(value: &mut i32) -> i32 {
    *value += 1;
    *value
}
```

`Send` y `Sync` son capacidades estructurales integradas:

- un valor es `Send` cuando todos sus campos pueden transferirse;
- un valor es `Sync` cuando todos sus campos permiten acceso compartido;
- los punteros raw nunca reciben ninguna de las dos capacidades;
- una referencia scoped no puede escapar dentro de un thread nativo;
- los channels y jobs mueven valores no `Copy`.

Los errores de creación, handles retirados, panics de workers y layouts
incompatibles se representan con `ThreadError` o `JobError`.

## Sincronización

`std::thread` expone:

- `Mutex<T>` y `RwLock<T>`;
- `Channel<T>` acotado;
- `Barrier` y `Semaphore`;
- `ThreadLocal<T>` para valores `Copy`;
- `AtomicBool`, `AtomicI64`, `AtomicU64`, `AtomicIsize` y `AtomicUsize`.

Los atomics usan orden secuencialmente consistente. Clone conserva ownership
compartido del recurso; `deinit` retira cada handle explícitamente. Todo acceso
posterior a un handle retirado es una violación de invariantes y termina con
`panic`, nunca con una desreferencia inválida.

## Jobs y paralelismo por datos

Un `JobPool` recibe allocator y una cantidad fija de workers. Los workers son
persistentes, cada uno posee una cola local y puede robar trabajo del extremo
de otras colas. `submit` mueve su argumento al pool y `Job::wait` transfiere el
resultado al caller.

```reimer
from std::alloc import general_allocator;
from std::job import JobPool, JobPoolConfig;

let allocator = general_allocator();
let pool = JobPool::init(
    &allocator,
    JobPoolConfig::fixed(4),
)?;
```

`parallel_for_mut` parte un slice en regiones exclusivas no solapadas. La
variante para arrays conserva su longitud estática y `tensor::parallel_for_mut`
opera sobre el almacenamiento contiguo del tensor. La llamada espera todos los
chunks antes de devolver, de modo que el préstamo exclusivo termina de forma
estructurada. `minimum_chunk` es un mínimo preferido; el runtime también
balancea la cantidad de tareas según el número de workers.

El type checker rechaza:

- dos préstamos mutables solapados del mismo array;
- punteros raw enviados como argumento de un job;
- referencias locales que escapan a un thread nativo;
- callbacks de `parallel_for_mut` que no reciben `&mut [T]`.

## Aislamiento del runtime

Cada ejecución JIT recibe una sesión propia. Threads, jobs y pools conservan
esa identidad, incluso al crear recursos desde un worker. Al terminar un
programa, el backend espera y retira únicamente sus recursos. Esto permite que
las pruebas y herramientas ejecuten varios programas concurrentemente sin que
la limpieza de uno cierre handles de otro.

Async/await, fibers, un scheduler ECS y atomics con orden configurable
permanecen fuera de Reimer v0.1.

## Pruebas manuales

```text
cargo run -p reimer-cli -- run examples/m9_threads/main.reim
cargo run -p reimer-cli -- run examples/m9_synchronization/main.reim
cargo run -p reimer-cli -- run examples/m9_atomics/main.reim
cargo run -p reimer-cli -- run examples/m9_jobs/main.reim
cargo run -p reimer-cli -- run examples/m9_tensor_parallel/main.reim
```
