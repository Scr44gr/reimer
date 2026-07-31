# Vendored packages

`vendor` contains maintained integrations whose versions and native artifacts
are pinned with the compiler source. Applications depend on these packages by
path and import their Reimer facade like any other package.

Each package must keep its upstream license, artifact checksums, supported
target matrix, update procedure, and tests next to the binding. Raw FFI remains
private whenever a safe, focused facade can represent the supported API.

Available packages:

- [`opengl`](opengl/README.md): generated OpenGL 4.6 core constants,
  signatures, context-local dispatch, and a Windows WGL resolver.
- [`sdl3`](sdl3/README.md): SDL3 windows, input, software frame buffers, and
  presentation for Windows x64.
- [`vulkan`](vulkan/README.md): generated Vulkan 1.4 core constants, typed
  global/instance/device dispatch, and extension-free instance bootstrap.
