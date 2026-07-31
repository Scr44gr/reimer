# Environment and processes

Reimer exposes process context through safe `std::env` and `std::process`
wrappers. Native operating-system values remain behind a private runtime ABI;
application code does not need `unsafe` for arguments, environment variables,
process identifiers, or child-process management.

## Command-line arguments

`std::env::args()` returns a lightweight `Arguments` view. Index zero
conventionally identifies the source entry or executable. Each `get` copies one
argument into a caller-selected allocator because Reimer `String` is owned and
always valid UTF-8:

```reimer
from std::alloc import general_allocator;
from std::env import args;

fn main() -> i32 {
    let allocator = general_allocator();
    let arguments = args();
    match arguments.get(1, &allocator) {
        Ok(Some(value)) => {
            defer value.deinit();
            if value.matches("fast") { 42 } else { 1 }
        },
        Ok(None) => 2,
        Err(_) => 3,
    }
}
```

The JIT accepts program arguments after `--`:

```text
reimer run examples/platform_environment.reim -- fast
```

An executable produced by `reimer build` reads its own native argument list in
the same way.

Native argument and path values are not assumed to be Unicode. Conversion to a
Reimer string returns `EnvError::NotUnicode` instead of replacing bytes or
panicking.

## Environment and paths

The environment module provides:

- `var(allocator, name) -> Result<Option<String>, EnvError>`;
- `current_dir(allocator) -> Result<String, EnvError>`;
- `current_exe(allocator) -> Result<String, EnvError>`.

`Ok(None)` means that a variable is not set. Variable names reject empty
strings, `=`, and NUL. All returned strings belong to the supplied allocator
and require `deinit`.

Reimer intentionally does not expose process-global `set_var` or `remove_var`.
Global environment mutation is not safely portable once a process has multiple
threads. Configure a child's environment through `Command` instead.

## Direct commands

`std::process::Command` invokes an executable directly. It does not parse a
shell command line:

```reimer
from std::process import Command, ProcessError;

fn run_tool() -> Result<i32, ProcessError> {
    let command = Command::new("tool.exe")?
        .with_arg("--check")?
        .with_env("MODE", "strict")?
        .with_current_dir("workspace")?;
    let status = command.status()?;
    Ok(if status.success() { 42 } else { 2 })
}
```

Standard input, output, and error are inherited by default. `env`,
`env_remove`, and `env_clear` modify only the child's environment and do not
mutate global process state.

The consuming `with_*` builders release the command automatically if
validation fails, which makes them the preferred form in a `?` chain. Mutable
`arg`, `env`, and `current_dir` remain available for incremental configuration
when the caller handles cleanup explicitly.

On Windows, direct `.bat` and `.cmd` execution is rejected with
`UnsupportedScript`. Those formats require command-shell escaping with
security-sensitive rules. Prefer a real executable.

## Child ownership

`Command::status(self)` consumes the command and waits. `Command::spawn(self)`
returns a move-only `Child`:

```reimer
let child = command.spawn()?;
let child_id = child.id();
let status = child.wait()?;
```

`Child::wait(self)` consumes and collects the native child. `kill(&mut self)`
requests forceful termination but keeps ownership so the caller can still
wait. `Child::deinit(self)` is scoped cleanup: it terminates a live child,
waits, and releases its handle. This avoids silently leaving uncollected child
resources.

`ExitStatus::success()` is the portable success test. `code()` returns
`Option<i32>` because some operating systems can report termination without a
numeric exit code.

`std::process::id()` returns the current process identifier. `exit(code)`
terminates immediately and does not run deferred cleanup, so returning from
`main` is preferred when possible.

## Safety boundary

The runtime stores commands, children, and temporary UTF-8 snapshots behind
opaque integer handles. Every raw pointer crossing the ABI is paired with an
explicit length or a fixed scalar output slot and is validated before use.
The public source API contains no `unsafe` operations.
