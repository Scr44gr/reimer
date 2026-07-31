# Time and sleeping

`std::time` separates wall-clock timestamps from monotonic interval
measurement. Its public API is safe and allocation-free.

## Clocks

`time()` and `unix_time()` return `f64` seconds relative to the Unix epoch.
They are appropriate for timestamps, logging, and interoperability. Operating
system clock corrections can move them forwards or backwards, so they must not
be used to measure elapsed work.

`monotonic()` and `perf_counter()` return fractional seconds from a
process-local monotonic clock. `Instant` is preferred when exact nanosecond
intervals matter because it avoids converting the counter to floating point.

```reimer
from std::time import Instant;

let started = Instant::now();
// measured work
let elapsed = started.elapsed();
```

An `Instant` is meaningful only inside the current process execution. It is
not a calendar timestamp and should not be persisted.

## Durations

`Duration` represents a non-negative span as whole seconds plus a normalized
nanosecond component. Constructors accept seconds, milliseconds,
microseconds, or nanoseconds without overflowing during normalization.

Total millisecond, microsecond, and nanosecond conversions return `u64` and
saturate when the complete value exceeds that range. Fractional seconds are
available through `as_seconds_f64()`.

## Sleeping

`sleep(duration)` blocks the current native thread for at least the requested
duration. The runtime delegates to the operating system through Rust's
`std::thread::sleep`; it does not poll a clock or consume a CPU core in a busy
loop. Other runnable native threads continue normally.

```reimer
from std::time import Duration, sleep, sleep_milliseconds;

sleep(Duration::from_seconds(1));
sleep_milliseconds(250);
```

`sleep_seconds` and `sleep_milliseconds` are convenience wrappers. Sleeping is
blocking rather than asynchronous; async timers remain outside the v0.1
execution model.
