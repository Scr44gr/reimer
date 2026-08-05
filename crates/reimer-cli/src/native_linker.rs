//! Host-native executable linking for generated Reimer objects.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use reimer_project::NativeDependencies;

use crate::generated_output;

const RUNTIME_ARCHIVE: &[u8] = include_bytes!(env!("REIMER_RUNTIME_ARCHIVE"));
const STARTUP_SOURCE: &str = include_str!("../native/startup.rs");
#[cfg(windows)]
const HOST_TARGET: &str = env!("REIMER_HOST_TARGET");

/// Links one generated object and the bundled runtime into a native executable.
///
/// # Errors
///
/// Returns an error when an intermediate artifact cannot be written, the Rust
/// linker driver cannot start, native linking fails, or a runtime library
/// cannot be staged safely.
pub fn link_executable(
    object: &[u8],
    executable: &Path,
    artifact_directory: &Path,
    generated_root: &Path,
    native: &NativeDependencies,
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

    let compiler = env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    #[cfg(windows)]
    let linker = Some(bundled_windows_linker(&compiler)?);
    // Unix needs the system driver to supply platform search paths and
    // libraries that a direct rust-lld invocation cannot discover by itself.
    #[cfg(not(windows))]
    let linker: Option<PathBuf> = None;
    let mut command = Command::new(&compiler);
    command
        .arg(&startup_path)
        .arg("--edition=2024")
        .arg("--extern")
        .arg(format!("reimer_runtime={}", runtime_path.display()))
        .arg("-C")
        .arg(format!("link-arg={}", object_path.display()));
    if let Some(linker) = linker {
        command
            .arg("-C")
            .arg(format!("linker={}", linker.display()));
    }
    for path in native.library_paths() {
        let mut argument = OsString::from("native=");
        argument.push(path);
        command.arg("-L").arg(argument);
    }
    for library in native.link_libraries() {
        command.arg("-l").arg(format!("dylib={library}"));
    }
    let output = command
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
    stage_runtime_files(
        native.runtime_files(),
        &executable,
        generated_root,
        generated_executable,
    )?;
    Ok(object_path)
}

fn stage_runtime_files(
    sources: &[PathBuf],
    executable: &Path,
    generated_root: &Path,
    generated_executable: bool,
) -> Result<(), String> {
    let directory = executable.parent().unwrap_or_else(|| Path::new("."));
    let mut destinations = BTreeMap::new();
    let mut staged = Vec::with_capacity(sources.len());
    for source in sources {
        let name = source.file_name().ok_or_else(|| {
            format!(
                "native runtime file `{}` has no destination file name",
                source.display()
            )
        })?;
        let destination = directory.join(name);
        if same_output_path(&destination, executable) {
            return Err(format!(
                "native runtime file `{}` conflicts with executable `{}`",
                source.display(),
                executable.display()
            ));
        }
        let key = runtime_output_key(name);
        if let Some(existing) = destinations.insert(key, source)
            && existing != source
        {
            return Err(format!(
                "native runtime files `{}` and `{}` conflict at `{}`",
                existing.display(),
                source.display(),
                destination.display()
            ));
        }
        staged.push((source, destination));
    }
    for (source, destination) in staged {
        let contents = fs::read(source).map_err(|error| {
            format!(
                "failed to read native runtime file `{}`: {error}",
                source.display()
            )
        })?;
        if generated_executable {
            generated_output::write_if_changed(generated_root, &destination, &contents)?;
        } else {
            write_runtime_file(&destination, &contents)?;
        }
    }
    Ok(())
}

fn runtime_output_key(name: &std::ffi::OsStr) -> String {
    let name = name.to_string_lossy();
    if cfg!(windows) {
        name.to_ascii_lowercase()
    } else {
        name.into_owned()
    }
}

fn write_runtime_file(destination: &Path, contents: &[u8]) -> Result<(), String> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(format!(
                "refusing to replace linked or non-file native runtime output `{}`",
                destination.display()
            ));
        }
        Ok(_) => {
            let existing = fs::read(destination).map_err(|error| {
                format!(
                    "failed to read native runtime output `{}`: {error}",
                    destination.display()
                )
            })?;
            if existing == contents {
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect native runtime output `{}`: {error}",
                destination.display()
            ));
        }
    }
    fs::write(destination, contents).map_err(|error| {
        format!(
            "failed to stage native runtime output `{}`: {error}",
            destination.display()
        )
    })
}

fn same_output_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.components()
            .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase())
            .eq(right
                .components()
                .map(|component| component.as_os_str().to_string_lossy().to_ascii_lowercase()))
    } else {
        left == right
    }
}

#[cfg(windows)]
fn bundled_windows_linker(compiler: &OsString) -> Result<PathBuf, String> {
    let output = Command::new(compiler)
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
    Ok(linker)
}

const fn object_extension() -> &'static str {
    if cfg!(windows) { "obj" } else { "o" }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::stage_runtime_files;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let unique = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "reimer-native-linker-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("fixture root should be created");
            Self { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn stage_runtime_files_should_copy_libraries_beside_the_executable() {
        let fixture = Fixture::new();
        let source = fixture.root.join("vendor/native.dll");
        let output = fixture.root.join("target/game.exe");
        fs::create_dir_all(source.parent().expect("source should have a parent"))
            .expect("source directory should be created");
        fs::create_dir_all(output.parent().expect("output should have a parent"))
            .expect("output directory should be created");
        fs::write(&source, b"native").expect("native fixture should be written");

        stage_runtime_files(&[source], &output, &fixture.root, true)
            .expect("runtime file should be staged");

        assert_eq!(
            fs::read(fixture.root.join("target/native.dll"))
                .expect("staged runtime should be readable"),
            b"native"
        );
    }

    #[test]
    fn stage_runtime_files_should_reject_an_executable_name_collision() {
        let fixture = Fixture::new();
        let source = fixture.root.join("vendor/game.exe");
        let output = fixture.root.join("target/game.exe");
        fs::create_dir_all(source.parent().expect("source should have a parent"))
            .expect("source directory should be created");
        fs::create_dir_all(output.parent().expect("output should have a parent"))
            .expect("output directory should be created");
        fs::write(&source, b"native").expect("native fixture should be written");
        let expected = output
            .parent()
            .expect("output should have a parent")
            .join(source.file_name().expect("source should have a file name"));
        assert!(
            super::same_output_path(&expected, &output),
            "expected {expected:?} to equal {output:?}"
        );

        let error = stage_runtime_files(&[source], &output, &fixture.root, true)
            .expect_err("runtime file cannot replace the executable");

        assert!(error.contains("conflicts with executable"));
    }

    #[test]
    fn stage_runtime_files_should_reject_conflicting_custom_output_names() {
        let fixture = Fixture::new();
        let first = fixture.root.join("first/native.dll");
        let second = fixture.root.join("second/native.dll");
        let output = fixture.root.join("custom/game.exe");
        for (source, contents) in [
            (&first, b"first".as_slice()),
            (&second, b"second".as_slice()),
        ] {
            fs::create_dir_all(source.parent().expect("source should have a parent"))
                .expect("source directory should be created");
            fs::write(source, contents).expect("native fixture should be written");
        }
        fs::create_dir_all(output.parent().expect("output should have a parent"))
            .expect("output directory should be created");

        let error = stage_runtime_files(&[first, second], &output, &fixture.root, false)
            .expect_err("custom output cannot collapse distinct runtime files");

        assert!(error.contains("conflict at"));
        assert!(!fixture.root.join("custom/native.dll").exists());
    }
}
