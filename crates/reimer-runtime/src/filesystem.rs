//! Native file-system ABI used by the safe `std::fs` wrappers.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// ABI symbol used to open an existing file for reading.
pub const FILE_OPEN_SYMBOL: &str = "file_open";
/// ABI symbol used to create or truncate a file for writing.
pub const FILE_CREATE_SYMBOL: &str = "file_create";
/// ABI symbol used to create or open a file for appending.
pub const FILE_APPEND_SYMBOL: &str = "file_append";
/// ABI symbol used to close an owned file handle.
pub const FILE_CLOSE_SYMBOL: &str = "file_close";
/// ABI symbol used to read at most one bounded byte region.
pub const FILE_READ_SYMBOL: &str = "file_read";
/// ABI symbol used to fill one bounded byte region.
pub const FILE_READ_EXACT_SYMBOL: &str = "file_read_exact";
/// ABI symbol used to write at most one bounded byte region.
pub const FILE_WRITE_SYMBOL: &str = "file_write";
/// ABI symbol used to write a complete bounded byte region.
pub const FILE_WRITE_ALL_SYMBOL: &str = "file_write_all";
/// ABI symbol used to flush buffered file data.
pub const FILE_FLUSH_SYMBOL: &str = "file_flush";
/// ABI symbol used to synchronize file content and metadata to storage.
pub const FILE_SYNC_ALL_SYMBOL: &str = "file_sync_all";
/// ABI symbol used to query unread bytes in a regular file.
pub const FILE_REMAINING_LEN_SYMBOL: &str = "file_remaining_len";
/// ABI symbol used to test whether a UTF-8 path exists.
pub const PATH_EXISTS_SYMBOL: &str = "path_exists";
/// ABI symbol used to remove one file.
pub const PATH_REMOVE_FILE_SYMBOL: &str = "path_remove_file";
/// ABI symbol used to rename one file-system path.
pub const PATH_RENAME_SYMBOL: &str = "path_rename";
/// ABI symbol used to atomically replace a destination file.
pub const PATH_REPLACE_FILE_SYMBOL: &str = "path_replace_file";
/// ABI symbol used to recursively create a directory path.
pub const PATH_CREATE_DIR_ALL_SYMBOL: &str = "path_create_dir_all";
/// ABI symbol used to query one regular file's size.
pub const PATH_FILE_SIZE_SYMBOL: &str = "path_file_size";
/// ABI symbol used to verify canonical path containment.
pub const PATH_IS_WITHIN_SYMBOL: &str = "path_is_within";
/// ABI symbol used to snapshot one canonical path.
pub const PATH_CANONICAL_OPEN_SYMBOL: &str = "path_canonical_open";
/// ABI symbol used to query a canonical-path snapshot's byte length.
pub const PATH_SNAPSHOT_LEN_SYMBOL: &str = "path_snapshot_len";
/// ABI symbol used to copy a canonical-path snapshot.
pub const PATH_SNAPSHOT_COPY_SYMBOL: &str = "path_snapshot_copy";
/// ABI symbol used to release a canonical-path snapshot.
pub const PATH_SNAPSHOT_CLOSE_SYMBOL: &str = "path_snapshot_close";

/// A read could not fill the requested destination before end-of-file.
pub const FILE_UNEXPECTED_EOF: isize = -2;
const FILE_OPERATION_FAILED: isize = -1;
const PATH_OPERATION_FAILED: isize = -1;
const PATH_NOT_UNICODE: isize = -2;
const PATH_INVALID_HANDLE: isize = -3;

static FILES: OnceLock<Mutex<HashMap<usize, File>>> = OnceLock::new();
static NEXT_FILE_HANDLE: AtomicUsize = AtomicUsize::new(1);
static PATH_SNAPSHOTS: OnceLock<Mutex<HashMap<usize, Vec<u8>>>> = OnceLock::new();
static NEXT_PATH_SNAPSHOT_HANDLE: AtomicUsize = AtomicUsize::new(1);

