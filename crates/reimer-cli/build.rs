use std::env;
use std::error::Error;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=../reimer-runtime/src");
    println!("cargo:rerun-if-changed=native/startup.rs");

    let manifest_directory = required_path("CARGO_MANIFEST_DIR")?;
    let output_directory = required_path("OUT_DIR")?;
    let compiler = required_value("RUSTC")?;
    let target = required_value("TARGET")?;
    let optimization = required_value("OPT_LEVEL")?;
    let mut optimization_flag = OsString::from("opt-level=");
    optimization_flag.push(optimization);
    let runtime_source = manifest_directory.join("../reimer-runtime/src/lib.rs");
    let runtime_archive = output_directory.join("libreimer_runtime.rlib");
    let sysroot_output = Command::new(&compiler)
        .args(["--print", "sysroot"])
        .output()?;
    if !sysroot_output.status.success() {
        return Err(io::Error::other(format!(
            "failed to locate the Rust sysroot:\n{}",
            String::from_utf8_lossy(&sysroot_output.stderr)
        ))
        .into());
    }
    let sysroot = PathBuf::from(String::from_utf8(sysroot_output.stdout)?.trim());
    let linker = sysroot
        .join("lib")
        .join("rustlib")
        .join(&target)
        .join("bin")
        .join(format!("rust-lld{}", env::consts::EXE_SUFFIX));
    if !linker.is_file() {
        return Err(io::Error::other(format!(
            "bundled native linker was not found at `{}`",
            linker.display()
        ))
        .into());
    }

    let output = Command::new(&compiler)
        .arg(&runtime_source)
        .args([
            "--crate-name",
            "reimer_runtime",
            "--crate-type",
            "rlib",
            "--edition",
            "2024",
            "--target",
        ])
        .arg(&target)
        .arg("-C")
        .arg(optimization_flag)
        .args(["-C", "debuginfo=0"])
        .arg("-o")
        .arg(&runtime_archive)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "failed to bundle the native runtime:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }

    println!(
        "cargo:rustc-env=REIMER_RUNTIME_ARCHIVE={}",
        runtime_archive.display()
    );
    println!(
        "cargo:rustc-env=REIMER_RUST_COMPILER={}",
        PathBuf::from(compiler).display()
    );
    println!("cargo:rustc-env=REIMER_NATIVE_LINKER={}", linker.display());
    Ok(())
}

fn required_value(name: &str) -> Result<OsString, io::Error> {
    env::var_os(name).ok_or_else(|| io::Error::other(format!("missing build variable `{name}`")))
}

fn required_path(name: &str) -> Result<PathBuf, io::Error> {
    required_value(name).map(PathBuf::from)
}
