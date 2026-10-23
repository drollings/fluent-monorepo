//! Epoch-millisecond instants: one numeric time domain for comparisons that
//! cross bar timeframes.
//!
//! Timestamps reach this codebase in three shapes — [`DateTime<Utc>`] on a
//! closed bar, whole seconds in the Pine runtime, and epoch milliseconds in the
//! injected-dataset columns. A comparison between two of them that does not
//! name its domain silently mixes units, so the boundary takes this type and
//! the conversions live here, once.

use std::fmt;

/// A UTC instant measured in milliseconds since the Unix epoch.
///
/// Ordering is numeric, so a slice of these is always sorted when its stamps
/// are. The type is `Copy` and costs nothing at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct EpochMs(i64);

impl EpochMs {
    /// The Unix epoch itself.
    pub const UNIX_EPOCH: Self = Self(0);

    /// Wrap a millisecond count.
    #[must_use]
    pub const fn new(millis: i64) -> Self {
        Self(millis)
    }

    /// The millisecond count.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Milliseconds from `other` to `self`, or [`None`] when the difference
    /// leaves the `i64` range.
    #[must_use]
    pub fn checked_sub(self, other: Self) -> Option<i64> {
        self.0.checked_sub(other.0)
    }

    /// Read a whole-second stamp carried as a float — the Pine runtime's
    /// closed-bar clock. Non-finite and out-of-range values read as [`None`].
    #[must_use]
    pub fn from_seconds_f64(seconds: f64) -> Option<Self> {
        Self::from_millis_f64(seconds * 1000.0)
    }

    /// Read a millisecond stamp carried as a float — the Pine runtime's
    /// tick-emulation clock. Non-finite and out-of-range values read as
    /// [`None`].
    ///
    /// `2^63` is the first float that no longer fits an `i64`; excluding it on
    /// both sides keeps the cast exact.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn from_millis_f64(millis: f64) -> Option<Self> {
        const LIMIT: f64 = 9_223_372_036_854_775_808.0;
        if !millis.is_finite() || !(-LIMIT..LIMIT).contains(&millis) {
            return None;
        }
        Some(Self(millis.trunc() as i64))
    }
}

impl From<i64> for EpochMs {
    fn from(millis: i64) -> Self {
        Self(millis)
    }
}

impl From<EpochMs> for i64 {
    fn from(stamp: EpochMs) -> Self {
        stamp.0
    }
}

impl From<chrono::DateTime<chrono::Utc>> for EpochMs {
    fn from(stamp: chrono::DateTime<chrono::Utc>) -> Self {
        Self(stamp.timestamp_millis())
    }
}

impl fmt::Display for EpochMs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
#[path = "../tests/epoch.rs"]
mod tests;
