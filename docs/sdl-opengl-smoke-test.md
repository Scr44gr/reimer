# SDL3 and OpenGL smoke test

`examples/m5_sdl_opengl.reim` validates the smallest native graphics path:

1. initialize the SDL3 video subsystem;
2. create a window flagged for OpenGL;
3. create and activate the associated OpenGL context;
4. clear the framebuffer to blue;
5. swap buffers and keep the window visible for 1.2 seconds;
6. destroy the context before the window and stop SDL through `defer`.

The example's public API is safe. `unsafe` blocks are limited to FFI calls and
use opaque handles returned by SDL.

## Run on Windows x64

From the repository root:

```powershell
.\scripts\run-sdl-opengl-demo.ps1
```

The script downloads SDL 3.4.12 from its official release only when missing,
verifies the archive SHA-256, and temporarily adds the `SDL3.dll` directory to
`PATH`. OpenGL 1.1 comes from `opengl32.dll`, which is included with Windows.
The example only calls `glClearColor` and `glClear`, so it does not require an
extension loader.

A successful run displays a blue window and ends with
`program returned 42`.

## Check without native libraries

Object generation validates the lexer, parser, types, FFI, and backend without
loading SDL or opening a window:

```powershell
cargo run -p reimer-cli --locked -- emit-object examples/m5_sdl_opengl.reim
```

Execution requires Windows x64 with a working OpenGL driver. On other systems,
replace the `opengl32` link with the platform OpenGL library and provide SDL3
on the dynamic loader path.
