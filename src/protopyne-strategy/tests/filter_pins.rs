//! Composition pins for filter admissions.
//!
//! These tests freeze the input/output contract of the filter catalog that
//! later composition work must preserve: same tape and direction in, same
//! admission out, with the documented skip reason text.

use super::*;
use crate::proposal::Direction;
use crate::tape::Tape;

fn bars(len: usize) -> Vec<protopyne_types::Bar> {
    (0..len)
        .map(|index| {
            let close = 100.0 + index as f64;
            let value = rust_decimal::Decimal::from_f64_retain(close).expect("finite");
            protopyne_types::Bar::new()
                .ts(
                    chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 1, 5, 15, 0, 0)
                        .single()
                        .expect("time")
                        + chrono::Duration::minutes(index as i64),
                )
                .o(value)
                .h(value)
                .l(value)
                .c(value)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn legs_with_pair(
    adaptive: Vec<Option<f64>>,
    signal: Vec<Option<f64>>,
    len: usize,
) -> protopyne_kernels::lite::LiteLegs {
    let none = |n: usize| vec![None; n];
    protopyne_kernels::lite::LiteLegs {
        fast: none(len),
        slow: none(len),
        fast_raw: none(len),
        slow_raw: none(len),
        adaptive,
        signal,
        mean: none(len),
        ribbon_health: none(len),
        ma_health: none(len),
        atr: none(len),
        atr_risk: none(len),
        atr_long: none(len),
        stdev: none(len),
        vol_ratio: vec![1.0; len],
        vol_sma: none(len),
        close: (0..len).map(|index| 100.0 + index as f64).collect(),
    }
}

#[test]
fn the_filter_catalog_is_nonempty_and_rejects_unknown_ids() {
    assert!(
        !filter_ids().is_empty(),
        "the admission catalog must list at least one guard"
    );
    assert!(filters()
        .build("not_a_filter", &serde_json::json!({}))
        .is_err());
}

#[test]
fn ribbon_filter_passes_aligned_sides_and_blocks_the_rest() {
    let bars = bars(2);
    let legs = legs_with_pair(vec![None, Some(2.0)], vec![None, Some(1.0)], 2);
    let tape = Tape::new(&bars, &legs, &legs.close, 1).expect("tape");
    let filter = RibbonFilter;
    assert_eq!(filter.admit(&tape, Direction::Long), Admission::Pass);
    assert_eq!(
        filter.admit(&tape, Direction::Short),
        Admission::Block("ribbon is not aligned with the side")
    );
}

#[test]
fn ribbon_filter_blocks_when_either_leg_is_absent() {
    let bars = bars(2);
    let legs = legs_with_pair(vec![None, None], vec![None, Some(1.0)], 2);
    let tape = Tape::new(&bars, &legs, &legs.close, 1).expect("tape");
    assert_eq!(
        RibbonFilter.admit(&tape, Direction::Long),
        Admission::Block("ribbon is not aligned with the side"),
        "absence is not alignment"
    );
}

#[test]
fn ribbon_filter_scope_is_trend_only() {
    assert_eq!(RibbonFilter.scope(), FilterScope::TrendOnly);
}