fn lock_files() -> std::sync::MutexGuard<'static, HashMap<usize, File>> {
    FILES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn store_file(file: File) -> usize {
    let mut files = lock_files();
    loop {
        let handle = NEXT_FILE_HANDLE.fetch_add(1, Ordering::Relaxed);
        if handle != 0 && !files.contains_key(&handle) {
            files.insert(handle, file);
            return handle;
        }
    }
}

fn lock_path_snapshots() -> std::sync::MutexGuard<'static, HashMap<usize, Vec<u8>>> {
    PATH_SNAPSHOTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn store_path_snapshot(path: &Path) -> isize {
    let Some(path) = path.to_str() else {
        return PATH_NOT_UNICODE;
    };
    let mut snapshots = lock_path_snapshots();
    loop {
        let handle = NEXT_PATH_SNAPSHOT_HANDLE.fetch_add(1, Ordering::Relaxed);
        let Ok(result) = isize::try_from(handle) else {
            continue;
        };
        if handle != 0 && !snapshots.contains_key(&handle) {
            snapshots.insert(handle, path.as_bytes().to_vec());
            return result;
        }
    }
}

unsafe fn with_utf8_path<Output>(
    data: *const u8,
    length: usize,
    action: impl FnOnce(&Path) -> Output,
) -> Option<Output> {
    if data.is_null() {
        return (length == 0).then(|| action(Path::new("")));
    }
    // SAFETY: The caller guarantees `length` readable bytes at `data`.
    let bytes = unsafe { std::slice::from_raw_parts(data, length) };
    std::str::from_utf8(bytes).ok().map(Path::new).map(action)
}

unsafe fn with_utf8_paths<Output>(
    source_data: *const u8,
    source_length: usize,
    destination_data: *const u8,
    destination_length: usize,
    action: impl FnOnce(&Path, &Path) -> Output,
) -> Option<Output> {
    // SAFETY: The caller guarantees both path buffers remain live for this call.
    unsafe {
        with_utf8_path(source_data, source_length, |source| {
            with_utf8_path(destination_data, destination_length, |destination| {
                action(source, destination)
            })
        })
    }
    .flatten()
}

fn open_path(path: &Path, operation: FileOperation) -> Option<File> {
    match operation {
        FileOperation::Open => File::open(path).ok(),
        FileOperation::Create => File::create(path).ok(),
        FileOperation::Append => OpenOptions::new().create(true).append(true).open(path).ok(),
    }
}

#[derive(Debug, Clone, Copy)]
enum FileOperation {
    Open,
    Create,
    Append,
}

unsafe fn open_file(data: *const u8, length: usize, operation: FileOperation) -> usize {
    // SAFETY: The caller supplies the path buffer covered by this ABI call.
    unsafe {
        with_utf8_path(data, length, |path| {
            open_path(path, operation).map_or(0, store_file)
        })
    }
    .unwrap_or(0)
}

/// Opens an existing file for reading and returns an owned nonzero handle.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// readable UTF-8 bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn file_open(data: *const u8, length: usize) -> usize {
    // SAFETY: The caller upholds the path-buffer contract documented above.
    unsafe { open_file(data, length, FileOperation::Open) }
}

/// Creates or truncates a file and returns an owned nonzero handle.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// readable UTF-8 bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn file_create(data: *const u8, length: usize) -> usize {
    // SAFETY: The caller upholds the path-buffer contract documented above.
    unsafe { open_file(data, length, FileOperation::Create) }
}

/// Creates or opens a file for appending and returns an owned nonzero handle.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// readable UTF-8 bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn file_append(data: *const u8, length: usize) -> usize {
    // SAFETY: The caller upholds the path-buffer contract documented above.
    unsafe { open_file(data, length, FileOperation::Append) }
}

/// Closes an owned file handle.
#[unsafe(no_mangle)]
pub extern "C" fn file_close(handle: usize) -> i32 {
    i32::from(lock_files().remove(&handle).is_none())
}

