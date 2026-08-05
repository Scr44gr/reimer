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

## Run a manifest-backed SDL3 and OpenGL application

`m5_sdl_opengl.reim` remains a focused single-file compiler fixture. For an
interactive project that declares every native dependency in `reimer.toml`,
run the SDL3 + OpenGL + Dear ImGui example from the repository root:

```powershell
reimer run examples\imgui_demo --release --locked
```

The project receives SDL3, OpenGL, and Dear ImGui through package dependencies.
Reimer resolves the vendored linker and runtime files transitively without a
launcher or `PATH`/`LIB` changes. The upstream SDL archive and checked-in
artifact digests remain recorded in `vendor/sdl3/checksums.sha256`.

## Check the standalone compiler fixture

Object generation validates the lexer, parser, types, FFI, and backend without
loading SDL or opening a window:

```powershell
reimer emit-object examples\m5_sdl_opengl.reim
```

The standalone fixture names Windows OpenGL directly. Portable applications
should use a project manifest and target-specific vendor declarations instead
of relying on process environment variables.
