# SDL3 vendor package

This package provides a curated, source-visible SDL3 binding for Reimer. Its
initial API focuses on software-rendered applications: explicit initialization,
window and renderer ownership, streaming ARGB8888 frame buffers, event draining,
generic keyboard and mouse snapshots, whole-file byte loading, presentation, and
deterministic cleanup.

The public `Display`, `FrameBuffer`, and `FileBytes` APIs are safe. Raw pointers
and native calls stay inside the package implementation. The binding is
intentionally a maintained subset rather than a mechanically generated copy of
every SDL API.

## Dependency

Add the package by path from an application manifest:

```toml
[dependencies]
sdl3 = { path = "../reimer/vendor/sdl3", version = "=3.4.12" }
```

Then import the public facade:

```reimer
from sdl3 import Display, FrameBuffer, SdlError, load_file, rgb;
import sdl3::input as input;
```

Input remains application-neutral. SDL scancodes describe physical keys, and
the application decides which actions they trigger:

```reimer
let state = display.poll_input();
if state.quit_requested()
    || state.key_down(input::SCANCODE_ESCAPE)
{
    return Ok(());
}
if state.mouse_button_down(input::MOUSE_BUTTON_LEFT) {
    // Handle a primary-button action.
}
```

The `input` module exposes typed `Scancode` and `MouseButton` values, the
complete SDL 3.4.12 scancode catalog, and checked `from_raw` constructors.
Each `InputState` owns its snapshot; it does not retain SDL keyboard pointers.

## Bundled target

The repository currently ships the official SDL 3.4.12 Visual C++ x64
`SDL3.dll` and `SDL3.lib` under `native/windows-x86_64`. They came from
`SDL3-devel-3.4.12-VC.zip`, whose SHA-256 is recorded in `checksums.sha256`.
The SDL source and binaries use the zlib license in `LICENSE.txt`.

Run a dependent project with the vendored loader and linker paths:

```powershell
.\vendor\sdl3\tools\invoke.ps1 -Project ..\voxel-space-reimer -Command run -Release -Locked
```

For `build`, the launcher also copies `SDL3.dll` beside the generated
executable. Other operating systems can use the same source binding after a
matching, version-pinned native directory and launcher branch are added.

## Updating SDL

1. Select a stable upstream SDL release.
2. Download the official development archive from the SDL GitHub release.
3. Verify the archive digest published by GitHub.
4. Replace only the target-specific library files and upstream license.
5. Regenerate `checksums.sha256` and run this package's format, check, and tests.
