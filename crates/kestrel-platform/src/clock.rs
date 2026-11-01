//! Wall-independent clocks.

use std::time::Duration;

/// Time since boot, counting time spent suspended.
///
/// The lid switch may suspend in spite of a logind sleep inhibitor, so a
/// deadline measured this way still ends when the user expects.
pub fn boot_time() -> Duration {
    // SAFETY: `clock_gettime` writes one `timespec` into caller-owned storage.
    let mut value: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: the pointer is valid for the duration of the call.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut value) } != 0 {
        return Duration::ZERO;
    }
    let Ok(seconds) = u64::try_from(value.tv_sec) else {
        return Duration::ZERO;
    };
    let nanos = u64::try_from(value.tv_nsec).unwrap_or_default();
    Duration::from_secs(seconds).saturating_add(Duration::from_nanos(nanos))
}
