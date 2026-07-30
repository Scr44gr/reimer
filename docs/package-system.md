# Sistema de paquetes y build

M8 introduce proyectos declarativos sin scripts de build arbitrarios. El
compilador resuelve el grafo completo antes de cargar módulos, pero cada paquete
solo puede importar sus dependencias directas.

## Estructura

```text
game/
├── reimer.toml
├── reimer.lock
├── src/
│   ├── main.reim
│   └── world.reim
├── tests/
│   └── loading.reim
├── examples/
└── assets/
```

`src/main.reim` define un ejecutable. Un paquete biblioteca usa
`src/package.reim` como fachada pública y no necesita `main`. Las dependencias
siempre deben ofrecer esa fachada.

## Manifest

```toml
[package]
name = "game"
version = "0.1.0"
edition = "2026"

[dependencies]
physics = { path = "../physics", version = "^0.1" }
assets = { git = "https://example.com/assets.git", tag = "0.4.0" }

[profile.debug]
optimization = 0

[profile.release]
optimization = 3
```

Los nombres de versión y requisitos siguen SemVer. Una dependencia puede
renombrar el paquete con `package = "nombre-real"`. Las fuentes admitidas son:

- `path`, relativo al manifest que declara la dependencia;
- `git`, fijado por commit y opcionalmente seleccionado mediante `rev`,
  `branch` o `tag`.

Solo puede aparecer un selector Git. Las dependencias de registry se reconocen
para producir un error preciso, pero el registry y la publicación quedan para
un hito posterior.

## Resolución y lockfile

`reimer.lock` contiene identidades, versiones, fuentes, checksums y aristas
exactas. Los paths se guardan relativos a la raíz, por lo que mover el árbol
completo produce el mismo lockfile. Los commits Git quedan fijados hasta
solicitar una actualización.

- modo normal: reutiliza commits fijados y actualiza checksums obsoletos;
- `--locked`: rechaza un lockfile ausente o cualquier deriva;
- `--refresh`: vuelve a resolver referencias Git y reemplaza el lockfile.

Las versiones se unifican cuando nombre, versión y fuente identifican el mismo
paquete. Los ciclos se rechazan mostrando la cadena completa. Una dependencia
transitiva no es visible desde el paquete raíz salvo que también se declare
directamente.

## Comandos

```text
reimer new <path>
reimer init [path]
reimer check [path] [--locked|--refresh]
reimer build [path] [--release] [--locked|--refresh]
reimer run [path] [--release] [--locked|--refresh]
reimer test [path] [--release] [--locked|--refresh]
reimer fmt [path] [--check]
reimer clean [path]
reimer add <alias> (--path <path>|--git <url>)
reimer remove <alias>
```

`build` emite un objeto en `target/reimer/debug` o
`target/reimer/release`. Los niveles de manifest se traducen a las estrategias
de Cranelift: `0` desactiva optimización, `1–2` optimizan velocidad y `3`
equilibra velocidad y tamaño. `run` ejecuta el mismo grafo por JIT.

Cada archivo `.reim` bajo `tests/` es una prueba de integración independiente.
Debe definir `fn main() -> i32`; retornar `0` significa éxito. La selección y
ejecución se ordenan por path para que el resultado sea determinista.

`fmt` normaliza espacios finales, salto final y orden de imports; con `--check`
solo verifica. `clean` elimina exclusivamente `target/reimer` después de
resolver y comprobar la raíz del proyecto.

## Restricción deliberada

El manifest no ejecuta código, plugins ni scripts Turing-completos. Cualquier
generación futura deberá expresarse mediante entradas y salidas declarativas,
de modo que el grafo continúe siendo reproducible, inspeccionable por el LSP y
seguro para herramientas.
