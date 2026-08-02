//! Generates documented Reimer bindings from pinned wgpu-native headers.

mod model;
mod parser;
mod render;

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use parser::parse_headers;
use render::{RenderedApi, render_api};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wgpu-native binding generation failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), GeneratorError> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    let [
        standard_header,
        native_header,
        types,
        constants,
        functions,
        coverage,
    ] = arguments.as_slice()
    else {
        return Err(GeneratorError::Usage);
    };
    let standard_header = Path::new(standard_header);
    let native_header = Path::new(native_header);
    let standard = read(standard_header)?;
    let native = read(native_header)?;
    let api = parse_headers(&standard, &native).map_err(GeneratorError::Parse)?;
    let RenderedApi {
        types: rendered_types,
        constants: rendered_constants,
        functions: rendered_functions,
        coverage: rendered_coverage,
    } = render_api(&api).map_err(GeneratorError::Render)?;
    write(Path::new(types), &rendered_types)?;
    write(Path::new(constants), &rendered_constants)?;
    write(Path::new(functions), &rendered_functions)?;
    write(Path::new(coverage), &rendered_coverage)?;
    Ok(())
}

fn read(path: &Path) -> Result<String, GeneratorError> {
    fs::read_to_string(path).map_err(|source| GeneratorError::Io {
        operation: "read",
        path: path.to_path_buf(),
        source,
    })
}

fn write(path: &Path, contents: &str) -> Result<(), GeneratorError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| GeneratorError::Io {
            operation: "create output directory for",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, contents).map_err(|source| GeneratorError::Io {
        operation: "write",
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug)]
enum GeneratorError {
    Usage,
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Parse(String),
    Render(String),
}

impl fmt::Display for GeneratorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => formatter.write_str(
                "usage: wgpu-api-gen <webgpu.h> <wgpu.h> <types.reim> <constants.reim> <functions.reim> <coverage.toml>",
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "cannot {operation} `{}`: {source}", path.display()),
            Self::Parse(message) => write!(formatter, "cannot parse headers: {message}"),
            Self::Render(message) => write!(formatter, "cannot render bindings: {message}"),
        }
    }
}

impl Error for GeneratorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Usage | Self::Parse(_) | Self::Render(_) => None,
        }
    }
}