/// Reads at most `length` bytes into a live destination.
///
/// Returns the initialized byte count, or `-1` on failure.
///
/// # Safety
///
/// When `length` is nonzero, `destination` must point to `length` live,
/// writable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn file_read(handle: usize, destination: *mut u8, length: usize) -> isize {
    if length == 0 {
        return 0;
    }
    if destination.is_null() {
        return FILE_OPERATION_FAILED;
    }
    // SAFETY: The caller guarantees a live writable destination of `length` bytes.
    let destination = unsafe { std::slice::from_raw_parts_mut(destination, length) };
    let mut files = lock_files();
    let Some(file) = files.get_mut(&handle) else {
        return FILE_OPERATION_FAILED;
    };
    file.read(destination)
        .ok()
        .and_then(|read| isize::try_from(read).ok())
        .unwrap_or(FILE_OPERATION_FAILED)
}

/// Reads exactly `length` bytes into a live destination.
///
/// Returns the initialized byte count, `-2` on early end-of-file, or `-1` on
/// any other failure.
///
/// # Safety
///
/// When `length` is nonzero, `destination` must point to `length` live,
/// writable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn file_read_exact(
    handle: usize,
    destination: *mut u8,
    length: usize,
) -> isize {
    if length == 0 {
        return 0;
    }
    if destination.is_null() {
        return FILE_OPERATION_FAILED;
    }
    // SAFETY: The caller guarantees a live writable destination of `length` bytes.
    let destination = unsafe { std::slice::from_raw_parts_mut(destination, length) };
    let mut files = lock_files();
    let Some(file) = files.get_mut(&handle) else {
        return FILE_OPERATION_FAILED;
    };
    match file.read_exact(destination) {
        Ok(()) => isize::try_from(length).unwrap_or(FILE_OPERATION_FAILED),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => FILE_UNEXPECTED_EOF,
        Err(_) => FILE_OPERATION_FAILED,
    }
}

/// Writes at most `length` readable bytes.
///
/// Returns the written byte count, or `-1` on failure.
///
/// # Safety
///
/// When `length` is nonzero, `source` must point to `length` live, readable
/// bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn file_write(handle: usize, source: *const u8, length: usize) -> isize {
    if length == 0 {
        return 0;
    }
    if source.is_null() {
        return FILE_OPERATION_FAILED;
    }
    // SAFETY: The caller guarantees a live readable source of `length` bytes.
    let source = unsafe { std::slice::from_raw_parts(source, length) };
    let mut files = lock_files();
    let Some(file) = files.get_mut(&handle) else {
        return FILE_OPERATION_FAILED;
    };
    file.write(source)
        .ok()
        .and_then(|written| isize::try_from(written).ok())
        .unwrap_or(FILE_OPERATION_FAILED)
}

/// Writes all `length` readable bytes.
///
/// # Safety
///
/// When `length` is nonzero, `source` must point to `length` live, readable
/// bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn file_write_all(handle: usize, source: *const u8, length: usize) -> i32 {
    if length == 0 {
        return 0;
    }
    if source.is_null() {
        return 1;
    }
    // SAFETY: The caller guarantees a live readable source of `length` bytes.
    let source = unsafe { std::slice::from_raw_parts(source, length) };
    let mut files = lock_files();
    let Some(file) = files.get_mut(&handle) else {
        return 1;
    };
    i32::from(file.write_all(source).is_err())
}

/// Flushes buffered data for an owned file handle.
#[unsafe(no_mangle)]
pub extern "C" fn file_flush(handle: usize) -> i32 {
    let mut files = lock_files();
    let Some(file) = files.get_mut(&handle) else {
        return 1;
    };
    i32::from(file.flush().is_err())
}

/// Attempts to synchronize file content and metadata to the filesystem.
#[unsafe(no_mangle)]
pub extern "C" fn file_sync_all(handle: usize) -> i32 {
    let file = {
        let files = lock_files();
        let Some(file) = files.get(&handle) else {
            return 1;
        };
        let Ok(file) = file.try_clone() else {
            return 1;
        };
        file
    };
    i32::from(file.sync_all().is_err())
}

