# Dear ImGui vendor package

This package pins Dear ImGui 1.92.8 and Dear Bindings 0.21. It exposes 730
documented core C functions, 21 SDL3/OpenGL3 backend functions, public flag
constants, ABI types, and a safe `SdlOpenGl` lifecycle facade. The bundled
native artifact currently targets Windows x64.

The public facade owns the Dear ImGui context and borrows the SDL OpenGL
context. It processes SDL events, validates frame ordering, shows the official
demo window, renders with the official OpenGL3 backend, and shuts everything
down in the required reverse order. Application code does not need `unsafe`.
The generated `raw` module remains available for advanced integrations and
does require `unsafe` at native calls.

Every generated function and constant carries the comments preserved by Dear
Bindings. The Reimer language server therefore shows the signature and its
Dear ImGui documentation in VS Code hover and completion details.

## Use the package

Add `imgui`, `sdl3`, and `opengl` as direct path dependencies when the
application imports all three packages. Create and make the SDL OpenGL context
current before calling `SdlOpenGl::create`. During each iteration:

1. Pass every polled SDL event to `process_event`.
2. Call `new_frame`.
3. Submit the demo window or other widgets.
4. Clear the framebuffer.
5. Call `render` and swap the SDL window.

Run the complete example from the repository root:

```powershell
vendor\imgui\tools\invoke.ps1 -Project examples\imgui_demo -Command run -Release
```

For a distributable executable, use `-Command build`; the helper copies
`imgui.dll` and `SDL3.dll` beside the generated program.

## Reproduce the bindings and native bridge

The update script downloads only pinned upstream archives and release assets,
verifies each SHA-256 digest, regenerates the Reimer API from Dear Bindings
metadata, compiles the official SDL3/OpenGL3 backends, and records output
checksums:

```powershell
vendor\imgui\tools\generate.ps1
```

MSVC x64 build tools are required. Downloaded source, generated C glue, object
files, and symbol files remain under `target/imgui-api-gen`; they are not part
of the vendor package.

The fifteen printf-style variadic functions are intentionally excluded because
Reimer does not expose a C variadic ABI. Use non-variadic helpers such as
`ImGui_TextUnformatted`. Fourteen upstream-internal functions are also omitted;
the inactive `IMGUI_HAS_IMSTR` helper is omitted as well. `coverage.toml`
records these decisions.

Upstream references:

- [Dear ImGui 1.92.8](https://github.com/ocornut/imgui/releases/tag/v1.92.8)
- [Dear Bindings](https://github.com/dearimgui/dear_bindings)
- [Dear ImGui backend guide](https://github.com/ocornut/imgui/blob/master/docs/BACKENDS.md)
