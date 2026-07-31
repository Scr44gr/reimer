# Filesystem

`std::fs` exposes owned files and UTF-8 path operations without requiring
application code to enter `unsafe`. Its native ABI is private to the standard
library.

## Ownership

`File` owns one native handle. It is move-only and must be closed explicitly,
normally with `defer`:

```reimer
from std::fs import FileError, open;

fn inspect(path: str) -> Result<usize, FileError> {
    let mut file = open(path)?;
    defer file.deinit();
    file.remaining_len()
}
```

`open`, `create`, and `append` return `Result<File, FileError>`. `create`
truncates an existing file; `append` preserves existing contents. `flush`
reports buffered write failures before the handle is closed.

## Explicit reads

Reads never select an allocator implicitly:

```reimer
let mut file = open(path)?;
defer file.deinit();
let buffer = file.read(&allocator, 4096)?;
defer buffer.deinit();
```

`read` initializes at most the requested capacity. `read_exact` distinguishes
an early end-of-file with `FileError::UnexpectedEndOfFile`. `read_to_end`
queries the unread regular-file length and allocates exactly that many bytes
through the supplied allocator. `read_to_string` additionally validates UTF-8
and transfers the allocation into `String`.

`FileBuffer` records initialized length separately from allocated capacity.
Moving it into `String` or calling `deinit` releases the allocation exactly
once.

## Writes and paths

`write` reports a potentially partial byte count. `write_all` and
`write_buffer` either write the complete bounded region or return an error.
Convenience functions `read_to_string` and `write_string` still require an
explicit allocator for reads and close their temporary file handles through
`defer`.

Paths are UTF-8 `str` views. `exists`, `remove_file`, and `rename` are safe,
recoverable operations. They do not expose native path pointers to application
code.

## Safety boundary

The runtime stores native files behind opaque nonzero integer handles. Every
raw pointer received by the ABI is paired with a byte length and validated
before Rust creates a slice or UTF-8 path. The safe standard-library wrappers
own those handles, enforce bounded buffers, and keep all native calls inside
documented `unsafe` blocks.
