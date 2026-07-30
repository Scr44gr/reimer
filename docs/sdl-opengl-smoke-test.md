# Prueba mínima de SDL3 y OpenGL

`examples/m5_sdl_opengl.reim` valida la ruta gráfica nativa más pequeña:

1. inicializa el subsistema de vídeo de SDL3;
2. crea una ventana marcada para OpenGL;
3. crea y activa el contexto OpenGL asociado;
4. limpia el framebuffer con un color azul;
5. intercambia los buffers y mantiene la ventana visible durante 1,2 segundos;
6. destruye el contexto antes de la ventana y termina SDL mediante `defer`.

La API pública del ejemplo es segura. Los bloques `unsafe` quedan limitados a
las llamadas FFI, donde se usan los handles opacos entregados por SDL.

## Ejecutar en Windows x64

Desde la raíz del repositorio:

```powershell
.\scripts\run-sdl-opengl-demo.ps1
```

El script descarga SDL 3.4.12 desde su release oficial solo cuando falta,
comprueba el SHA-256 del archivo y añade temporalmente la carpeta de `SDL3.dll`
al `PATH`. OpenGL 1.1 procede de `opengl32.dll`, incluido en Windows; el ejemplo
solo usa `glClearColor` y `glClear`, por lo que no necesita un loader de
extensiones.

Un resultado correcto muestra una ventana azul y termina con
`program returned 42`.

## Comprobar sin librerías nativas

La generación del objeto valida lexer, parser, tipos, FFI y backend sin cargar
SDL ni abrir una ventana:

```powershell
cargo run -p reimer-cli --locked -- emit-object examples/m5_sdl_opengl.reim
```

Para ejecutarlo se necesita Windows x64 con un controlador OpenGL funcional.
En otros sistemas debe sustituirse el enlace `opengl32` por la biblioteca
OpenGL de la plataforma y proporcionar SDL3 en la ruta de carga dinámica.
