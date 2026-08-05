//! Process-local identity generation for runtime-owned namespaces.

use std::sync::atomic::{AtomicU64, Ordering};

/// ABI symbol that returns one process-unique nonzero identifier.
pub const PROCESS_UNIQUE_ID_SYMBOL: &str = "runtime_process_unique_id";

const FIRST_PROCESS_UNIQUE_ID: u64 = 1;

static NEXT_PROCESS_UNIQUE_ID: AtomicU64 = AtomicU64::new(FIRST_PROCESS_UNIQUE_ID);

/// Returns a monotonically increasing identifier, or zero after exhaustion.
///
/// Identifiers are never reused during the lifetime of the current process.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn runtime_process_unique_id() -> u64 {
    NEXT_PROCESS_UNIQUE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::runtime_process_unique_id;

    #[test]
    fn process_unique_ids_should_be_nonzero_and_monotonic() {
        let first = runtime_process_unique_id();
        let second = runtime_process_unique_id();

        assert_ne!(first, 0);
        assert!(second > first);
    }
}
