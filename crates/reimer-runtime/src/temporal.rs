//! Clock and blocking-sleep ABI used by `std::time`.

use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// ABI symbol for Unix wall-clock seconds.
pub const TIME_UNIX_SECONDS_SYMBOL: &str = "time_unix_seconds";
/// ABI symbol for monotonic nanoseconds from a process-local epoch.
pub const TIME_MONOTONIC_NANOS_SYMBOL: &str = "time_monotonic_nanoseconds";
/// ABI symbol for blocking the current native thread.
pub const TIME_SLEEP_SYMBOL: &str = "time_sleep";

static MONOTONIC_EPOCH: OnceLock<Instant> = OnceLock::new();

/// Returns seconds before or after the Unix epoch using the system wall clock.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn time_unix_seconds() -> f64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64(),
        Err(error) => -error.duration().as_secs_f64(),
    }
}

/// Returns monotonic nanoseconds from a stable process-local epoch.
#[must_use]
#[unsafe(no_mangle)]
pub extern "C" fn time_monotonic_nanoseconds() -> u64 {
    let epoch = MONOTONIC_EPOCH.get_or_init(Instant::now);
    u64::try_from(epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Blocks the current native thread without spinning on the CPU.
#[unsafe(no_mangle)]
pub extern "C" fn time_sleep(seconds: u64, nanoseconds: u32) {
    thread::sleep(Duration::new(seconds, nanoseconds));
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{time_monotonic_nanoseconds, time_sleep, time_unix_seconds};

    #[test]
    fn unix_clock_should_report_a_modern_positive_timestamp() {
        assert!(time_unix_seconds() > 1_600_000_000.0);
    }

    #[test]
    fn monotonic_clock_should_not_move_backwards() {
        let before = time_monotonic_nanoseconds();
        let after = time_monotonic_nanoseconds();

        assert!(after >= before);
    }

    #[test]
    fn sleep_should_block_for_at_least_the_requested_duration() {
        let started = Instant::now();
        time_sleep(0, 2_000_000);

        assert!(started.elapsed() >= Duration::from_millis(2));
    }
}
