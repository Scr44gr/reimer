//! Declarative project manifests, reproducible lockfiles, and dependency graphs.

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use reimer_package::{SourceDependency, SourceGraph, SourcePackage};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const MANIFEST_FILE: &str = "reimer.toml";
const LOCK_FILE: &str = "reimer.lock";
const LOCK_FORMAT: u32 = 1;
const EDITION: &str = "2026";

/// Controls how an existing lockfile participates in resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Reuses valid locked Git commits and updates stale lock data.
    Use,
    /// Requires the existing lockfile to describe the current source graph exactly.
    Locked,
    /// Resolves Git references again and replaces the lockfile.
    Refresh,
}

/// Compilation profile selected from the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildProfile {
    /// Fast compiler output intended for development.
    Debug,
    /// Optimized compiler output intended for distribution.
    Release,
}

/// A resolved project and its complete dependency graph.
#[derive(Debug, Clone)]
pub struct Project {
    manifest_path: PathBuf,
    lock_path: PathBuf,
    root: String,
    packages: Vec<ResolvedPackage>,
    debug_optimization: u8,
    release_optimization: u8,
}

impl Project {
    /// Resolves the project containing `start` and synchronizes its lockfile.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError`] for missing or invalid manifests, stale locked
    /// state, dependency cycles, filesystem failures, or Git failures.
    pub fn open(start: &Path, mode: LockMode) -> Result<Self, ProjectError> {
        let manifest_path = find_manifest(start)?;
        Resolver::new(&manifest_path, mode)?.resolve()
    }

    /// Returns the root manifest path.
    #[must_use]
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    /// Returns the root project directory.
    #[must_use]
    pub fn root_directory(&self) -> &Path {
        self.manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
    }

    /// Returns the root package name.
    #[must_use]
    pub fn package_name(&self) -> &str {
        self.root_package().map_or("", |package| &package.name)
    }

    /// Returns the root package version.
    #[must_use]
    pub fn package_version(&self) -> Option<&Version> {
        self.root_package().map(|package| &package.version)
    }

    /// Returns the selected profile's optimization level.
    #[must_use]
    pub const fn optimization(&self, profile: BuildProfile) -> u8 {
        match profile {
            BuildProfile::Debug => self.debug_optimization,
            BuildProfile::Release => self.release_optimization,
        }
    }

    /// Returns the default binary or library entry module.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::MissingEntry`] when neither `src/main.reim` nor
    /// `src/package.reim` exists.
    pub fn entry(&self) -> Result<PathBuf, ProjectError> {
        let root = self
            .root_package()
            .ok_or_else(|| ProjectError::InvalidGraph {
                message: "resolved graph does not contain its root package".to_owned(),
            })?;
        select_root_entry(&root.directory)
    }

    /// Finds integration test entry files in deterministic path order.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the `tests` tree cannot be read.
    pub fn test_entries(&self) -> Result<Vec<PathBuf>, ProjectError> {
        let tests = self.root_directory().join("tests");
        if !tests.exists() {
            return Ok(Vec::new());
        }
        collect_source_files(&tests)
    }

    /// Adapts this resolved dependency graph to the module loader.
    ///
    /// The root entry may be a normal package entry or an integration test.
    #[must_use]
    pub fn source_graph(&self, entry: &Path) -> SourceGraph {
        let packages = self
            .packages
            .iter()
            .map(|package| SourcePackage {
                id: package.id.clone(),
                name: package.name.clone(),
                source_root: package.directory.join("src"),
                entry: if package.id == self.root {
                    entry.to_path_buf()
                } else {
                    package.directory.join("src").join("package.reim")
                },
                dependencies: package
                    .dependencies
                    .iter()
                    .map(|dependency| SourceDependency {
                        alias: dependency.alias.clone(),
                        package: dependency.package.clone(),
                    })
                    .collect(),
            })
            .collect();
        SourceGraph {
            root: self.root.clone(),
            packages,
        }
    }

    /// Returns the synchronized lockfile path.
    #[must_use]
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    fn root_package(&self) -> Option<&ResolvedPackage> {
        self.packages.iter().find(|package| package.id == self.root)
    }
}

