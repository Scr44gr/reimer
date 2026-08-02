# WebGPU with wgpu-native

Reimer vendors [wgpu-native](https://github.com/gfx-rs/wgpu-native) 29.0.1.1
as the `wgpu` package. The vendor has three layers:

1. `wgpu::raw` is generated from the pinned official `webgpu.h` and `wgpu.h`
   headers. It exposes the complete standard WebGPU C API and the
   wgpu-native extensions.
2. `wgpu` owns reference-counted handles, validates null results and mapped
   ranges, and hides foreign calls from ordinary application code.
3. `wgpu-sdl3` converts an SDL3 window into a WebGPU surface without adding
   window-system details to either core package.

The raw layer remains available for advanced descriptors and operations. Raw
calls require `unsafe`; the owning facade does not.

## Add the packages

```toml
[dependencies]
wgpu = { path = "../reimer/vendor/wgpu", version = "^29.0" }
```

Add SDL integration only to applications that open a window:

```toml
sdl3 = { path = "../reimer/vendor/sdl3", version = "^3.4" }
wgpu_sdl3 = { package = "wgpu-sdl3", path = "../reimer/vendor/wgpu-sdl3", version = "^0.1" }
```

## Headless initialization

Every returned handle is an owner and has one matching `deinit` operation.
`defer` makes the required reverse destruction order explicit:

```reimer
from wgpu import Error, Instance;

fn initialize() -> Result<(), Error> {
    let instance = Instance::create()?;
    defer instance.deinit();

    let adapter = instance.request_adapter()?;
    defer adapter.deinit();

    let device = adapter.request_device()?;
    defer device.deinit();

    let queue = device.queue()?;
    defer queue.deinit();
    queue.wait()
}
```

Run the complete headless and mapped-buffer check from the repository root:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File vendor\wgpu\tools\invoke.ps1 -Project examples\wgpu_info
```

## SDL3 surfaces

`wgpu-sdl3` reads the platform handles exposed by SDL window properties:

- `HWND` and `HINSTANCE` on Windows;
- Wayland display/surface handles, with an Xlib fallback, on Linux;
- an SDL Metal view and its `CAMetalLayer` on macOS.

The adapter owns the Metal view on macOS and destroys it only after releasing
the WebGPU surface. This avoids a dangling native layer.

Surface construction transfers an `Instance` owner and borrows the SDL window:

```reimer
let instance = Instance::create()?;
let window_surface = create_surface(&window, instance)?;
defer window_surface.deinit();

let adapter = window_surface.request_adapter()?;
```

The surface retains its native instance reference, and the window borrow makes
destroying the SDL window before `WindowSurface::deinit` a compile-time error.
Use `instance.clone_ref()` when the original instance owner must remain usable.

The `examples/wgpu_window` program verifies native surface creation and
requests an adapter compatible with that surface. The currently bundled SDL3
runtime is Windows x64, so its convenience runner is:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File vendor\wgpu-sdl3\tools\invoke.ps1 -Project examples\wgpu_window
```

The adapter source is portable; Linux and macOS execution additionally require
the matching SDL3 runtime artifact in `vendor/sdl3/native/<platform>`.

## Native artifacts and reproducibility

`vendor/wgpu/artifacts.lock.json` records official release archive hashes for
Windows, Linux, and macOS on x86-64 and AArch64. `fetch.ps1` selects the host
archive, verifies its SHA-256 hash, verifies both headers, and builds the small
callback bridge locally with warnings treated as errors.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File vendor\wgpu\tools\generate.ps1
```

Generation keeps downloads under `target/wgpu-api-gen`, regenerates all raw
declarations, formats and checks the package, and rewrites
`checksums.sha256`. No floating branch or unverified binary participates in the
build.

## Asynchronous operations

wgpu-native 29.0.1.1 does not implement `wgpuInstanceWaitAny`. The bridge
therefore uses native blocking synchronization for callbacks and the
wgpu-native `wgpuDevicePoll(wait = true)` extension for GPU progress. The
calling thread sleeps in the operating system instead of polling in a CPU loop.
Callback ABI details remain in C, where by-value `WGPUStringView` parameters
automatically use the target platform's C calling convention.

## Supported C aggregate ABIs

The native backend classifies `@repr(C)` values for these 64-bit ABIs:

| Target family | Aggregate rules |
|---|---|
| Windows x86-64 | Microsoft x64 direct sizes and indirect aggregate copies |
| Linux x86-64 | System V INTEGER/SSE eightbytes, atomic register rollback, and stack memory class |
| Linux AArch64 | AAPCS64 integer chunks, HFA SIMD registers, full HFA spill, and indirect large values |
| macOS AArch64 | Apple AArch64 rules, including its general-register alignment exception |
| macOS x86-64 | System V AMD64 classification |

Big-endian AArch64 and 32-bit aggregate ABIs are rejected with a compiler
diagnostic instead of being lowered with guessed rules.
