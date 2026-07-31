//! Host-native executable linking for generated Reimer objects.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const RUNTIME_ARCHIVE: &[u8] = include_bytes!(env!("REIMER_RUNTIME_ARCHIVE"));
const STARTUP_SOURCE: &str = include_str!("../native/startup.rs");
const RUST_COMPILER: &str = env!("REIMER_RUST_COMPILER");
const NATIVE_LINKER: &str = env!("REIMER_NATIVE_LINKER");

/// Links one generated object and the bundled runtime into a native executable.
///
/// # Errors
///
/// Returns an error when an intermediate artifact cannot be written, the Rust
/// linker driver cannot start, or native linking fails.
pub fn link_executable(
    object: &[u8],
    executable: &Path,
    artifact_directory: &Path,
) -> Result<PathBuf, String> {
    fs::create_dir_all(artifact_directory).map_err(|error| {
        format!(
            "failed to create `{}`: {error}",
            artifact_directory.display()
        )
    })?;
    let object_path = artifact_directory.join(format!("program.{}", object_extension()));
    let runtime_path = artifact_directory.join("libreimer_runtime.rlib");
    let startup_path = artifact_directory.join("startup.rs");
    write_if_changed(&object_path, object)?;
    write_if_changed(&runtime_path, RUNTIME_ARCHIVE)?;
    write_if_changed(&startup_path, STARTUP_SOURCE.as_bytes())?;
    if let Some(parent) = executable.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create `{}`: {error}", parent.display()))?;
    }

    let output = Command::new(RUST_COMPILER)
        .arg(&startup_path)
        .arg("--edition=2024")
        .arg("--extern")
        .arg(format!("reimer_runtime={}", runtime_path.display()))
        .arg("-C")
        .arg(format!("link-arg={}", object_path.display()))
        .arg("-C")
        .arg(format!("linker={NATIVE_LINKER}"))
        .arg("-o")
        .arg(executable)
        .output()
        .map_err(|error| format!("failed to start native linker `{RUST_COMPILER}`: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "native linking failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(object_path)
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<(), String> {
    match fs::read(path) {
        Ok(existing) if existing == contents => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("failed to read `{}`: {error}", path.display())),
    }
    fs::write(path, contents)
        .map_err(|error| format!("failed to write `{}`: {error}", path.display()))
}

const fn object_extension() -> &'static str {
    if cfg!(windows) { "obj" } else { "o" }
}
