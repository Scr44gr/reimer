# OpenGL vendor package

This package provides generated, application-neutral OpenGL 4.6 core bindings
for Reimer. The generated `raw` module contains the Khronos constants, typed
command signatures, and an optional function table. It does not create a
window or a graphics context.

The registry snapshot is pinned by commit and SHA-256 in
`registry.lock.json`. Generated code carries the same provenance and uses the
Apache-2.0 license declared by `gl.xml`.

## Loading commands

Provide a platform or window-system resolver after making an OpenGL context
current:

```reimer
import opengl::raw;

let functions = raw::Functions::load(load_current_context_command);
```

`src/windows.reim` supplies a correct WGL resolver which falls back to
`opengl32.dll` for OpenGL 1.1 exports. It does not depend on SDL3. An SDL3,
GLFW, EGL, GLX, or native window package can provide the same one-function
contract without changing this vendor.

Every entry in `Functions` is optional because command availability belongs to
the current context version and extension set. Check an entry before calling a
raw command. The small facade wraps clear color, buffer clearing, viewport, and
error reporting with explicit `Result` values.

## Updating

Edit `registry.lock.json` to a reviewed Khronos commit and checksum, then run:

```text
python tools/update.py
reimer fmt .
reimer check . --locked
reimer test . --locked
```

For an offline or pre-reviewed snapshot, pass `--registry path/to/gl.xml`.