/// Returns the unread byte length from the current cursor to the end.
#[unsafe(no_mangle)]
pub extern "C" fn file_remaining_len(handle: usize) -> isize {
    let mut files = lock_files();
    let Some(file) = files.get_mut(&handle) else {
        return FILE_OPERATION_FAILED;
    };
    let Some(remaining) = file
        .stream_position()
        .ok()
        .and_then(|position| file.metadata().ok().map(|metadata| (position, metadata)))
        .map(|(position, metadata)| metadata.len().saturating_sub(position))
    else {
        return FILE_OPERATION_FAILED;
    };
    isize::try_from(remaining).unwrap_or(FILE_OPERATION_FAILED)
}

/// Returns whether one UTF-8 path exists.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// readable UTF-8 bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn path_exists(data: *const u8, length: usize) -> bool {
    // SAFETY: The caller upholds the path-buffer contract documented above.
    unsafe { with_utf8_path(data, length, Path::exists) }.unwrap_or(false)
}

/// Removes one file at a UTF-8 path.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// readable UTF-8 bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn path_remove_file(data: *const u8, length: usize) -> i32 {
    // SAFETY: The caller upholds the path-buffer contract documented above.
    unsafe { with_utf8_path(data, length, |path| fs::remove_file(path).is_err()) }
        .map_or(1, i32::from)
}

/// Renames one UTF-8 path.
///
/// # Safety
///
/// Both pointer/length pairs must describe live readable UTF-8 bytes for the
/// duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn path_rename(
    source_data: *const u8,
    source_length: usize,
    destination_data: *const u8,
    destination_length: usize,
) -> i32 {
    // SAFETY: The caller upholds both path-buffer contracts documented above.
    unsafe {
        with_utf8_paths(
            source_data,
            source_length,
            destination_data,
            destination_length,
            |source, destination| fs::rename(source, destination).is_err(),
        )
    }
    .map_or(1, i32::from)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: Both UTF-16 buffers are NUL-terminated and remain live for the call.
    let status = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if status == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

/// Atomically publishes `source` at `destination`, replacing an existing file.
///
/// # Safety
///
/// Both pointer/length pairs must describe live readable UTF-8 bytes for the
/// duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn path_replace_file(
    source_data: *const u8,
    source_length: usize,
    destination_data: *const u8,
    destination_length: usize,
) -> i32 {
    // SAFETY: The caller upholds both path-buffer contracts documented above.
    unsafe {
        with_utf8_paths(
            source_data,
            source_length,
            destination_data,
            destination_length,
            |source, destination| replace_file(source, destination).is_err(),
        )
    }
    .map_or(1, i32::from)
}

/// Recursively creates one UTF-8 directory path.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// readable UTF-8 bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn path_create_dir_all(data: *const u8, length: usize) -> i32 {
    // SAFETY: The caller upholds the path-buffer contract documented above.
    unsafe { with_utf8_path(data, length, |path| fs::create_dir_all(path).is_err()) }
        .map_or(1, i32::from)
}

/// Writes the size of one regular file to `size`.
///
/// # Safety
///
/// `data` must describe live readable UTF-8 bytes and `size` must point to one
/// writable `u64` for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn path_file_size(data: *const u8, length: usize, size: *mut u64) -> i32 {
    if size.is_null() {
        return 1;
    }
    // SAFETY: The caller upholds the path-buffer contract documented above.
    let result = unsafe {
        with_utf8_path(data, length, |path| {
            fs::metadata(path)
                .ok()
                .filter(fs::Metadata::is_file)
                .map(|metadata| metadata.len())
        })
    }
    .flatten();
    let Some(length) = result else {
        return 1;
    };
    // SAFETY: The caller guarantees one live writable `u64` at `size`.
    unsafe { *size = length };
    0
}

