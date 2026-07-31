<div class="reimer-hero">
  <p class="reimer-kicker">NATIVE · EXPLICIT · TOOLABLE</p>
  <h1>Build close to the machine without losing the map.</h1>
  <p class="reimer-lead">Reimer is an experimental compiled language for games, engines, and content tools. It combines explicit allocation and predictable native layouts with recoverable errors, a modern package workflow, and compiler-backed editor tooling.</p>
  <div class="reimer-actions">
    <a class="reimer-button reimer-button-primary" href="getting-started/installation.html">Install Reimer</a>
    <a class="reimer-button" href="guide/tour.html">Take the tour</a>
  </div>
</div>

<div class="reimer-status">
  <strong>Experimental release</strong>
  <span>The v0.1 language surface is implemented end to end, but compatibility is not frozen yet. Expect deliberate changes before a stable release.</span>
</div>

## A small language with visible costs

Reimer makes resource decisions part of the program. Owned collections receive an allocator, cleanup is explicit, and operations that can fail return `Result` or `Option`. The compiler still keeps everyday code compact through type inference, implicit moves, `defer`, and the `?` operator.

```reimer
from std::alloc import AllocError, general_allocator;
from std::io import println;
from std::string import String;

fn greet(name: str) -> Result<(), AllocError> {
    let allocator = general_allocator();
    let mut message = String::with_capacity(&allocator, 48)?;
    defer message.deinit();

    message.push_format(f"Hello, {name}!")?;
    match println(message.as_str()) {
        Ok(_) => Ok(()),
        Err(_) => Ok(()),
    }
}
```

## What can you build today?

<div class="reimer-grid">
  <div class="reimer-card">
    <h3>Native tools</h3>
    <p>Read files, inspect the environment, launch child processes, format Unicode text, and emit standalone executables.</p>
    <a href="standard-library/io-and-files.html">Explore system APIs →</a>
  </div>
  <div class="reimer-card">
    <h3>Games and graphics</h3>
    <p>Call C libraries through typed FFI, create SDL3 windows, use OpenGL, and manage contiguous tensor-backed data.</p>
    <a href="interoperability/sdl3-opengl.html">Run the graphics demo →</a>
  </div>
  <div class="reimer-card">
    <h3>Parallel workloads</h3>
    <p>Use scoped threads, locks, channels, atomics, barriers, semaphores, and a fixed work-stealing job pool.</p>
    <a href="standard-library/concurrency.html">Read the concurrency model →</a>
  </div>
  <div class="reimer-card">
    <h3>Compiler-aware editing</h3>
    <p>Get real inferred types, documentation hovers, completion, rename, import organization, lints, and allocator estimates in VS Code.</p>
    <a href="getting-started/editor.html">Configure the editor →</a>
  </div>
</div>

## The shortest path to a program

```powershell
reimer new hello
cd hello
reimer run .
reimer build . --release --locked
```

`reimer run` executes through the JIT. `reimer build` emits and links a native executable. Once built, the program does not need the Reimer compiler or a separate Reimer runtime.

Continue with [installation](getting-started/installation.md), then build [your first project](getting-started/first-project.md). For exact feature maturity, see the [implementation status](internals/implementation-status.md).
