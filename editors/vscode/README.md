# Reimer Language Support for VS Code

Soporte de editor basado en la gramática y el compilador reales del repositorio.
La extensión no copia gramáticas de Rust ni componentes de Pylance: ofrece una
experiencia equivalente en intención mediante TextMate y un servidor LSP propio.

## Capacidades

- reconocimiento de archivos `.reim` y resaltado de comentarios anidados,
  literales UTF-8/C, declaraciones, genéricos, atributos, paths `::`, operadores
  y control de flujo;
- diagnósticos del lexer, parser, manifest/lockfile, package loader y resolver;
- hover e inlay hints con tipos inferidos;
- ir a definición local, símbolos del documento y completado;
- quick fixes para typos cercanos y antipatrones;
- `source.organizeImports` para ordenar `std` primero y normalizar imports
  selectivos sin eliminar comentarios;
- estimaciones estáticas, explícitamente etiquetadas, de reservas conocidas por
  allocator, con inlay hints y CodeLens.
- resolución de dependencias path/git desde `reimer.toml`, con reanálisis al
  cambiar el manifest, el lockfile o una fuente `.reim`.
- resaltado y completado para `comptime`, constantes, reflexión y atributos de
  M10; `*` y `->` comparten el mismo scope de operador.
- advertencias de `@must_use` respaldadas por la HIR, también en paquetes
  multarchivo guardados.

## Instalación para desarrollo

Desde la raíz del repositorio:

```powershell
cargo build --release -p reimer-lsp
cd editors\vscode
npm install
npm run compile
```

En VS Code ejecuta **Developer: Install Extension from Location...** y elige
`editors/vscode`. Durante el desarrollo, configura `reimer.server.path` con la
ruta absoluta a `target/release/reimer-lsp.exe`.

Para generar un paquete instalable:

```powershell
npm run package
```

El comando compila e incluye el servidor y produce un VSIX `win32-x64`
autocontenido. En esa instalación no hace falta configurar el path. Para otras
plataformas, compila el servidor local y usa `reimer.server.path`.

## Precisión de memoria

Las cifras mostradas son estimaciones estáticas, no perfiles de memoria. Una
cantidad constante en `allocate_bytes`, entrada acotada o `String::from` puede
calcularse exactamente; valores de runtime se muestran como dinámicos. Las
reservas dentro de loops se expresan por iteración. No se afirma un pico total
cuando el control de flujo o la vida útil no pueden demostrarse.

## Limitaciones actuales

Los cambios sin guardar se analizan completamente cuando no dependen de imports.
Para un paquete multiarchivo, los diagnósticos e inferencia intermodular se
actualizan contra el snapshot guardado en disco. El soporte actual de ir a
definición es intradocumento.