/// Reports whether one existing path resolves within an existing root.
///
/// Returns `0` when contained, `1` when outside, and `-1` when either path is
/// invalid or cannot be canonicalized. Whole path components are compared, so
/// a sibling whose name merely shares the root's textual prefix is rejected.
///
/// # Safety
///
/// Both pointer/length pairs must describe live readable UTF-8 bytes for the
/// duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn path_is_within(
    root_data: *const u8,
    root_length: usize,
    candidate_data: *const u8,
    candidate_length: usize,
) -> i32 {
    // SAFETY: The caller upholds both path-buffer contracts documented above.
    unsafe {
        with_utf8_paths(
            root_data,
            root_length,
            candidate_data,
            candidate_length,
            |root, candidate| {
                let Ok(root) = fs::canonicalize(root) else {
                    return -1;
                };
                let Ok(candidate) = fs::canonicalize(candidate) else {
                    return -1;
                };
                i32::from(!candidate.starts_with(root))
            },
        )
    }
    .unwrap_or(-1)
}

/// Creates an owned snapshot of one canonical UTF-8 path.
///
/// Returns a positive handle, `-1` for an operating-system failure, or `-2`
/// when the canonical native path is not valid UTF-8.
///
/// # Safety
///
/// `data` must either be null with a zero `length`, or point to `length`
/// readable UTF-8 bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn path_canonical_open(data: *const u8, length: usize) -> isize {
    // SAFETY: The caller upholds the path-buffer contract documented above.
    unsafe {
        with_utf8_path(data, length, |path| {
            fs::canonicalize(path).map_or(PATH_OPERATION_FAILED, |canonical| {
                store_path_snapshot(&canonical)
            })
        })
    }
    .unwrap_or(PATH_OPERATION_FAILED)
}

/// Returns the byte length of one live canonical-path snapshot.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn path_snapshot_len(handle: usize) -> isize {
    lock_path_snapshots()
        .get(&handle)
        .and_then(|path| isize::try_from(path.len()).ok())
        .unwrap_or(PATH_INVALID_HANDLE)
}

/// Copies one complete canonical-path snapshot into caller-owned storage.
///
/// # Safety
///
/// When `capacity` is nonzero, `destination` must point to `capacity` live,
/// writable bytes for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn path_snapshot_copy(
    handle: usize,
    destination: *mut u8,
    capacity: usize,
) -> isize {
    let snapshots = lock_path_snapshots();
    let Some(path) = snapshots.get(&handle) else {
        return PATH_INVALID_HANDLE;
    };
    if path.len() > capacity || (!path.is_empty() && destination.is_null()) {
        return PATH_OPERATION_FAILED;
    }
    if !path.is_empty() {
        // SAFETY: The ABI contract provides a non-overlapping destination with
        // at least `path.len()` live writable bytes.
        unsafe { std::ptr::copy_nonoverlapping(path.as_ptr(), destination, path.len()) };
    }
    isize::try_from(path.len()).unwrap_or(PATH_OPERATION_FAILED)
}