/// Failures produced while reading or resolving a project.
#[derive(Debug, Error)]
pub enum ProjectError {
    /// No manifest was found at or above the requested path.
    #[error("could not find `{MANIFEST_FILE}` from `{start}`")]
    ManifestNotFound {
        /// Path where the upward search began.
        start: PathBuf,
    },
    /// A filesystem operation failed.
    #[error("failed to {operation} `{path}`: {source}")]
    Io {
        /// Operation that failed.
        operation: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },
    /// TOML syntax or manifest semantics are invalid.
    #[error("invalid manifest `{path}`: {message}")]
    InvalidManifest {
        /// Manifest containing the error.
        path: PathBuf,
        /// Human-readable reason.
        message: String,
    },
    /// The initial registry syntax is recognized but deliberately unavailable.
    #[error(
        "registry dependency `{dependency}` is not supported yet; use a `path` or `git` source"
    )]
    RegistryUnavailable {
        /// Dependency alias from the manifest.
        dependency: String,
    },
    /// An existing lockfile is malformed.
    #[error("invalid lockfile `{path}`: {message}")]
    InvalidLock {
        /// Lockfile containing the error.
        path: PathBuf,
        /// Human-readable reason.
        message: String,
    },
    /// Locked mode detected source or manifest drift.
    #[error("lockfile is out of date: {message}")]
    LockOutOfDate {
        /// Difference that requires regeneration.
        message: String,
    },
    /// Package dependencies contain a cycle.
    #[error("package dependency cycle: {}", chain.join(" -> "))]
    DependencyCycle {
        /// Complete package-name chain ending at the repeated package.
        chain: Vec<String>,
    },
    /// A dependency did not satisfy its declared identity or version.
    #[error("dependency `{dependency}`: {message}")]
    DependencyMismatch {
        /// Dependency alias.
        dependency: String,
        /// Failed constraint.
        message: String,
    },
    /// Git could not resolve or materialize a dependency.
    #[error("Git operation `{operation}` failed: {message}")]
    Git {
        /// Git operation being performed.
        operation: &'static str,
        /// Captured diagnostic.
        message: String,
    },
    /// The root package has no compilable entry.
    #[error("package `{package}` needs `src/main.reim` or `src/package.reim`")]
    MissingEntry {
        /// Root package name.
        package: String,
    },
    /// A dependency has no public facade.
    #[error("dependency `{package}` needs `src/package.reim`")]
    MissingFacade {
        /// Dependency package name.
        package: String,
    },
    /// Internal graph metadata is inconsistent.
    #[error("invalid resolved package graph: {message}")]
    InvalidGraph {
        /// Consistency violation.
        message: String,
    },
}

#[derive(Debug, Clone)]
struct ResolvedPackage {
    id: String,
    name: String,
    version: Version,
    directory: PathBuf,
    source: String,
    checksum: String,
    dependencies: Vec<ResolvedDependency>,
}

