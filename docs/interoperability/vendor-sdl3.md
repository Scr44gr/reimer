# Vendored SDL3 package

Reimer keeps a curated SDL3 package under `vendor/sdl3`. This gives applications
a reproducible dependency version, a visible native license and provenance,
and a safe public API without copying raw declarations into every program.

The first target is Windows x64 and ships the official SDL 3.4.12 Visual C++
runtime and import library. Their SHA-256 digests, together with the digest of
the upstream development archive, live in `vendor/sdl3/checksums.sha256`.

## Add the dependency

Reference the package from the application's `reimer.toml`:

```toml
[dependencies]
sdl3 = { path = "../reimer/vendor/sdl3", version = "=3.4.12" }
```

The facade exposes window ownership, generic input snapshots, an allocator-backed
ARGB8888 frame buffer, whole-file byte loading, and presentation:

```reimer
from std::alloc import general_allocator;
from sdl3 import Display, FrameBuffer, SdlError, rgb;
import sdl3::input as input;

fn main() -> Result<(), SdlError> {
    let allocator = general_allocator();
    let mut frame = FrameBuffer::new(&allocator, 320, 200)?;
    defer frame.deinit();

    let display = Display::open(c"Software renderer", 800, 600, 320, 200)?;
    defer display.close();

    let state = display.poll_input();
    if state.quit_requested()
        || state.key_down(input::SCANCODE_ESCAPE)
    {
        return Ok(());
    }

    frame.clear(rgb(135, 206, 235));
    display.present(&frame)
}
```

Raw handles, pointers, and `unsafe` FFI calls stay private to the package. The
safe facade validates dimensions, clips pixel writes, drains events, and owns
SDL teardown order. Input policy stays in the application: the vendor exposes
typed mouse buttons and the complete SDL 3.4.12 physical-key scancode catalog,
while each program maps those values to its own actions.

## Run or build an application

Use the package launcher so the native loader and linker both see the pinned
artifacts:

```powershell
.\vendor\sdl3\tools\invoke.ps1 `
    -Project ..\my-graphics-app `
    -Command run `
    -Release `
    -Locked
```

`-Command build` also copies `SDL3.dll` beside the generated executable. The
package README documents its target matrix and update procedure.
