//! Host-native executable linking for generated Reimer objects.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::generated_output;

const RUNTIME_ARCHIVE: &[u8] = include_bytes!(env!("REIMER_RUNTIME_ARCHIVE"));
const STARTUP_SOURCE: &str = include_str!("../native/startup.rs");
const HOST_TARGET: &str = env!("REIMER_HOST_TARGET");

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
    generated_root: &Path,
    generated_executable: bool,
) -> Result<PathBuf, String> {
    let artifact_directory =
        generated_output::prepare_directory(generated_root, artifact_directory)?;
    let object_path = artifact_directory.join(format!("program.{}", object_extension()));
    let runtime_path = artifact_directory.join("libreimer_runtime.rlib");
    let startup_path = artifact_directory.join("startup.rs");
    generated_output::write_if_changed(generated_root, &object_path, object)?;
    generated_output::write_if_changed(generated_root, &runtime_path, RUNTIME_ARCHIVE)?;
    generated_output::write_if_changed(generated_root, &startup_path, STARTUP_SOURCE.as_bytes())?;
    let executable = if generated_executable {
        generated_output::prepare_file(generated_root, executable)?
    } else {
        if let Some(parent) = executable.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create `{}`: {error}", parent.display()))?;
        }
        executable.to_path_buf()
    };

    let (compiler, linker) = native_toolchain()?;
    let output = Command::new(&compiler)
        .arg(&startup_path)
        .arg("--edition=2024")
        .arg("--extern")
        .arg(format!("reimer_runtime={}", runtime_path.display()))
        .arg("-C")
        .arg(format!("link-arg={}", object_path.display()))
        .arg("-C")
        .arg(format!("linker={}", linker.display()))
        .arg("-o")
        .arg(&executable)
        .output()
        .map_err(|error| {
            format!(
                "failed to start Rust compiler `{}`: {error}",
                Path::new(&compiler).display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "native linking failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(object_path)
}

fn native_toolchain() -> Result<(OsString, PathBuf), String> {
    let compiler = env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let output = Command::new(&compiler)
        .args(["--print", "sysroot"])
        .output()
        .map_err(|error| {
            format!(
                "failed to locate Rust sysroot with `{}`: {error}",
                Path::new(&compiler).display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "failed to locate Rust sysroot with `{}`:\n{}",
            Path::new(&compiler).display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let sysroot = String::from_utf8(output.stdout)
        .map_err(|error| format!("Rust returned a non-UTF-8 sysroot path: {error}"))?;
    let linker = Path::new(sysroot.trim())
        .join("lib")
        .join("rustlib")
        .join(HOST_TARGET)
        .join("bin")
        .join(format!("rust-lld{}", env::consts::EXE_SUFFIX));
    if !linker.is_file() {
        return Err(format!(
            "native linker for target `{HOST_TARGET}` was not found at `{}`; install the matching Rust toolchain and target",
            linker.display()
        ));
    }
    Ok((compiler, linker))
}

const fn object_extension() -> &'static str {
    if cfg!(windows) { "obj" } else { "o" }
}