#[derive(Debug, Clone)]
struct ResolvedDependency {
    alias: String,
    request: String,
    package: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestDocument {
    package: PackageTable,
    #[serde(default)]
    dependencies: BTreeMap<String, DependencyValue>,
    #[serde(default)]
    profile: ProfileTables,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageTable {
    name: String,
    version: String,
    edition: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DependencyValue {
    Registry(String),
    Detailed(DependencyTable),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DependencyTable {
    package: Option<String>,
    version: Option<String>,
    path: Option<PathBuf>,
    git: Option<String>,
    rev: Option<String>,
    branch: Option<String>,
    tag: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileTables {
    #[serde(default)]
    debug: ProfileTable,
    #[serde(default)]
    release: ProfileTable,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileTable {
    #[serde(default)]
    optimization: Option<u8>,
}

#[derive(Debug)]
struct ParsedManifest {
    path: PathBuf,
    name: String,
    version: Version,
    dependencies: Vec<DependencyRequest>,
    debug_optimization: u8,
    release_optimization: u8,
}

#[derive(Debug)]
struct DependencyRequest {
    alias: String,
    package: Option<String>,
    requirement: Option<VersionReq>,
    source: DependencySource,
}

impl DependencyRequest {
    fn description(&self) -> String {
        match &self.source {
            DependencySource::Path(path) => {
                format!("path+{}", normalized_path(path))
            }
            DependencySource::Git { repository, select } => {
                format!("git+{repository}?{}", select.description())
            }
        }
    }
}

#[derive(Debug)]
enum DependencySource {
    Path(PathBuf),
    Git {
        repository: String,
        select: GitSelect,
    },
}

#[derive(Debug)]
enum GitSelect {
    Head,
    Revision(String),
    Branch(String),
    Tag(String),
}

impl GitSelect {
    fn description(&self) -> String {
        match self {
            Self::Head => "head".to_owned(),
            Self::Revision(revision) => format!("rev={revision}"),
            Self::Branch(branch) => format!("branch={branch}"),
            Self::Tag(tag) => format!("tag={tag}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockDocument {
    format: u32,
    #[serde(default)]
    package: Vec<LockedPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockedPackage {
    id: String,
    name: String,
    version: String,
    source: String,
    checksum: String,
    #[serde(default)]
    dependencies: Vec<LockedDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockedDependency {
    alias: String,
    request: String,
    package: String,
}

struct Resolver {
    manifest_path: PathBuf,
    lock_path: PathBuf,
    cache_root: PathBuf,
    source_root: PathBuf,
    mode: LockMode,
    previous: Option<LockDocument>,
    packages: Vec<ResolvedPackage>,
    indices: HashMap<String, usize>,
    visiting: Vec<(String, String)>,
    debug_optimization: u8,
    release_optimization: u8,
}

impl Resolver {
    fn new(manifest_path: &Path, mode: LockMode) -> Result<Self, ProjectError> {
        let manifest_path = canonicalize(manifest_path, "canonicalize manifest")?;
        let directory = manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let lock_path = directory.join(LOCK_FILE);
        let previous = read_lock(&lock_path)?;
        if mode == LockMode::Locked && previous.is_none() {
            return Err(ProjectError::LockOutOfDate {
                message: format!("`{}` does not exist", lock_path.display()),
            });
        }
        Ok(Self {
            manifest_path,
            lock_path,
            cache_root: directory.join("target").join("reimer").join("dependencies"),
            source_root: directory,
            mode,
            previous,
            packages: Vec::new(),
            indices: HashMap::new(),
            visiting: Vec::new(),
            debug_optimization: 0,
            release_optimization: 3,
        })
    }

    fn resolve(mut self) -> Result<Project, ProjectError> {
        let manifest = parse_manifest(&self.manifest_path)?;
        self.debug_optimization = manifest.debug_optimization;
        self.release_optimization = manifest.release_optimization;
        let root = self.resolve_path(manifest, None)?;
        let current = self.lock_document();
        if self.mode == LockMode::Locked {
            if self.previous.as_ref() != Some(&current) {
                return Err(ProjectError::LockOutOfDate {
                    message: "manifest or source checksums differ from `reimer.lock`".to_owned(),
                });
            }
        } else if self.previous.as_ref() != Some(&current) {
            write_lock(&self.lock_path, &current)?;
        }
        Ok(Project {
            manifest_path: self.manifest_path,
            lock_path: self.lock_path,
            root,
            packages: self.packages,
            debug_optimization: self.debug_optimization,
            release_optimization: self.release_optimization,
        })
    }

    fn resolve_path(
        &mut self,
        manifest: ParsedManifest,
        expectation: Option<&DependencyRequest>,
    ) -> Result<String, ProjectError> {
        validate_expectation(&manifest, expectation)?;
        let directory = manifest
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        if expectation.is_some() {
            require_facade(&directory, &manifest.name)?;
        }
        let relative = relative_path(&self.source_root, &directory).ok_or_else(|| {
            ProjectError::InvalidManifest {
                path: manifest.path.clone(),
                message: format!(
                    "path package `{}` must be on the same filesystem as the root package",
                    directory.display()
                ),
            }
        })?;
        let source = format!("path+{}", normalized_path(&relative));
        self.resolve_package(manifest, directory, source)
    }

    fn resolve_git(
        &mut self,
        parent: &str,
        request: &DependencyRequest,
        repository: &str,
        select: &GitSelect,
    ) -> Result<String, ProjectError> {
        let description = request.description();
        let locked = if self.mode == LockMode::Refresh {
            None
        } else {
            self.locked_dependency(parent, &request.alias, &description)
        };
        let commit = if let Some(package) = locked {
            locked_git_commit(package, repository).ok_or_else(|| ProjectError::LockOutOfDate {
                message: format!(
                    "Git source for dependency `{}` no longer matches its manifest",
                    request.alias
                ),
            })?
        } else if self.mode == LockMode::Locked {
            return Err(ProjectError::LockOutOfDate {
                message: format!("dependency `{}` is not locked", request.alias),
            });
        } else {
            resolve_commit(repository, select)?
        };
        let directory = checkout(repository, &commit, &self.cache_root)?;
        let manifest_path = directory.join(MANIFEST_FILE);
        let manifest = parse_manifest(&manifest_path)?;
        validate_expectation(&manifest, Some(request))?;
        require_facade(&directory, &manifest.name)?;
        let source = format!("git+{repository}#{commit}");
        self.resolve_package(manifest, directory, source)
    }

    fn resolve_package(
        &mut self,
        manifest: ParsedManifest,
        directory: PathBuf,
        source: String,
    ) -> Result<String, ProjectError> {
        let id = package_id(&manifest.name, &manifest.version, &source);
        if let Some(start) = self
            .visiting
            .iter()
            .position(|(candidate, _)| candidate == &id)
        {
            let mut chain = self.visiting[start..]
                .iter()
                .map(|(_, name)| name.clone())
                .collect::<Vec<_>>();
            chain.push(manifest.name);
            return Err(ProjectError::DependencyCycle { chain });
        }
        if self.indices.contains_key(&id) {
            return Ok(id);
        }
        self.visiting.push((id.clone(), manifest.name.clone()));
        let checksum = source_checksum(&directory)?;
        let mut dependencies = Vec::with_capacity(manifest.dependencies.len());
        for request in &manifest.dependencies {
            let target = match &request.source {
                DependencySource::Path(path) => {
                    let manifest_path = dependency_manifest(&manifest.path, path);
                    let dependency = parse_manifest(&manifest_path)?;
                    self.resolve_path(dependency, Some(request))?
                }
                DependencySource::Git { repository, select } => {
                    self.resolve_git(&id, request, repository, select)?
                }
            };
            if self.mode == LockMode::Locked {
                self.validate_locked_edge(&id, request, &target)?;
            }
            dependencies.push(ResolvedDependency {
                alias: request.alias.clone(),
                request: request.description(),
                package: target,
            });
        }
        self.visiting.pop();
        let index = self.packages.len();
        self.indices.insert(id.clone(), index);
        self.packages.push(ResolvedPackage {
            id: id.clone(),
            name: manifest.name,
            version: manifest.version,
            directory,
            source,
            checksum,
            dependencies,
        });
        Ok(id)
    }

    fn validate_locked_edge(
        &self,
        parent: &str,
        request: &DependencyRequest,
        target: &str,
    ) -> Result<(), ProjectError> {
        let description = request.description();
        let Some(locked) = self.locked_dependency(parent, &request.alias, &description) else {
            return Err(ProjectError::LockOutOfDate {
                message: format!("dependency `{}` is not locked", request.alias),
            });
        };
        if locked.id != target {
            return Err(ProjectError::LockOutOfDate {
                message: format!(
                    "dependency `{}` resolved to a different package",
                    request.alias
                ),
            });
        }
        Ok(())
    }

    fn locked_dependency(
        &self,
        parent: &str,
        alias: &str,
        request: &str,
    ) -> Option<&LockedPackage> {
        let lock = self.previous.as_ref()?;
        let parent = lock.package.iter().find(|package| package.id == parent)?;
        let dependency = parent
            .dependencies
            .iter()
            .find(|dependency| dependency.alias == alias && dependency.request == request)?;
        lock.package
            .iter()
            .find(|package| package.id == dependency.package)
    }

    fn lock_document(&self) -> LockDocument {
        let mut package = self
            .packages
            .iter()
            .map(|resolved| {
                let mut dependencies = resolved
                    .dependencies
                    .iter()
                    .map(|dependency| LockedDependency {
                        alias: dependency.alias.clone(),
                        request: dependency.request.clone(),
                        package: dependency.package.clone(),
                    })
                    .collect::<Vec<_>>();
                dependencies.sort_by(|left, right| left.alias.cmp(&right.alias));
                LockedPackage {
                    id: resolved.id.clone(),
                    name: resolved.name.clone(),
                    version: resolved.version.to_string(),
                    source: resolved.source.clone(),
                    checksum: resolved.checksum.clone(),
                    dependencies,
                }
            })
            .collect::<Vec<_>>();
        package.sort_by(|left, right| left.id.cmp(&right.id));
        LockDocument {
            format: LOCK_FORMAT,
            package,
        }
    }
}

fn find_manifest(start: &Path) -> Result<PathBuf, ProjectError> {
    if start.file_name() == Some(OsStr::new(MANIFEST_FILE)) && start.is_file() {
        return Ok(start.to_path_buf());
    }
    let start = if start.is_file() {
        start.parent().unwrap_or_else(|| Path::new("."))
    } else {
        start
    };
    for directory in start.ancestors() {
        let candidate = directory.join(MANIFEST_FILE);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(ProjectError::ManifestNotFound {
        start: start.to_path_buf(),
    })
}

fn parse_manifest(path: &Path) -> Result<ParsedManifest, ProjectError> {
    let path = canonicalize(path, "canonicalize manifest")?;
    let text = read_to_string(&path, "read manifest")?;
    let document = toml::from_str::<ManifestDocument>(&text).map_err(|error| {
        ProjectError::InvalidManifest {
            path: path.clone(),
            message: error.to_string(),
        }
    })?;
    validate_package_name(&path, &document.package.name)?;
    let version = Version::parse(&document.package.version).map_err(|error| {
        ProjectError::InvalidManifest {
            path: path.clone(),
            message: format!("package version is not SemVer: {error}"),
        }
    })?;
    if document.package.edition != EDITION {
        return Err(ProjectError::InvalidManifest {
            path,
            message: format!("edition must be `{EDITION}`"),
        });
    }
    let dependencies = document
        .dependencies
        .into_iter()
        .map(|(alias, value)| parse_dependency(&path, alias, value))
        .collect::<Result<Vec<_>, _>>()?;
    let debug_optimization =
        validate_optimization(&path, "debug", document.profile.debug.optimization, 0)?;
    let release_optimization =
        validate_optimization(&path, "release", document.profile.release.optimization, 3)?;
    Ok(ParsedManifest {
        path,
        name: document.package.name,
        version,
        dependencies,
        debug_optimization,
        release_optimization,
    })
}

fn parse_dependency(
    manifest: &Path,
    alias: String,
    value: DependencyValue,
) -> Result<DependencyRequest, ProjectError> {
    validate_alias(manifest, &alias)?;
    let DependencyValue::Detailed(table) = value else {
        let DependencyValue::Registry(requirement) = value else {
            unreachable!();
        };
        VersionReq::parse(&requirement).map_err(|error| ProjectError::InvalidManifest {
            path: manifest.to_path_buf(),
            message: format!("dependency `{alias}` has an invalid version requirement: {error}"),
        })?;
        return Err(ProjectError::RegistryUnavailable { dependency: alias });
    };
    let requirement = table
        .version
        .as_deref()
        .map(VersionReq::parse)
        .transpose()
        .map_err(|error| ProjectError::InvalidManifest {
            path: manifest.to_path_buf(),
            message: format!("dependency `{alias}` has an invalid version requirement: {error}"),
        })?;
    let source = match (table.path, table.git) {
        (Some(path), None) => {
            if table.rev.is_some() || table.branch.is_some() || table.tag.is_some() {
                return Err(ProjectError::InvalidManifest {
                    path: manifest.to_path_buf(),
                    message: format!(
                        "path dependency `{alias}` cannot declare `rev`, `branch`, or `tag`"
                    ),
                });
            }
            DependencySource::Path(path)
        }
        (None, Some(repository)) => DependencySource::Git {
            repository,
            select: git_select(manifest, &alias, table.rev, table.branch, table.tag)?,
        },
        (Some(_), Some(_)) => {
            return Err(ProjectError::InvalidManifest {
                path: manifest.to_path_buf(),
                message: format!("dependency `{alias}` cannot use both `path` and `git`"),
            });
        }
        (None, None) => {
            return Err(ProjectError::InvalidManifest {
                path: manifest.to_path_buf(),
                message: format!("dependency `{alias}` needs a `path` or `git` source"),
            });
        }
    };
    Ok(DependencyRequest {
        alias,
        package: table.package,
        requirement,
        source,
    })
}

fn git_select(
    manifest: &Path,
    alias: &str,
    revision: Option<String>,
    branch: Option<String>,
    tag: Option<String>,
) -> Result<GitSelect, ProjectError> {
    let selectors = usize::from(revision.is_some())
        .saturating_add(usize::from(branch.is_some()))
        .saturating_add(usize::from(tag.is_some()));
    if selectors > 1 {
        return Err(ProjectError::InvalidManifest {
            path: manifest.to_path_buf(),
            message: format!("Git dependency `{alias}` can select only one of rev, branch, or tag"),
        });
    }
    Ok(match (revision, branch, tag) {
        (Some(revision), None, None) => GitSelect::Revision(revision),
        (None, Some(branch), None) => GitSelect::Branch(branch),
        (None, None, Some(tag)) => GitSelect::Tag(tag),
        (None, None, None) => GitSelect::Head,
        _ => unreachable!(),
    })
}

fn validate_package_name(path: &Path, name: &str) -> Result<(), ProjectError> {
    let mut characters = name.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic());
    let valid_rest = characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));
    if valid_start && valid_rest {
        Ok(())
    } else {
        Err(ProjectError::InvalidManifest {
            path: path.to_path_buf(),
            message: format!("invalid package name `{name}`"),
        })
    }
}

fn validate_alias(path: &Path, alias: &str) -> Result<(), ProjectError> {
    let mut characters = alias.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest =
        characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    if valid_start && valid_rest && !matches!(alias, "self" | "super" | "std") {
        Ok(())
    } else {
        Err(ProjectError::InvalidManifest {
            path: path.to_path_buf(),
            message: format!("dependency key `{alias}` is not a valid import alias"),
        })
    }
}

fn validate_optimization(
    path: &Path,
    profile: &str,
    value: Option<u8>,
    default: u8,
) -> Result<u8, ProjectError> {
    let value = value.unwrap_or(default);
    if value <= 3 {
        Ok(value)
    } else {
        Err(ProjectError::InvalidManifest {
            path: path.to_path_buf(),
            message: format!("profile `{profile}` optimization must be between 0 and 3"),
        })
    }
}

fn validate_expectation(
    manifest: &ParsedManifest,
    expectation: Option<&DependencyRequest>,
) -> Result<(), ProjectError> {
    let Some(expectation) = expectation else {
        return Ok(());
    };
    if let Some(package) = &expectation.package
        && package != &manifest.name
    {
        return Err(ProjectError::DependencyMismatch {
            dependency: expectation.alias.clone(),
            message: format!("expected package `{package}`, found `{}`", manifest.name),
        });
    }
    if let Some(requirement) = &expectation.requirement
        && !requirement.matches(&manifest.version)
    {
        return Err(ProjectError::DependencyMismatch {
            dependency: expectation.alias.clone(),
            message: format!(
                "version {} does not satisfy `{requirement}`",
                manifest.version
            ),
        });
    }
    Ok(())
}

fn dependency_manifest(parent: &Path, dependency: &Path) -> PathBuf {
    let directory = parent.parent().unwrap_or_else(|| Path::new("."));
    let path = directory.join(dependency);
    if path.file_name() == Some(OsStr::new(MANIFEST_FILE)) {
        path
    } else {
        path.join(MANIFEST_FILE)
    }
}

fn select_root_entry(directory: &Path) -> Result<PathBuf, ProjectError> {
    let main = directory.join("src").join("main.reim");
    if main.is_file() {
        return Ok(main);
    }
    let facade = directory.join("src").join("package.reim");
    if facade.is_file() {
        return Ok(facade);
    }
    let package = parse_manifest(&directory.join(MANIFEST_FILE))?;
    Err(ProjectError::MissingEntry {
        package: package.name,
    })
}

fn require_facade(directory: &Path, package: &str) -> Result<(), ProjectError> {
    if directory.join("src").join("package.reim").is_file() {
        Ok(())
    } else {
        Err(ProjectError::MissingFacade {
            package: package.to_owned(),
        })
    }
}

fn read_lock(path: &Path) -> Result<Option<LockDocument>, ProjectError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = read_to_string(path, "read lockfile")?;
    let lock =
        toml::from_str::<LockDocument>(&text).map_err(|error| ProjectError::InvalidLock {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    if lock.format != LOCK_FORMAT {
        return Err(ProjectError::InvalidLock {
            path: path.to_path_buf(),
            message: format!("unsupported lockfile format {}", lock.format),
        });
    }
    Ok(Some(lock))
}

fn write_lock(path: &Path, lock: &LockDocument) -> Result<(), ProjectError> {
    let mut text = toml::to_string_pretty(lock).map_err(|error| ProjectError::InvalidLock {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    text.push('\n');
    let temporary = path.with_extension(format!("lock.tmp-{}", std::process::id()));
    fs::write(&temporary, text).map_err(|source| ProjectError::Io {
        operation: "write temporary lockfile",
        path: temporary.clone(),
        source,
    })?;
    if path.exists() {
        fs::remove_file(path).map_err(|source| ProjectError::Io {
            operation: "replace lockfile",
            path: path.to_path_buf(),
            source,
        })?;
    }
    fs::rename(&temporary, path).map_err(|source| ProjectError::Io {
        operation: "install lockfile",
        path: path.to_path_buf(),
        source,
    })
}

fn source_checksum(directory: &Path) -> Result<String, ProjectError> {
    let manifest = directory.join(MANIFEST_FILE);
    let mut files = vec![manifest];
    let source = directory.join("src");
    if source.is_dir() {
        files.extend(collect_source_files(&source)?);
    }
    files.sort();
    let mut digest = Sha256::new();
    for path in files {
        let relative = path.strip_prefix(directory).unwrap_or(&path);
        digest.update(normalized_path(relative).as_bytes());
        digest.update([0]);
        let bytes = fs::read(&path).map_err(|source| ProjectError::Io {
            operation: "read package source",
            path: path.clone(),
            source,
        })?;
        digest.update(bytes);
        digest.update([0]);
    }
    Ok(format_digest(digest.finalize()))
}

fn collect_source_files(root: &Path) -> Result<Vec<PathBuf>, ProjectError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|source| ProjectError::Io {
            operation: "read source directory",
            path: directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| ProjectError::Io {
                operation: "read source directory entry",
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| ProjectError::Io {
                operation: "read source file type",
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file()
                && path.extension().and_then(OsStr::to_str) == Some("reim")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn package_id(name: &str, version: &Version, source: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(name.as_bytes());
    digest.update([0]);
    digest.update(version.to_string().as_bytes());
    digest.update([0]);
    digest.update(source.as_bytes());
    format_digest(digest.finalize())[..24].to_owned()
}

fn resolve_commit(repository: &str, select: &GitSelect) -> Result<String, ProjectError> {
    if let GitSelect::Revision(revision) = select
        && is_commit(revision)
    {
        return Ok(revision.to_ascii_lowercase());
    }
    let references = match select {
        GitSelect::Head => vec!["HEAD".to_owned()],
        GitSelect::Revision(revision) => vec![revision.clone()],
        GitSelect::Branch(branch) => vec![format!("refs/heads/{branch}")],
        GitSelect::Tag(tag) => vec![format!("refs/tags/{tag}^{{}}"), format!("refs/tags/{tag}")],
    };
    for reference in references {
        let output = git_output(
            None,
            [
                OsString::from("ls-remote"),
                OsString::from(repository),
                OsString::from(&reference),
            ],
            "resolve reference",
        )?;
        if let Some(commit) = output
            .split_whitespace()
            .next()
            .filter(|value| is_commit(value))
        {
            return Ok(commit.to_ascii_lowercase());
        }
    }
    Err(ProjectError::Git {
        operation: "resolve reference",
        message: format!(
            "repository `{repository}` has no reference `{}`",
            select.description()
        ),
    })
}

fn checkout(repository: &str, commit: &str, cache_root: &Path) -> Result<PathBuf, ProjectError> {
    fs::create_dir_all(cache_root).map_err(|source| ProjectError::Io {
        operation: "create dependency cache",
        path: cache_root.to_path_buf(),
        source,
    })?;
    let key = digest_text(&format!("{repository}#{commit}"));
    let directory = cache_root.join(&key[..24]);
    if directory.is_dir() {
        let current = git_output(
            Some(&directory),
            [OsString::from("rev-parse"), OsString::from("HEAD")],
            "inspect cached dependency",
        );
        if current
            .as_ref()
            .is_ok_and(|current| current.trim().eq_ignore_ascii_case(commit))
        {
            return Ok(directory);
        }
        fs::remove_dir_all(&directory).map_err(|source| ProjectError::Io {
            operation: "replace dependency cache",
            path: directory.clone(),
            source,
        })?;
    }
    git_output(
        None,
        [
            OsString::from("clone"),
            OsString::from("--no-checkout"),
            OsString::from(repository),
            directory.as_os_str().to_owned(),
        ],
        "clone dependency",
    )?;
    git_output(
        Some(&directory),
        [
            OsString::from("fetch"),
            OsString::from("--depth"),
            OsString::from("1"),
            OsString::from("origin"),
            OsString::from(commit),
        ],
        "fetch locked commit",
    )?;
    git_output(
        Some(&directory),
        [
            OsString::from("checkout"),
            OsString::from("--detach"),
            OsString::from(commit),
        ],
        "checkout locked commit",
    )?;
    Ok(directory)
}

fn git_output(
    directory: Option<&Path>,
    arguments: impl IntoIterator<Item = OsString>,
    operation: &'static str,
) -> Result<String, ProjectError> {
    let executable = env::var_os("GIT").unwrap_or_else(|| OsString::from("git"));
    let mut command = Command::new(executable);
    command.args(arguments);
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    let output = command.output().map_err(|error| ProjectError::Git {
        operation,
        message: error.to_string(),
    })?;
    decode_git_output(output, operation)
}

fn decode_git_output(output: Output, operation: &'static str) -> Result<String, ProjectError> {
    if output.status.success() {
        return String::from_utf8(output.stdout).map_err(|error| ProjectError::Git {
            operation,
            message: format!("Git returned non-UTF-8 output: {error}"),
        });
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(ProjectError::Git {
        operation,
        message: stderr.trim().to_owned(),
    })
}

fn locked_git_commit(package: &LockedPackage, repository: &str) -> Option<String> {
    let prefix = format!("git+{repository}#");
    let commit = package.source.strip_prefix(&prefix)?;
    is_commit(commit).then(|| commit.to_owned())
}

fn is_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest_text(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format_digest(digest.finalize())
}

fn format_digest(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;

    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn canonicalize(path: &Path, operation: &'static str) -> Result<PathBuf, ProjectError> {
    fs::canonicalize(path)
        .map(ordinary_path)
        .map_err(|source| ProjectError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })
}

fn read_to_string(path: &Path, operation: &'static str) -> Result<String, ProjectError> {
    fs::read_to_string(path).map_err(|source| ProjectError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    })
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let mut base_components = base.components().peekable();
    let mut target_components = target.components().peekable();
    while base_components.peek() == target_components.peek() && base_components.peek().is_some() {
        base_components.next();
        target_components.next();
    }
    if matches!(
        base_components.peek(),
        Some(Component::Prefix(_) | Component::RootDir)
    ) || matches!(
        target_components.peek(),
        Some(Component::Prefix(_) | Component::RootDir)
    ) {
        return None;
    }

    let mut relative = PathBuf::new();
    for component in base_components {
        match component {
            Component::Normal(_) | Component::ParentDir => relative.push(".."),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    for component in target_components {
        match component {
            Component::Normal(value) => relative.push(value),
            Component::ParentDir => relative.push(".."),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Some(relative)
}

#[cfg(windows)]
fn ordinary_path(path: PathBuf) -> PathBuf {
    let ordinary = {
        let display = path.to_string_lossy();
        display
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| PathBuf::from(format!(r"\\{rest}")))
            .or_else(|| display.strip_prefix(r"\\?\").map(PathBuf::from))
    };
    ordinary.unwrap_or(path)
}

#[cfg(not(windows))]
const fn ordinary_path(path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{BuildProfile, LockMode, Project, ProjectError, git_output, normalized_path};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let unique = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("project-{}-{unique}", std::process::id()));
            fs::create_dir_all(&root).expect("fixture directory should be created");
            Self { root }
        }

        fn write(&self, relative: &str, contents: &str) {
            let path = self.path(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("fixture parent should be created");
            }
            fs::write(path, contents).expect("fixture file should be written");
        }

        fn path(&self, relative: &str) -> PathBuf {
            let path = self.root.join(relative);
            assert!(path.starts_with(&self.root));
            path
        }

        fn commit(&self, relative: &str, message: &str) {
            let directory = self.path(relative);
            git_output(
                Some(&directory),
                ["add", "."].into_iter().map(Into::into),
                "stage test repository",
            )
            .expect("test files should be staged");
            git_output(
                Some(&directory),
                ["commit", "-m", message].into_iter().map(Into::into),
                "commit test repository",
            )
            .expect("test commit should be created");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn execute(project: &Project) -> i32 {
        let entry = project.entry().expect("project should have an entry");
        let graph = project.source_graph(&entry);
        let package = reimer_package::load_graph(&graph).expect("source graph should load");
        let program =
            reimer_resolver::resolve(&package.program).expect("source graph should resolve");
        reimer_codegen_native::execute(&program).expect("source graph should execute")
    }

    fn write_portable_graph(fixture: &Fixture) {
        fixture.write(
            "app/reimer.toml",
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2026"

[dependencies]
math = { path = "../math" }
"#,
        );
        fixture.write(
            "app/src/main.reim",
            "from math import answer; fn main() -> i32 { answer() }",
        );
        fixture.write(
            "math/reimer.toml",
            r#"[package]
name = "math"
version = "0.1.0"
edition = "2026"
"#,
        );
        fixture.write("math/src/package.reim", "pub fn answer() -> i32 { 42 }");
    }

    #[test]
    fn open_should_resolve_path_graph_and_write_a_deterministic_lockfile() {
        let fixture = Fixture::new();
        fixture.write(
            "app/reimer.toml",
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2026"

[dependencies]
physics = { path = "../physics", version = "^0.1" }

[profile.debug]
optimization = 1
"#,
        );
        fixture.write(
            "app/src/main.reim",
            "from physics import combined; fn main() -> i32 { combined() }",
        );
        fixture.write(
            "physics/reimer.toml",
            r#"[package]
name = "physics"
version = "0.1.2"
edition = "2026"

[dependencies]
vectors = { path = "../vectors" }
"#,
        );
        fixture.write(
            "physics/src/package.reim",
            "from vectors import answer; pub fn combined() -> i32 { answer() }",
        );
        fixture.write(
            "vectors/reimer.toml",
            r#"[package]
name = "vectors"
version = "0.2.0"
edition = "2026"
"#,
        );
        fixture.write("vectors/src/package.reim", "pub fn answer() -> i32 { 42 }");

        let project =
            Project::open(&fixture.path("app"), LockMode::Use).expect("project should resolve");
        let first_lock =
            fs::read_to_string(project.lock_path()).expect("lockfile should be readable");
        let repeated =
            Project::open(&fixture.path("app"), LockMode::Use).expect("project should resolve");
        let second_lock =
            fs::read_to_string(repeated.lock_path()).expect("lockfile should be readable");

        assert_eq!(execute(&project), 42);
        assert_eq!(project.optimization(BuildProfile::Debug), 1);
        assert_eq!(first_lock, second_lock);
    }

    #[test]
    fn lockfile_should_be_identical_after_the_source_tree_moves() {
        let first = Fixture::new();
        let second = Fixture::new();
        write_portable_graph(&first);
        write_portable_graph(&second);
        let first_project =
            Project::open(&first.path("app"), LockMode::Use).expect("first graph should resolve");
        let second_project =
            Project::open(&second.path("app"), LockMode::Use).expect("second graph should resolve");

        let first_lock =
            fs::read_to_string(first_project.lock_path()).expect("first lock should be readable");
        let second_lock =
            fs::read_to_string(second_project.lock_path()).expect("second lock should be readable");

        assert_eq!(first_lock, second_lock);
    }

    #[test]
    fn locked_mode_should_reject_changed_path_sources() {
        let fixture = Fixture::new();
        fixture.write(
            "app/reimer.toml",
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2026"

[dependencies]
physics = { path = "../physics" }
"#,
        );
        fixture.write(
            "app/src/main.reim",
            "from physics import answer; fn main() -> i32 { answer() }",
        );
        fixture.write(
            "physics/reimer.toml",
            r#"[package]
name = "physics"
version = "0.1.0"
edition = "2026"
"#,
        );
        fixture.write("physics/src/package.reim", "pub fn answer() -> i32 { 42 }");
        Project::open(&fixture.path("app"), LockMode::Use).expect("lockfile should be created");
        fixture.write("physics/src/package.reim", "pub fn answer() -> i32 { 43 }");

        let error = Project::open(&fixture.path("app"), LockMode::Locked)
            .expect_err("source drift should fail");

        assert!(matches!(error, ProjectError::LockOutOfDate { .. }));
    }

    #[test]
    fn open_should_report_the_complete_package_cycle() {
        let fixture = Fixture::new();
        fixture.write(
            "app/reimer.toml",
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2026"

[dependencies]
physics = { path = "../physics" }
"#,
        );
        fixture.write("app/src/main.reim", "fn main() -> i32 { 0 }");
        fixture.write("app/src/package.reim", "pub fn value() -> i32 { 0 }");
        fixture.write(
            "physics/reimer.toml",
            r#"[package]
name = "physics"
version = "0.1.0"
edition = "2026"

[dependencies]
app = { path = "../app" }
"#,
        );
        fixture.write("physics/src/package.reim", "pub fn value() -> i32 { 1 }");

        let error = Project::open(&fixture.path("app"), LockMode::Use)
            .expect_err("package cycle should fail");

        assert!(matches!(
            error,
            ProjectError::DependencyCycle { chain }
                if chain == ["app", "physics", "app"]
        ));
    }

    #[test]
    fn git_lock_should_pin_a_commit_until_refresh() {
        let fixture = Fixture::new();
        let repository = fixture.path("physics");
        fs::create_dir_all(&repository).expect("repository should be created");
        git_output(
            Some(&repository),
            ["init", "-b", "main"].into_iter().map(Into::into),
            "initialize test repository",
        )
        .expect("repository should initialize");
        git_output(
            Some(&repository),
            ["config", "user.email", "tests@example.invalid"]
                .into_iter()
                .map(Into::into),
            "configure test repository",
        )
        .expect("email should configure");
        git_output(
            Some(&repository),
            ["config", "user.name", "Project Tests"]
                .into_iter()
                .map(Into::into),
            "configure test repository",
        )
        .expect("name should configure");
        fixture.write(
            "physics/reimer.toml",
            r#"[package]
name = "physics"
version = "0.1.0"
edition = "2026"
"#,
        );
        fixture.write("physics/src/package.reim", "pub fn answer() -> i32 { 42 }");
        fixture.commit("physics", "initial");
        let repository = normalized_path(&repository);
        fixture.write(
            "app/reimer.toml",
            &format!(
                r#"[package]
name = "app"
version = "0.1.0"
edition = "2026"

[dependencies]
physics = {{ git = "{repository}", branch = "main" }}
"#
            ),
        );
        fixture.write(
            "app/src/main.reim",
            "from physics import answer; fn main() -> i32 { answer() }",
        );

        let pinned =
            Project::open(&fixture.path("app"), LockMode::Use).expect("Git project should resolve");
        fixture.write("physics/src/package.reim", "pub fn answer() -> i32 { 43 }");
        fixture.commit("physics", "change answer");
        let still_pinned =
            Project::open(&fixture.path("app"), LockMode::Use).expect("lock should be reused");
        let refreshed = Project::open(&fixture.path("app"), LockMode::Refresh)
            .expect("Git reference should refresh");

        assert_eq!(execute(&pinned), 42);
        assert_eq!(execute(&still_pinned), 42);
        assert_eq!(execute(&refreshed), 43);
    }

    #[test]
    fn test_entries_should_be_sorted_recursively() {
        let fixture = Fixture::new();
        fixture.write(
            "app/reimer.toml",
            r#"[package]
name = "app"
version = "0.1.0"
edition = "2026"
"#,
        );
        fixture.write("app/src/main.reim", "fn main() -> i32 { 0 }");
        fixture.write("app/tests/z.reim", "fn main() -> i32 { 0 }");
        fixture.write("app/tests/nested/a.reim", "fn main() -> i32 { 0 }");
        let project =
            Project::open(&fixture.path("app"), LockMode::Use).expect("project should resolve");

        let entries = project
            .test_entries()
            .expect("test entries should be discovered");

        assert_eq!(
            entries,
            [
                fixture.path("app/tests/nested/a.reim"),
                fixture.path("app/tests/z.reim"),
            ]
        );
    }

    #[test]
    fn fixture_paths_should_remain_inside_the_temporary_root() {
        let fixture = Fixture::new();

        let path = fixture.path("child");

        assert!(path.starts_with(Path::new(&fixture.root)));
    }
}
