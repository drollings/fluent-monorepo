//! Time utilities: epoch-second helpers.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current system time as seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Wall clock now, in nanoseconds since the UNIX epoch.
///
/// Canonical home of the former `common_core::io::now_ns`; the old
/// path stays as a deprecated re-export while callers migrate.
#[must_use]
pub fn now_ns() -> u64 {
    crate::io::system_time_ns(SystemTime::now())
}
