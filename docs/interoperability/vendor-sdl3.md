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

The facade exposes reference-counted subsystem initialization, window
ownership, events, generic input snapshots, clipboard text, timing, and
whole-file byte loading. It intentionally contains no application-specific
frame buffer, controls, or rendering policy:

```reimer
from sdl3 import Subsystems, Window;
import sdl3::events as events;
import sdl3::init as init;
import sdl3::input as input;
import sdl3::raw::constants as constants;
import sdl3::time as time;

fn main() -> i32 {
    let systems = match Subsystems::init(init::VIDEO | init::EVENTS) {
        Ok(value) => value,
        Err(_) => return 1,
    };
    defer systems.deinit();

    let window = match Window::create(
        c"SDL application",
        1280,
        720,
        constants::SDL_WINDOW_RESIZABLE,
    ) {
        Ok(value) => value,
        Err(_) => return 2,
    };
    defer window.deinit();

    let mut running = true;
    while running {
        match events::poll() {
            Some(event) => {
                if event.is_quit() {
                    running = false;
                }
            },
            None => (),
        }
        let keyboard = input::keyboard_state();
        if keyboard.is_down(input::SCANCODE_ESCAPE) {
            running = false;
        }
        time::sleep(1);
    }
    0
}
```

The safe modules hide their native calls and validate values where necessary.
Advanced bindings can use `sdl3::raw`, which exposes generated ABI declarations
and therefore requires explicit `unsafe`. Input policy stays in the application:
the vendor exposes typed mouse buttons and the complete SDL 3.4.12 physical-key
scancode catalog, while each program maps them to its own actions.

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
