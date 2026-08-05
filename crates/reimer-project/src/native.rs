//! Declarative native-library inputs for host-native Reimer projects.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use super::{MANIFEST_FILE, ProjectError, ResolvedPackage, ordinary_path};

/// Host-native files contributed by the resolved package graph.
///
/// Manifest paths are resolved and validated before this value is exposed, so
/// callers never need to interpret package-relative paths themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeDependencies {
    library_paths: Vec<PathBuf>,
    link_libraries: Vec<String>,
    runtime_files: Vec<PathBuf>,
}

impl NativeDependencies {
    /// Directories searched for native import and shared libraries.
    #[must_use]
    pub fn library_paths(&self) -> &[PathBuf] {
        &self.library_paths
    }

    /// Native libraries linked into generated executables.
    #[must_use]
    pub fn link_libraries(&self) -> &[String] {
        &self.link_libraries
    }

    /// Shared libraries copied beside generated executables.
    #[must_use]
    pub fn runtime_files(&self) -> &[PathBuf] {
        &self.runtime_files
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeTargetTable {
    #[serde(default, rename = "library-paths")]
    library_paths: Vec<PathBuf>,
    #[serde(default, rename = "link-libraries")]
    link_libraries: Vec<String>,
    #[serde(default, rename = "runtime-files")]
    runtime_files: Vec<PathBuf>,
}

#[derive(Debug)]
pub(super) struct ParsedNativeTarget {
    platform: NativePlatform,
    library_paths: Vec<PathBuf>,
    link_libraries: Vec<String>,
    runtime_files: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedNativeTarget {
    platform: NativePlatform,
    library_paths: Vec<PathBuf>,
    link_libraries: Vec<String>,
    runtime_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct NativePlatform {
    pub(super) operating_system: NativeOperatingSystem,
    architecture: NativeArchitecture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum NativeOperatingSystem {
    Windows,
    Linux,
    MacOs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NativeArchitecture {
    X86_64,
    Aarch64,
}

impl NativePlatform {
    fn parse(manifest: &Path, value: &str) -> Result<Self, ProjectError> {
        let Some((operating_system, architecture)) = value.rsplit_once('-') else {
            return Err(invalid_native_platform(manifest, value));
        };
        let operating_system = match operating_system {
            "windows" => NativeOperatingSystem::Windows,
            "linux" => NativeOperatingSystem::Linux,
            "macos" => NativeOperatingSystem::MacOs,
            _ => return Err(invalid_native_platform(manifest, value)),
        };
        let architecture = match architecture {
            "x86_64" => NativeArchitecture::X86_64,
            "aarch64" => NativeArchitecture::Aarch64,
            _ => return Err(invalid_native_platform(manifest, value)),
        };
        Ok(Self {
            operating_system,
            architecture,
        })
    }
}

impl std::fmt::Display for NativePlatform {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let operating_system = match self.operating_system {
            NativeOperatingSystem::Windows => "windows",
            NativeOperatingSystem::Linux => "linux",
            NativeOperatingSystem::MacOs => "macos",
        };
        let architecture = match self.architecture {
            NativeArchitecture::X86_64 => "x86_64",
            NativeArchitecture::Aarch64 => "aarch64",
        };
        write!(formatter, "{operating_system}-{architecture}")
    }
}

pub(super) fn parse_targets(
    manifest: &Path,
    targets: BTreeMap<String, NativeTargetTable>,
) -> Result<Vec<ParsedNativeTarget>, ProjectError> {
    targets
        .into_iter()
        .map(|(platform, table)| parse_target(manifest, &platform, table))
        .collect()
}

fn parse_target(
    manifest: &Path,
    platform: &str,
    table: NativeTargetTable,
) -> Result<ParsedNativeTarget, ProjectError> {
    let platform = NativePlatform::parse(manifest, platform)?;
    validate_unique_manifest_values(
        manifest,
        platform,
        "library path",
        table.library_paths.iter(),
    )?;
    validate_unique_link_libraries(manifest, platform, &table.link_libraries)?;
    validate_unique_manifest_values(
        manifest,
        platform,
        "runtime file",
        table.runtime_files.iter(),
    )?;
    for path in table.library_paths.iter().chain(&table.runtime_files) {
        validate_relative_path(manifest, platform, path)?;
    }
    for library in &table.link_libraries {
        validate_link_library(manifest, platform, library)?;
    }
    for file in &table.runtime_files {
        validate_runtime_file_extension(manifest, platform, file)?;
    }
    validate_unique_runtime_destinations(manifest, platform, &table.runtime_files)?;
    Ok(ParsedNativeTarget {
        platform,
        library_paths: table.library_paths,
        link_libraries: table.link_libraries,
        runtime_files: table.runtime_files,
    })
}

fn validate_unique_runtime_destinations(
    manifest: &Path,
    platform: NativePlatform,
    files: &[PathBuf],
) -> Result<(), ProjectError> {
    let mut destinations = BTreeMap::new();
    for file in files {
        let name = file.file_name().and_then(OsStr::to_str).unwrap_or_default();
        let key = output_key(platform, name);
        if let Some(existing) = destinations.insert(key, file) {
            return Err(ProjectError::InvalidManifest {
                path: manifest.to_path_buf(),
                message: format!(
                    "native target `{platform}` runtime files `{}` and `{}` use the same output name",
                    existing.display(),
                    file.display()
                ),
            });
        }
    }
    Ok(())
}

fn validate_unique_link_libraries(
    manifest: &Path,
    platform: NativePlatform,
    libraries: &[String],
) -> Result<(), ProjectError> {
    let mut unique = BTreeSet::new();
    for library in libraries {
        let key = output_key(platform, library);
        if !unique.insert(key) {
            return Err(ProjectError::InvalidManifest {
                path: manifest.to_path_buf(),
                message: format!(
                    "native target `{platform}` contains duplicate link library `{library}`"
                ),
            });
        }
    }
    Ok(())
}

fn validate_unique_manifest_values<'a, T>(
    manifest: &Path,
    platform: NativePlatform,
    kind: &str,
    values: impl Iterator<Item = &'a T>,
) -> Result<(), ProjectError>
where
    T: Ord + std::fmt::Debug + 'a,
{
    let mut unique = BTreeSet::new();
    for value in values {
        if !unique.insert(value) {
            return Err(ProjectError::InvalidManifest {
                path: manifest.to_path_buf(),
                message: format!("native target `{platform}` contains duplicate {kind} {value:?}"),
            });
        }
    }
    Ok(())
}

fn validate_relative_path(
    manifest: &Path,
    platform: NativePlatform,
    path: &Path,
) -> Result<(), ProjectError> {
    let valid = !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if valid {
        Ok(())
    } else {
        Err(ProjectError::InvalidManifest {
            path: manifest.to_path_buf(),
            message: format!(
                "native target `{platform}` path `{}` must be relative and cannot traverse parent directories",
                path.display()
            ),
        })
    }
}

fn validate_link_library(
    manifest: &Path,
    platform: NativePlatform,
    library: &str,
) -> Result<(), ProjectError> {
    let valid = !library.is_empty()
        && library.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '+')
        });
    if valid {
        Ok(())
    } else {
        Err(ProjectError::InvalidManifest {
            path: manifest.to_path_buf(),
            message: format!(
                "native target `{platform}` link library `{library}` must be a library name without path separators or linker options"
            ),
        })
    }
}

fn validate_runtime_file_extension(
    manifest: &Path,
    platform: NativePlatform,
    path: &Path,
) -> Result<(), ProjectError> {
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = Path::new(&name).extension().and_then(OsStr::to_str);
    let valid = match platform.operating_system {
        NativeOperatingSystem::Windows => extension.is_some_and(|value| value == "dll"),
        NativeOperatingSystem::Linux => {
            extension.is_some_and(|value| value == "so") || name.contains(".so.")
        }
        NativeOperatingSystem::MacOs => extension.is_some_and(|value| value == "dylib"),
    };
    if valid {
        Ok(())
    } else {
        Err(ProjectError::InvalidManifest {
            path: manifest.to_path_buf(),
            message: format!(
                "native target `{platform}` runtime file `{}` does not use the platform shared-library extension",
                path.display()
            ),
        })
    }
}

pub(super) fn resolve_targets(
    manifest: &Path,
    directory: &Path,
    targets: &[ParsedNativeTarget],
) -> Result<Vec<ResolvedNativeTarget>, ProjectError> {
    targets
        .iter()
        .map(|target| {
            let library_paths = target
                .library_paths
                .iter()
                .map(|path| resolve_path(manifest, directory, target.platform, path, true))
                .collect::<Result<Vec<_>, _>>()?;
            let runtime_files = target
                .runtime_files
                .iter()
                .map(|path| resolve_path(manifest, directory, target.platform, path, false))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ResolvedNativeTarget {
                platform: target.platform,
                library_paths,
                link_libraries: target.link_libraries.clone(),
                runtime_files,
            })
        })
        .collect()
}