/// Releases one canonical-path snapshot.
#[unsafe(no_mangle)]
pub extern "C" fn path_snapshot_close(handle: usize) -> i32 {
    i32::from(lock_path_snapshots().remove(&handle).is_none())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        FILE_UNEXPECTED_EOF, file_close, file_create, file_flush, file_open, file_read_exact,
        file_remaining_len, file_sync_all, file_write_all, path_canonical_open,
        path_create_dir_all, path_exists, path_file_size, path_is_within, path_remove_file,
        path_rename, path_replace_file, path_snapshot_close, path_snapshot_copy, path_snapshot_len,
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        source: PathBuf,
        destination: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let stem = format!("reimer-filesystem-{}-{nonce}", std::process::id());
            let source = std::env::temp_dir().join(format!("{stem}-source.txt"));
            let destination = std::env::temp_dir().join(format!("{stem}-destination.txt"));
            Self {
                source,
                destination,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.source);
            let _ = std::fs::remove_file(&self.destination);
        }
    }

    fn path_bytes(path: &Path) -> &[u8] {
        path.to_str()
            .expect("temporary path should be valid UTF-8")
            .as_bytes()
    }

    #[test]
    fn file_abi_should_create_write_reopen_and_read_exact_bytes() {
        let fixture = Fixture::new();
        let path = path_bytes(&fixture.source);
        // SAFETY: All byte slices remain live for each bounded ABI call.
        let created = unsafe { file_create(path.as_ptr(), path.len()) };
        let expected = b"answer";

        assert_ne!(created, 0);
        // SAFETY: `expected` remains live for the complete call.
        assert_eq!(
            unsafe { file_write_all(created, expected.as_ptr(), expected.len()) },
            0
        );
        assert_eq!(file_flush(created), 0);
        assert_eq!(file_sync_all(created), 0);
        assert_eq!(file_close(created), 0);

        // SAFETY: The path slice remains live for the complete call.
        let opened = unsafe { file_open(path.as_ptr(), path.len()) };
        let mut actual = [0_u8; 6];

        assert_ne!(opened, 0);
        assert_eq!(file_remaining_len(opened), 6);
        // SAFETY: `actual` provides exactly six live writable bytes.
        assert_eq!(
            unsafe { file_read_exact(opened, actual.as_mut_ptr(), actual.len()) },
            6
        );
        assert_eq!(&actual, expected);
        assert_eq!(file_close(opened), 0);
    }

    #[test]
    fn file_abi_should_report_unexpected_end_of_file() {
        let fixture = Fixture::new();
        let path = path_bytes(&fixture.source);
        // SAFETY: All byte slices remain live for each bounded ABI call.
        let created = unsafe { file_create(path.as_ptr(), path.len()) };
        let expected = b"ok";
        // SAFETY: `expected` remains live for the complete call.
        let write_status = unsafe { file_write_all(created, expected.as_ptr(), expected.len()) };
        let close_status = file_close(created);
        // SAFETY: The path slice remains live for the complete call.
        let opened = unsafe { file_open(path.as_ptr(), path.len()) };
        let mut destination = [0_u8; 3];
        // SAFETY: `destination` provides three live writable bytes.
        let read_status =
            unsafe { file_read_exact(opened, destination.as_mut_ptr(), destination.len()) };
        let final_close_status = file_close(opened);

        assert_eq!(
            (write_status, close_status, read_status, final_close_status),
            (0, 0, FILE_UNEXPECTED_EOF, 0)
        );
    }

    #[test]
    fn path_abi_should_rename_detect_and_remove_files() {
        let fixture = Fixture::new();
        let source = path_bytes(&fixture.source);
        let destination = path_bytes(&fixture.destination);
        // SAFETY: Both path byte slices remain live for every bounded ABI call.
        let handle = unsafe { file_create(source.as_ptr(), source.len()) };
        assert_ne!(handle, 0);
        assert_eq!(file_close(handle), 0);

        // SAFETY: Both path byte slices remain live for every bounded ABI call.
        assert!(unsafe { path_exists(source.as_ptr(), source.len()) });
        // SAFETY: Both path byte slices remain live for every bounded ABI call.
        assert_eq!(
            unsafe {
                path_rename(
                    source.as_ptr(),
                    source.len(),
                    destination.as_ptr(),
                    destination.len(),
                )
            },
            0
        );
        // SAFETY: Both path byte slices remain live for every bounded ABI call.
        assert!(!unsafe { path_exists(source.as_ptr(), source.len()) });
        // SAFETY: Both path byte slices remain live for every bounded ABI call.
        assert!(unsafe { path_exists(destination.as_ptr(), destination.len()) });
        // SAFETY: The destination path slice remains live for the complete call.
        assert_eq!(
            unsafe { path_remove_file(destination.as_ptr(), destination.len()) },
            0
        );
    }

    #[test]
    fn path_replace_should_overwrite_an_existing_destination_atomically() {
        let fixture = Fixture::new();
        fs::write(&fixture.source, b"new").expect("source fixture should be writable");
        fs::write(&fixture.destination, b"old").expect("destination fixture should be writable");
        let source = path_bytes(&fixture.source);
        let destination = path_bytes(&fixture.destination);

        // SAFETY: Both UTF-8 path buffers remain live for this bounded call.
        let status = unsafe {
            path_replace_file(
                source.as_ptr(),
                source.len(),
                destination.as_ptr(),
                destination.len(),
            )
        };

        assert_eq!(status, 0);
        assert_eq!(
            fs::read(&fixture.destination).expect("published fixture should remain readable"),
            b"new"
        );
    }

    #[test]
    fn path_canonical_snapshot_should_preserve_one_stable_utf8_result() {
        let fixture = Fixture::new();
        fs::write(&fixture.source, b"data").expect("source fixture should be writable");
        let source = path_bytes(&fixture.source);
        // SAFETY: The UTF-8 path buffer remains live for this bounded call.
        let opened = unsafe { path_canonical_open(source.as_ptr(), source.len()) };
        assert!(opened > 0);
        let handle = opened.cast_unsigned();
        let length = path_snapshot_len(handle);
        assert!(length > 0);
        let mut canonical = vec![0_u8; length.cast_unsigned()];
        // SAFETY: `canonical` is a live destination with the queried capacity.
        let copied = unsafe { path_snapshot_copy(handle, canonical.as_mut_ptr(), canonical.len()) };

        assert_eq!(copied, length);
        assert_eq!(path_snapshot_close(handle), 0);
        assert_eq!(
            Path::new(std::str::from_utf8(&canonical).expect("path should be UTF-8")),
            fixture
                .source
                .canonicalize()
                .expect("fixture should canonicalize")
        );
    }

    #[test]
    fn path_file_size_should_accept_files_and_reject_directories() {
        let fixture = Fixture::new();
        fs::write(&fixture.source, b"12345").expect("source fixture should be writable");
        let source = path_bytes(&fixture.source);
        let directory = path_bytes(
            fixture
                .source
                .parent()
                .expect("fixture should have a parent"),
        );
        let mut file_size = 0_u64;
        let mut directory_size = 0_u64;

        // SAFETY: Each path and output buffer remains live for its bounded call.
        let file_status =
            unsafe { path_file_size(source.as_ptr(), source.len(), &raw mut file_size) };
        // SAFETY: Each path and output buffer remains live for its bounded call.
        let directory_status =
            unsafe { path_file_size(directory.as_ptr(), directory.len(), &raw mut directory_size) };

        assert_eq!((file_status, file_size, directory_status), (0, 5, 1));
    }

    #[test]
    fn path_create_dir_all_should_create_every_missing_component() {
        let fixture = Fixture::new();
        let root = fixture.source.with_extension("directory");
        let nested = root.join("one").join("two");
        let nested_bytes = path_bytes(&nested);

        // SAFETY: The UTF-8 path buffer remains live for this bounded call.
        let status = unsafe { path_create_dir_all(nested_bytes.as_ptr(), nested_bytes.len()) };
        let created = nested.is_dir();
        let _ = fs::remove_dir_all(root);

        assert_eq!((status, created), (0, true));
    }

    #[test]
    fn path_is_within_should_compare_canonical_components() {
        let nonce = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("reimer-root-{nonce}"));
        let nested = root.join("assets").join("sprite.png");
        let sibling = std::env::temp_dir().join(format!("reimer-root-{nonce}-sibling"));
        fs::create_dir_all(nested.parent().expect("nested file should have a parent"))
            .expect("test root should be created");
        fs::write(&nested, b"pixel").expect("test file should be created");
        fs::create_dir_all(&sibling).expect("test sibling should be created");

        let root_bytes = path_bytes(&root);
        let nested_bytes = path_bytes(&nested);
        let sibling_bytes = path_bytes(&sibling);
        // SAFETY: Every path slice remains live for the bounded calls.
        assert_eq!(
            unsafe {
                path_is_within(
                    root_bytes.as_ptr(),
                    root_bytes.len(),
                    nested_bytes.as_ptr(),
                    nested_bytes.len(),
                )
            },
            0
        );
        // SAFETY: Every path slice remains live for the bounded calls.
        assert_eq!(
            unsafe {
                path_is_within(
                    root_bytes.as_ptr(),
                    root_bytes.len(),
                    sibling_bytes.as_ptr(),
                    sibling_bytes.len(),
                )
            },
            1
        );

        fs::remove_dir_all(&root).expect("test root should be removed");
        fs::remove_dir_all(&sibling).expect("test sibling should be removed");
    }
}
