# Installation

Reimer releases contain native command-line tools and the matching standard library. Keep the `std` directory next to the executables; the compiler uses it to resolve imports such as `std::io` and `std::string`.

> The current compiler uses the Rust toolchain that built it as the LLD driver when `reimer build` performs the final native link. Install the pinned Rust toolchain before building standalone executables. `reimer check` and JIT-based `reimer run` do not invoke Cargo.

## Windows

1. Install [Rust with rustup](https://rustup.rs/) using the MSVC toolchain.
2. Download the `x86_64-pc-windows-msvc.zip` archive from the matching GitHub Release.
3. Extract the complete archive to a stable location such as `%LOCALAPPDATA%\Programs\Reimer`.
4. Add that directory to your user `PATH`.

PowerShell example:

```powershell
$releaseName = "reimer-v0.1.1-x86_64-pc-windows-msvc"
$releaseRoot = Join-Path $env:LOCALAPPDATA "Programs\Reimer"
New-Item -ItemType Directory -Force -Path $releaseRoot | Out-Null
Expand-Archive ".\$releaseName.zip" -DestinationPath $releaseRoot
$installDirectory = Join-Path $releaseRoot $releaseName
```

Add the extracted directory containing `reimer.exe` to `PATH` through **System Properties → Environment Variables**, then open a new terminal.

## Linux and macOS

1. Install Rust with [rustup](https://rustup.rs/).
2. Download the archive matching the host printed in the release asset name.
3. Extract it to a stable directory and expose the executables through `PATH`.

```bash
mkdir -p "$HOME/.local/lib/reimer" "$HOME/.local/bin"
tar -xzf reimer-v0.1.1-x86_64-unknown-linux-gnu.tar.gz \
  --strip-components=1 \
  -C "$HOME/.local/lib/reimer"
ln -sf "$HOME/.local/lib/reimer/reimer" "$HOME/.local/bin/reimer"
ln -sf "$HOME/.local/lib/reimer/reimer-lsp" "$HOME/.local/bin/reimer-lsp"
ln -sf "$HOME/.local/lib/reimer/reimer-lint" "$HOME/.local/bin/reimer-lint"
```

Make sure `$HOME/.local/bin` is in `PATH`.

## Building from source

The repository pins its Rust version in `rust-toolchain.toml`.

```text
git clone https://github.com/Scr44gr/reimer.git
cd reimer
cargo install --path crates/reimer-cli --locked --force
```

This installs `reimer` into Cargo's binary directory. A source installation can find the standard library in the checkout. A binary distribution instead finds the sibling `std` directory. `REIMER_STD_PATH` can explicitly select another standard-library directory for development or packaging tests.

## Verify the installation

```text
reimer check examples/exit_42.reim
reimer run examples/exit_42.reim --release
reimer build examples/exit_42.reim --release -o answer
```

The first command should report that the file was checked. The second should report `program returned 42`. The final command creates a standalone native executable.

## Verify a downloaded archive

Every release asset has a neighboring `.sha256` file. Compare it before extracting:

```powershell
(Get-FileHash .\reimer-v0.1.1-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash
```

```bash
sha256sum -c reimer-v0.1.1-x86_64-unknown-linux-gnu.tar.gz.sha256
```
