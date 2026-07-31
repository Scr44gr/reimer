# Terminal I/O and files

Terminal and filesystem operations are safe, bounded, and recoverable.

## Printing

```reimer
from std::io import eprintln, print, println;

fn main() -> i32 {
    match print("loading...") {
        Ok(_) => match println("done") {
            Ok(_) => 0,
            Err(_) => 2,
        },
        Err(_) => {
            match eprintln("stdout failed") {
                Ok(_) => 1,
                Err(_) => 3,
            }
        },
    }
}
```

`print` and `println` write UTF-8 to standard output. `eprint` and `eprintln` target standard error. Handle objects from `stdout()` and `stderr()` add partial writes, full writes, flushing, and terminal detection.

## Reading input

Reads always receive a maximum or destination capacity. This prevents an unbounded hidden allocation.

```reimer
from std::alloc import general_allocator;
from std::io import IoError, stdin;

fn read_name() -> Result<i32, IoError> {
    let allocator = general_allocator();
    let input = stdin();
    let line = input.read_line_string(&allocator, 256)?;
    defer line.deinit();

    if line.is_empty() { Ok(0) } else { Ok(42) }
}
```

Use `read`, `read_exact`, `read_line`, or `read_to_end` when byte ownership is more appropriate. Conversion to `String` validates UTF-8.

## Files

```reimer
from std::alloc import Allocator;
from std::fs import FileError, open;
from std::string import String;

fn load(path: str, allocator: &Allocator) -> Result<String, FileError> {
    let mut file = open(path)?;
    defer file.deinit();
    file.read_to_string(allocator)
}
```

`open`, `create`, and `append` return an owned `File`. File methods include bounded reads, full reads, writes, flush, remaining length, and explicit cleanup.

High-level helpers cover common complete-file operations:

```reimer
import std::fs;

let text = std::fs::read_to_string(&allocator, "settings.txt")?;
defer text.deinit();

std::fs::write_string("snapshot.txt", text.as_str())?;
```

Path operations also include `exists`, `rename`, and `remove_file`. Paths are borrowed UTF-8 `str` values; invalid native-to-UTF-8 conversions return an error.

## Ownership transfer from a file buffer

`FileBuffer::into_string()` consumes the buffer on success and returns its initialized storage as `String`. Do not register both a buffer cleanup and a consuming conversion on the same path.

```reimer
let buffer = file.read_to_end(allocator)?;
buffer.into_string()
```

The [filesystem reference](filesystem.md) documents error variants, partial I/O behavior, and handle invariants in more detail.
