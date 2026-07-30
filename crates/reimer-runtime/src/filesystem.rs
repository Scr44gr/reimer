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
/// ABI symbol used to query unread bytes in a regular file.
pub const FILE_REMAINING_LEN_SYMBOL: &str = "file_remaining_len";
/// ABI symbol used to test whether a UTF-8 path exists.
pub const PATH_EXISTS_SYMBOL: &str = "path_exists";
/// ABI symbol used to remove one file.
pub const PATH_REMOVE_FILE_SYMBOL: &str = "path_remove_file";
/// ABI symbol used to rename one file-system path.
pub const PATH_RENAME_SYMBOL: &str = "path_rename";

/// A read could not fill the requested destination before end-of-file.
pub const FILE_UNEXPECTED_EOF: isize = -2;
const FILE_OPERATION_FAILED: isize = -1;

static FILES: OnceLock<Mutex<HashMap<usize, File>>> = OnceLock::new();
static NEXT_FILE_HANDLE: AtomicUsize = AtomicUsize::new(1);

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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        FILE_UNEXPECTED_EOF, file_close, file_create, file_flush, file_open, file_read_exact,
        file_remaining_len, file_write_all, path_exists, path_remove_file, path_rename,
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
}
