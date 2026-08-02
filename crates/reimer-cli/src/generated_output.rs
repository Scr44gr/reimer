//! Filesystem guards for compiler-managed output paths.

use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub fn prepare_directory(root: &Path, directory: &Path) -> Result<PathBuf, String> {
    let root = canonical_root(root)?;
    let directory = absolute_path(directory)?;
    let relative = directory.strip_prefix(&root).map_err(|_| {
        format!(
            "refusing to use generated path `{}` outside `{}`",
            directory.display(),
            root.display()
        )
    })?;
    let mut current = root;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(format!(
                "refusing to use generated path with non-normal components: `{}`",
                directory.display()
            ));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(_) => require_real_directory(&current)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| {
                    format!("failed to create `{}`: {error}", current.display())
                })?;
                require_real_directory(&current)?;
            }
            Err(error) => {
                return Err(format!(
                    "failed to inspect generated directory `{}`: {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(current)
}

pub fn prepare_file(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let path = absolute_path(path)?;
    let parent = path.parent().ok_or_else(|| {
        format!(
            "generated output `{}` has no parent directory",
            path.display()
        )
    })?;
    prepare_directory(root, parent)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(format!(
                    "refusing to replace linked or non-file generated output `{}`",
                    path.display()
                ));
            }
            let resolved = canonical_path(&path, "resolve generated output")?;
            if resolved != path {
                return Err(format!(
                    "refusing to replace linked generated output `{}` (resolved to `{}`)",
                    path.display(),
                    resolved.display()
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to inspect generated output `{}`: {error}",
                path.display()
            ));
        }
    }
    Ok(path)
}

pub fn write(root: &Path, path: &Path, contents: &[u8]) -> Result<(), String> {
    let path = prepare_file(root, path)?;
    fs::write(&path, contents)
        .map_err(|error| format!("failed to write `{}`: {error}", path.display()))
}

pub fn write_if_changed(root: &Path, path: &Path, contents: &[u8]) -> Result<(), String> {
    let path = prepare_file(root, path)?;
    match fs::read(&path) {
        Ok(existing) if existing == contents => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("failed to read `{}`: {error}", path.display())),
    }
    fs::write(&path, contents)
        .map_err(|error| format!("failed to write `{}`: {error}", path.display()))
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    let root = absolute_path(root)?;
    canonical_path(&root, "resolve generated-output root")
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|error| format!("failed to resolve current directory: {error}"))
}

fn require_real_directory(directory: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| {
        format!(
            "failed to inspect generated directory `{}`: {error}",
            directory.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "refusing to use linked or non-directory generated path `{}`",
            directory.display()
        ));
    }
    let resolved = canonical_path(directory, "resolve generated directory")?;
    if resolved != directory {
        return Err(format!(
            "refusing to use linked generated directory `{}` (resolved to `{}`)",
            directory.display(),
            resolved.display()
        ));
    }
    Ok(())
}

fn canonical_path(path: &Path, operation: &str) -> Result<PathBuf, String> {
    fs::canonicalize(path)
        .map(ordinary_path)
        .map_err(|error| format!("failed to {operation} `{}`: {error}", path.display()))
}

#[cfg(windows)]
fn ordinary_path(path: PathBuf) -> PathBuf {
    use std::path::Prefix;

    let mut components = path.components();
    match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(drive) => {
                let mut ordinary = PathBuf::from(format!("{}:\\", char::from(drive)));
                ordinary.extend(components);
                ordinary
            }
            Prefix::VerbatimUNC(server, share) => {
                let mut ordinary = PathBuf::from(r"\\");
                ordinary.push(server);
                ordinary.push(share);
                ordinary.extend(components);
                ordinary
            }
            _ => path,
        },
        _ => path,
    }
}

#[cfg(not(windows))]
const fn ordinary_path(path: PathBuf) -> PathBuf {
    path
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::write;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn generated_write_should_reject_a_linked_destination() {
        let unique = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "reimer-generated-output-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("fixture root should be created");
        let sentinel = root.join("sentinel");
        fs::write(&sentinel, "unchanged").expect("sentinel should be written");
        let output = root.join("target/reimer/doc/package.md");
        fs::create_dir_all(output.parent().expect("output should have a parent"))
            .expect("output directory should be created");
        symlink(&sentinel, &output).expect("linked output should be created");

        let error = write(&root, &output, b"replaced")
            .expect_err("linked generated output must be rejected");

        assert!(error.contains("refusing to replace linked"));
        assert_eq!(
            fs::read_to_string(&sentinel).expect("sentinel should remain readable"),
            "unchanged"
        );
        fs::remove_dir_all(&root).expect("fixture should be removed");
    }
}
