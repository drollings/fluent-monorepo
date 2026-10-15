//! The fetch-bounds round-trip pin: widen, then narrow, returns the window.
//!
//! `adjust_fetch_bounds` widens a fetch by the warmup headroom and
//! `trim_indicator_output` narrows the kernel output back. Both live in
//! `protopyne-types` and are used as-is — never re-derived here — and this
//! suite pins their composition from the store side: if the two helpers ever
//! drift, the terminal window and the backtest window disagree silently, and
//! this table is what makes that loud. The headroom values themselves are
//! `types`' own tests' business; what is pinned here is that trim undoes
//! adjust exactly, across a frozen table of `(warmup, timeframe, limit)`
//! triples.

use protopyne_types::{Timeframe, adjust_fetch_bounds, trim_indicator_output, warmup_headroom};

/// The user-requested start every row of the table widens from: large enough
/// that no headroom in the table clamps it at zero, so every row exercises
/// the widening rather than the clamp.
const START_MS: i64 = 1_767_225_900_000;

fn warmups() -> Vec<usize> {
    vec![0, 1, 2, 5, 14, 30]
}

fn timeframes() -> Vec<Timeframe> {
    vec![Timeframe::M1, Timeframe::M5, Timeframe::H1, Timeframe::D1]
}

fn limits() -> Vec<usize> {
    vec![1, 10, 100]
}

#[test]
fn trim_undoes_adjust_across_the_table() {
    let mut rows = 0;
    for warmup in warmups() {
        for timeframe in timeframes() {
            for limit in limits() {
                let adjusted = adjust_fetch_bounds(Some(START_MS), Some(limit), timeframe, warmup);
                let headroom = warmup_headroom(warmup);
                let headroom_i64 = i64::try_from(headroom).expect("headroom fits");
                let step_ms = timeframe.duration_ms();
                assert_eq!(
                    adjusted.fetch_start_ms,
                    Some(START_MS - headroom_i64 * step_ms),
                    "warmup {warmup} {timeframe} backs the fetch off by the headroom"
                );
                let fetch_limit = adjusted.fetch_limit.expect("a limit widens to a limit");
                assert_eq!(
                    fetch_limit,
                    limit + headroom,
                    "warmup {warmup}: the fetch grows by exactly the headroom"
                );
                // A full-length kernel output over the widened fetch, each
                // position carrying its own index so content is checked, not
                // just length.
                let fetch_len = i64::try_from(fetch_limit).expect("fetch fits");
                let values: Vec<i64> = (0..fetch_len).collect();
                let trimmed = trim_indicator_output(&values, warmup, adjusted.original_limit);
                let limit_i64 = i64::try_from(limit).expect("limit fits");
                let expected: Vec<i64> = (headroom_i64..headroom_i64 + limit_i64).collect();
                assert_eq!(
                    trimmed, expected,
                    "warmup {warmup} {timeframe} limit {limit}: trim ∘ adjust returns the window"
                );
                rows += 1;
            }
        }
    }
    assert_eq!(rows, 72, "the table runs every triple");
}
