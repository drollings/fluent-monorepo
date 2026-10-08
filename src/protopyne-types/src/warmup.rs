//! Warmup fetch-bounds and output-trimming discipline.
//!
//! Indicator kernels need `warmup - 1` extra bars before the user-requested
//! range before their first output is trustworthy. [`adjust_fetch_bounds`]
//! widens the fetch to include that headroom; [`trim_indicator_output`]
//! removes exactly the headroom afterwards, so `trim(adjust(n))` yields
//! precisely the user range. Warmup length is a **correctness** gate: it
//! measures whether an output is valid, never whether a trade is worthwhile.

use crate::timeframe::Timeframe;

/// Extra bars fetched ahead of the user range for a given warmup.
#[must_use]
pub fn warmup_headroom(warmup: usize) -> usize {
    warmup.saturating_sub(1)
}

/// Fetch parameters widened by the warmup headroom alongside the untouched
/// originals, mirroring the historical-service contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjustedBounds {
    /// Start to actually fetch (backed off by headroom bars), if bounded.
    pub fetch_start_ms: Option<i64>,
    /// Bar count to actually fetch (expanded by headroom), if bounded.
    pub fetch_limit: Option<usize>,
    /// User-requested start, preserved for trimming.
    pub original_start_ms: Option<i64>,
    /// User-requested bar count, preserved for trimming.
    pub original_limit: Option<usize>,
}

/// Widen fetch bounds by the warmup headroom.
///
/// A provided start moves earlier by `headroom * timeframe`; a provided
/// limit grows by `headroom`. `warmup` of 0 or 1 adds no headroom. The start
/// is clamped at zero rather than wrapping.
#[must_use]
pub fn adjust_fetch_bounds(
    start_ms: Option<i64>,
    limit: Option<usize>,
    timeframe: Timeframe,
    warmup: usize,
) -> AdjustedBounds {
    let headroom = warmup_headroom(warmup) as i64;
    let step_ms = i64::from(timeframe.minutes()) * 60_000;
    AdjustedBounds {
        fetch_start_ms: start_ms.map(|s| (s - headroom * step_ms).max(0)),
        fetch_limit: limit.map(|l| l + warmup_headroom(warmup)),
        original_start_ms: start_ms,
        original_limit: limit,
    }
}

/// Trim a full-length kernel output back to the user-requested window.
///
/// Drops exactly the warmup headroom from the head, then keeps the trailing
/// `original_limit` values when the window is longer. Trimming is positional:
/// when the warmup was too small (or zero), leading `None`s from the kernel
/// stay in the output where they remain visible instead of being silently
/// laundered.
#[must_use]
pub fn trim_indicator_output<T: Clone>(
    values: &[T],
    warmup: usize,
    original_limit: Option<usize>,
) -> Vec<T> {
    let head = warmup_headroom(warmup).min(values.len());
    let window = &values[head..];
    match original_limit {
        Some(n) if window.len() > n => window[window.len() - n..].to_vec(),
        _ => window.to_vec(),
    }
}

#[cfg(test)]
#[path = "../tests/warmup.rs"]
mod tests;
