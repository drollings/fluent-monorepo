//! Composition pins for trigger verdicts.
//!
//! These tests freeze the input/output contract of the trigger catalog that
//! later composition work (envelopes, signal assembly) must preserve
//! byte-identically: same tape in, same verdict out, regardless of which
//! composition path evaluates it.

use super::*;
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

fn legs_with_fast(fast: Vec<Option<f64>>, len: usize) -> protopyne_kernels::lite::LiteLegs {
    let none = |n: usize| vec![None; n];
    protopyne_kernels::lite::LiteLegs {
        fast,
        slow: none(len),
        fast_raw: none(len),
        slow_raw: none(len),
        adaptive: none(len),
        signal: none(len),
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
fn the_trigger_catalog_lists_the_five_pine_branches() {
    assert_eq!(
        trigger_ids(),
        vec![
            "momentum",
            "ribbon_flip",
            "osc_cross",
            "exhaustion",
            "dual_exhaustion",
        ]
    );
    assert!(triggers()
        .build("not_a_trigger", &serde_json::json!({}))
        .is_err());
}

#[test]
fn cross_level_arms_long_on_an_upward_cross_and_holds_at_index_zero() {
    let bars = bars(3);
    let legs = legs_with_fast(vec![Some(-150.0), Some(-50.0), Some(150.0)], 3);
    let crossed = Tape::new(&bars, &legs, &legs.close, 2).expect("tape");
    let trigger = CrossLevel {
        long_level: 100.0,
        short_level: -100.0,
        adaptive: false,
        family: TriggerFamily::Trend,
        reason: "pin",
    };
    let verdict = trigger.eval(&crossed);
    assert_eq!(
        verdict,
        TriggerVerdict::armed(true, false, TriggerFamily::Trend, "pin")
    );
    let first = Tape::new(&bars, &legs, &legs.close, 0).expect("tape");
    let idle = trigger.eval(&first);
    assert_eq!(
        idle,
        TriggerVerdict::idle(TriggerFamily::Trend, "pin"),
        "no previous bar means no crossing is observable"
    );
}

#[test]
fn cross_level_arms_short_on_a_downward_cross() {
    let bars = bars(3);
    let legs = legs_with_fast(vec![Some(150.0), Some(50.0), Some(-150.0)], 3);
    let tape = Tape::new(&bars, &legs, &legs.close, 2).expect("tape");
    let trigger = CrossLevel {
        long_level: 100.0,
        short_level: -100.0,
        adaptive: false,
        family: TriggerFamily::CounterTrend,
        reason: "pin",
    };
    let verdict = trigger.eval(&tape);
    assert_eq!(
        verdict,
        TriggerVerdict::armed(false, true, TriggerFamily::CounterTrend, "pin")
    );
}

#[test]
fn cross_level_adaptive_is_idle_without_both_oscillators() {
    let bars = bars(3);
    let legs = legs_with_fast(vec![Some(-150.0), Some(-50.0), Some(150.0)], 3);
    let tape = Tape::new(&bars, &legs, &legs.close, 2).expect("tape");
    let trigger = CrossLevel {
        long_level: 100.0,
        short_level: -100.0,
        adaptive: true,
        family: TriggerFamily::Trend,
        reason: "pin",
    };
    assert_eq!(
        trigger.eval(&tape),
        TriggerVerdict::idle(TriggerFamily::Trend, "pin"),
        "adaptive needs slow too; slow is absent so the thesis refuses"
    );
}