fn resolve_path(
    manifest: &Path,
    directory: &Path,
    platform: NativePlatform,
    relative: &Path,
    expect_directory: bool,
) -> Result<PathBuf, ProjectError> {
    let candidate = directory.join(relative);
    let metadata = fs::symlink_metadata(&candidate).map_err(|error| {
        invalid_path(
            manifest,
            platform,
            relative,
            format!("cannot be inspected: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(invalid_path(
            manifest,
            platform,
            relative,
            "cannot be a symbolic link",
        ));
    }
    let expected_kind = if expect_directory {
        "directory"
    } else {
        "file"
    };
    let kind_matches = if expect_directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !kind_matches {
        return Err(invalid_path(
            manifest,
            platform,
            relative,
            format!("must be a {expected_kind}"),
        ));
    }
    let resolved = fs::canonicalize(&candidate)
        .map(ordinary_path)
        .map_err(|error| {
            invalid_path(
                manifest,
                platform,
                relative,
                format!("cannot be resolved: {error}"),
            )
        })?;
    if !resolved.starts_with(directory) {
        return Err(invalid_path(
            manifest,
            platform,
            relative,
            "resolves outside its package",
        ));
    }
    Ok(resolved)
}

fn invalid_path(
    manifest: &Path,
    platform: NativePlatform,
    relative: &Path,
    reason: impl std::fmt::Display,
) -> ProjectError {
    ProjectError::InvalidManifest {
        path: manifest.to_path_buf(),
        message: format!(
            "native target `{platform}` path `{}` {reason}",
            relative.display()
        ),
    }
}

pub(super) fn collect_checksum_files(
    manifest: &Path,
    targets: &[ResolvedNativeTarget],
) -> Result<Vec<PathBuf>, ProjectError> {
    let mut files = BTreeSet::new();
    for target in targets {
        for directory in &target.library_paths {
            collect_files(manifest, target.platform, directory, &mut files)?;
        }
        files.extend(target.runtime_files.iter().cloned());
    }
    Ok(files.into_iter().collect())
}

fn collect_files(
    manifest: &Path,
    platform: NativePlatform,
    root: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<(), ProjectError> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| ProjectError::InvalidManifest {
            path: manifest.to_path_buf(),
            message: format!(
                "native target `{platform}` directory `{}` cannot be read: {error}",
                directory.display()
            ),
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| ProjectError::InvalidManifest {
                path: manifest.to_path_buf(),
                message: format!(
                    "native target `{platform}` directory `{}` cannot be read: {error}",
                    directory.display()
                ),
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| ProjectError::InvalidManifest {
                    path: manifest.to_path_buf(),
                    message: format!(
                        "native target `{platform}` path `{}` cannot be inspected: {error}",
                        path.display()
                    ),
                })?;
            if file_type.is_symlink() {
                return Err(ProjectError::InvalidManifest {
                    path: manifest.to_path_buf(),
                    message: format!(
                        "native target `{platform}` directory `{}` cannot contain symbolic links",
                        root.display()
                    ),
                });
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                files.insert(path);
            } else {
                return Err(ProjectError::InvalidManifest {
                    path: manifest.to_path_buf(),
                    message: format!(
                        "native target `{platform}` path `{}` must be a regular file or directory",
                        path.display()
                    ),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn collect_dependencies(
    packages: &[ResolvedPackage],
) -> Result<NativeDependencies, ProjectError> {
    let mut library_paths = Vec::new();
    let mut seen_library_paths = BTreeSet::new();
    let mut link_libraries = Vec::new();
    let mut seen_link_libraries = BTreeSet::new();
    let mut runtime_providers: BTreeMap<(NativePlatform, String), (&ResolvedPackage, PathBuf)> =
        BTreeMap::new();
    let mut runtime_files = BTreeMap::new();
    let host = host_native_platform();
    for package in packages {
        for native in &package.native {
            for file in &native.runtime_files {
                let name = file.file_name().and_then(OsStr::to_str).ok_or_else(|| {
                    ProjectError::InvalidManifest {
                        path: package.directory.join(MANIFEST_FILE),
                        message: format!(
                            "native runtime file `{}` has no portable file name",
                            file.display()
                        ),
                    }
                })?;
                let output = output_key(native.platform, name);
                let key = (native.platform, output.clone());
                if let Some((provider, existing)) = runtime_providers.get(&key)
                    && existing != file
                {
                    return Err(ProjectError::InvalidManifest {
                        path: package.directory.join(MANIFEST_FILE),
                        message: format!(
                            "native runtime file `{name}` for `{}` conflicts between packages `{}` and `{}`",
                            native.platform, provider.name, package.name
                        ),
                    });
                }
                runtime_providers
                    .entry(key)
                    .or_insert_with(|| (package, file.clone()));
                if Some(native.platform) == host {
                    runtime_files.entry(output).or_insert_with(|| file.clone());
                }
            }
            if Some(native.platform) != host {
                continue;
            }
            for path in &native.library_paths {
                if seen_library_paths.insert(path.clone()) {
                    library_paths.push(path.clone());
                }
            }
            for library in &native.link_libraries {
                let key = output_key(native.platform, library);
                if seen_link_libraries.insert(key) {
                    link_libraries.push(library.clone());
                }
            }
        }
    }
    Ok(NativeDependencies {
        library_paths,
        link_libraries,
        runtime_files: runtime_files.into_values().collect(),
    })
}

fn output_key(platform: NativePlatform, name: &str) -> String {
    if platform.operating_system == NativeOperatingSystem::Windows {
        name.to_ascii_lowercase()
    } else {
        name.to_owned()
    }
}

fn invalid_native_platform(manifest: &Path, value: &str) -> ProjectError {
    ProjectError::InvalidManifest {
        path: manifest.to_path_buf(),
        message: format!(
            "native target `{value}` is unsupported; expected `<windows|linux|macos>-<x86_64|aarch64>`"
        ),
    }
}

pub(super) fn host_native_platform() -> Option<NativePlatform> {
    let operating_system = match env::consts::OS {
        "windows" => NativeOperatingSystem::Windows,
        "linux" => NativeOperatingSystem::Linux,
        "macos" => NativeOperatingSystem::MacOs,
        _ => return None,
    };
    let architecture = match env::consts::ARCH {
        "x86_64" => NativeArchitecture::X86_64,
        "aarch64" => NativeArchitecture::Aarch64,
        _ => return None,
    };
    Some(NativePlatform {
        operating_system,
        architecture,
    })
}
