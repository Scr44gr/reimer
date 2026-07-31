# Vulkan vendor package

This package provides generated, application-neutral Vulkan 1.4 core bindings
for Reimer. It contains ABI aliases, registry constants, opaque pointer types,
typed command signatures, and separate global, instance, and device dispatch
tables. Surface creation belongs to a window-system package and is deliberately
outside this vendor.

The source registry is Vulkan-Headers tag `vulkan-sdk-1.4.357.0`. Its commit and
`vk.xml` SHA-256 are pinned in `registry.lock.json`. The generated file retains
Khronos' `Apache-2.0 OR MIT` license declaration.

## Loading commands

Supply a callback matching `vkGetInstanceProcAddr`:

```reimer
from vulkan import Loader, Version;

let loader = Loader::load(platform_get_instance_proc_addr);
let instance = loader.create_instance(
    c"Example",
    Version::new(0, 1, 0),
    c"Example Engine",
    Version::new(0, 1, 0),
    Version::new(1, 0, 0),
)?;
defer instance.close();
```

`src/windows.reim` provides the platform callback through `vulkan-1.dll` and
does not depend on SDL3. Other platforms can supply the same callback from
their Vulkan loader.

The checked facade intentionally bootstraps only an extension-free instance.
The generated `raw` module exposes all 234 core commands through optional
dispatch entries. Platform and optional extension commands are the next
generator layer; they must be enabled and loaded according to their registry
requirements rather than assumed globally.

Non-dispatchable handles use their 64-bit ABI representation. The current
native Reimer compiler targets 64-bit hosts; a future 32-bit target must select
the registry's platform-specific handle representation during generation.

## Updating

Edit `registry.lock.json` to a reviewed Khronos tag, commit, and checksum, then
run:

```text
python tools/update.py
reimer fmt .
reimer check . --locked
reimer test . --locked
```

For an offline or pre-reviewed snapshot, pass `--registry path/to/vk.xml`.
