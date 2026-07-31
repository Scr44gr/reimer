# SDL3 and OpenGL demo

`examples/m5_sdl_opengl.reim` validates the first complete native graphics
path from LDD section 22.1:

1. obtain the general allocator and create an allocator-backed
   `tensor<f32, 2>`;
2. store the RGBA clear color in that tensor and register its cleanup with
   `defer`;
3. initialize the SDL3 video subsystem;
4. create a window flagged for OpenGL and activate its context;
5. pass tensor elements to OpenGL, clear the framebuffer, and swap buffers;
6. propagate allocation, initialization, and presentation failures through
   `Result`;
7. destroy the context before the window, stop SDL, and release the tensor
   through LIFO `defer` actions.

The example's public API is safe. `unsafe` blocks are limited to FFI calls and
use opaque handles returned by SDL. Tensor allocation, indexing, borrowing,
and cleanup remain safe source-language operations.

## Run on Windows x64

From the repository root:

```powershell
.\scripts\demos\run-sdl-opengl.ps1
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
