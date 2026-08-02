# Standard library overview

The standard library is source-visible and ships with the compiler. Public wrappers expose safe language-level APIs; private runtime boundaries contain operating-system calls, allocation internals, and the small amount of implementation-level `unsafe` required by native interop.

## Modules by use case

| Need | Module | Main types and functions |
|---|---|---|
| Allocate owned memory | `std::alloc` | `Allocator`, `OwnedBytes`, `ArenaAllocator`, `FixedBufferAllocator` |
| Grow collections | `std::collections` | `Vec`, `HashMap`, `HashSet`, `RingBuffer` |
| Build UTF-8 text | `std::string` | `String`, concatenation, repetition, search, Unicode conversion |
| Format values | `std::fmt` | `Display`, `Debug`, `Formatter`, `FormatError` |
| Read and write terminals | `std::io` | `stdin`, `stdout`, `stderr`, `print`, `println`, bounded input buffers |
| Work with files | `std::fs` | `File`, `FileBuffer`, `open`, `create`, `append`, `rename`, `remove_file` |
| Decode binary formats | `std::binary` | bounded little-endian readers and IEEE-754 bit conversions |
| Detect byte corruption | `std::checksum` | CRC-32/ISO-HDLC over borrowed bytes |
| Measure and wait | `std::time` | `Duration`, `Instant`, `unix_time`, `monotonic`, `sleep` |
| Inspect the process | `std::env` | arguments, variables, current directory, executable path |
| Launch child processes | `std::process` | `Command`, `Child`, `ExitStatus`, `id`, `exit` |
| Inspect the target | `std::target` | `OperatingSystem`, `os` |
| Compute scalars and vectors | `std::math` | constants, scalar functions, `Vec2`, `Vec3`, `Vec4` |
| Use native layouts | `std::c` | target-correct C aliases, buffers, null-pointer helpers |
| Store multidimensional data | `std::tensor` | `tensor`, `TensorView`, `TensorViewMut` |
| Coordinate threads | `std::thread` | threads, locks, channels, atomics, barriers, semaphores, thread-local values |
| Schedule parallel work | `std::job` | `JobPool`, `Job`, `parallel_for_mut` |

## Importing APIs

Prefer selective imports when a module exposes several types:

```reimer
from std::alloc import AllocError, general_allocator;
from std::io import println;
from std::string import String;
```

Use a qualified module when the name itself provides useful context:

```reimer
import std::target;

let host = std::target::os();
```

The formatter orders standard-library imports before project and dependency imports.

## Ownership convention

Types that own memory or an operating-system resource expose `deinit`. Construct them through a fallible function, then register cleanup immediately:

```reimer
let mut file = open("data.txt")?;
defer file.deinit();
```

Methods named `into_*`, `wait`, or other consuming operations may transfer or finish ownership. Their signatures and documentation describe whether the original owner remains available.

## Failure convention

- Allocation returns `AllocError`.
- Terminal I/O returns `IoError`.
- Filesystem operations return `FileError`.
- Environment conversion returns `EnvError`.
- Child-process operations return `ProcessError`.
- Tensor construction and kernels return `TensorError`.

`Option` represents absence such as a missing environment variable or child exit code. Bounds-safe lookup also returns `Option`.

## API documentation in the editor

Standard declarations use `///` Markdown. VS Code displays the source-level signature, behavioral notes, and ownership information on hover. Projects can generate the same style of public Markdown with `reimer doc`.
