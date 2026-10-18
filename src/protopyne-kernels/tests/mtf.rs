//! Grid-math goldens: exact integer expectations plus exact tape tuples.
//! `#[allow(clippy::float_cmp)]` covers the literal tape comparisons.
#![allow(clippy::float_cmp)]

use super::*;
use crate::bars::BarParts;
use protopyne_types::Timeframe;
use std::num::NonZeroI64;

fn grid(ms: i64) -> NonZeroI64 {
    NonZeroI64::new(ms).expect("positive test grid")
}

#[test]
fn floor_grids_and_refuses() {
    assert_eq!(
        floor_to_grid(1_699_999_980_000, grid(60_000)),
        1_699_999_980_000
    );
    assert_eq!(
        floor_to_grid(1_699_999_980_001, grid(60_000)),
        1_699_999_980_000
    );
    assert_eq!(
        floor_to_grid(-1, grid(60_000)),
        -60_000,
        "euclid floors negatives"
    );
    assert_eq!(
        floor_to_grid_checked(61_000, 60_000).expect("valid"),
        60_000
    );
    for bad in [0, -60_000] {
        let err = floor_to_grid_checked(61_000, bad).expect_err("bad grid refuses");
        assert!(
            err.to_string().contains("grid"),
            "refusal names the reason: {err}"
        );
    }
}

/// Sub-weekly spans share one anchor: the streaming grid floor and the
/// relocated bucket rule agree stamp for stamp, so either may be used.
#[test]
fn sub_weekly_grid_matches_the_relocated_bucket_rule() {
    for variant in [Timeframe::M15, Timeframe::H1, Timeframe::H4, Timeframe::D1] {
        let step = variant.duration_ms();
        for stamp in [
            0_i64,
            1,
            -1,
            step - 1,
            step,
            1_699_999_980_001,
            -1_699_999_980_001,
        ] {
            assert_eq!(
                floor_to_grid(stamp, grid(step)),
                variant.bucket_start_ms(stamp),
                "{variant:?} anchors identically at {stamp}"
            );
        }
    }
}

/// The weekly anchor diverges, intentionally: the streaming projection grid
/// stays epoch-anchored (the epoch Thursday opens its bucket) while the
/// relocated bucket rule opens weeks on the epoch Monday. Both behaviors are
/// pinned here so neither can drift; never "fix" one to match the other —
/// kernels streaming semantics are out of scope for unification.
#[test]
fn weekly_grid_divergence_from_the_relocated_bucket_rule_is_pinned() {
    let week_ms = Timeframe::W1.duration_ms();
    assert_eq!(
        floor_to_grid(0, grid(week_ms)),
        0,
        "streaming grid stays epoch-anchored"
    );
    assert_eq!(
        Timeframe::W1.bucket_start_ms(0),
        -259_200_000,
        "relocated rule opens on the epoch Monday"
    );
    assert_ne!(
        floor_to_grid(0, grid(week_ms)),
        Timeframe::W1.bucket_start_ms(0),
        "the W1 divergence is real and stays stated, never silently unified"
    );
}

#[test]
fn fully_formed_edges() {
    // Bucket [T, T+60s) confirms exactly when now reaches T+60s.
    assert!(fully_formed(1_000_000, grid(60_000), 1_060_000));
    assert!(!fully_formed(1_000_000, grid(60_000), 1_059_999));
    assert!(
        !fully_formed(1_000_000, grid(60_000), 1_000_000),
        "open bucket unformed"
    );
}

#[test]
fn merge_prefers_later_on_overlap() {
    let a = vec![(1_000, 1.0), (2_000, 2.0), (3_000, 3.0)];
    let b = vec![(2_000, 20.0), (4_000, 4.0)];
    let merged = merge_sorted(&a, &b);
    assert_eq!(
        merged,
        vec![(1_000, 1.0), (2_000, 20.0), (3_000, 3.0), (4_000, 4.0)],
        "equal stamps resolve to the later tape"
    );
    assert!(merge_sorted(&[], &[]).is_empty());
    assert_eq!(merge_sorted(&a, &[]), a, "empty side is identity");
    assert_eq!(merge_sorted(&[], &b), b);
    // Generic arm over timestamped rows: same contract, any payload.
    let rows_a = [BarParts {
        ts_ms: 1_000,
        o: 1.0,
        h: 1.0,
        l: 1.0,
        c: 1.0,
        volume: 5.0,
        buy_volume: 5.0,
    }];
    let rows_b = [BarParts {
        ts_ms: 1_000,
        o: 2.0,
        h: 2.0,
        l: 2.0,
        c: 2.0,
        volume: 7.0,
        buy_volume: 7.0,
    }];
    let joined = merge_sorted_by(&rows_a, &rows_b, |row| row.ts_ms);
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0].volume, 7.0, "later row wins");
}

#[test]
fn interleave_keeps_both_sides() {
    let a = vec![(1_000, 1.0), (2_000, 2.0)];
    let b = vec![(2_000, 20.0), (3_000, 3.0)];
    assert_eq!(
        interleave_sorted(&a, &b),
        vec![(1_000, 1.0), (2_000, 2.0), (2_000, 20.0), (3_000, 3.0)],
        "ties keep both, earlier side first"
    );
}

#[test]
fn pairs_join_on_stamps() {
    let a = vec![(1_000, 1.0), (2_000, 2.0), (3_000, 3.0)];
    let b = vec![(2_000, 20.0), (3_000, 30.0), (4_000, 40.0)];
    let pairs = pairs_from_sorted(&a, &b, |row| row.0);
    assert_eq!(pairs.len(), 2, "inner join on equal stamps");
    assert_eq!(pairs[0], ((2_000, 2.0), (2_000, 20.0)));
    assert_eq!(pairs[1], ((3_000, 3.0), (3_000, 30.0)));
    assert!(pairs_from_sorted(&a, &[], |row| row.0).is_empty());
}

#[test]
fn buckets_detect_gaps() {
    let dense = [1_000_000, 1_060_000, 1_120_000];
    assert_eq!(bucket_ids(&dense, grid(60_000)), vec![16, 17, 18]);
    assert!(!bucket_has_gap(&dense, grid(60_000)));
    let sparse = [1_000_000, 1_180_000];
    assert!(
        bucket_has_gap(&sparse, grid(60_000)),
        "skipped bucket reads as gap"
    );
    assert!(!bucket_has_gap(&[], grid(60_000)));
    assert!(!bucket_has_gap(&[1_000_000], grid(60_000)));
}

#[test]
fn grid_math_roundtrips_and_ceils() {
    for ts in [0, 1, 59_999, 60_000, 1_699_999_980_001, -1] {
        assert!(
            grid_roundtrip(ts, grid(60_000)),
            "floors are fixed points at {ts}"
        );
    }
    // 1_000_000 % 60_000 == 40_000: unaligned, ceils to 1_020_000.
    assert_eq!(aligned_geq(1_000_000, grid(60_000)), 1_020_000, "ceils up");
    assert_eq!(aligned_geq(1_000_001, grid(60_000)), 1_020_000, "ceils up");
    assert_eq!(
        aligned_geq(1_020_000, grid(60_000)),
        1_020_000,
        "aligned stays"
    );
}

#[test]
fn split_validation_fails_closed() {
    validate_split(1_000_000, 2_000_000, 1_500_000, 60_000).expect("interior aligned split holds");
    for (first, last, split, grid_ms, why) in [
        (1_000_000, 2_000_000, 1_500_001, 60_000, "off-grid split"),
        (
            1_000_000,
            2_000_000,
            1_000_000 - 60_000,
            60_000,
            "split before first",
        ),
        (
            1_000_000,
            2_000_000,
            2_000_000,
            60_000,
            "split at last leaves no test side",
        ),
        (1_000_000, 1_000_000, 1_000_000, 60_000, "single-bar range"),
        (2_000_000, 1_000_000, 1_500_000, 60_000, "inverted range"),
        (1_000_000, 2_000_000, 1_500_000, 0, "dead grid"),
        (1_000_000, 2_000_000, 1_500_000, -60_000, "negative grid"),
    ] {
        let err = validate_split(first, last, split, grid_ms).expect_err("{why} must refuse");
        assert!(
            !err.to_string().is_empty(),
            "{why} refusal carries a reason"
        );
    }
}

#[test]
fn lookahead_stays_refused() {
    Lookahead::Off.require_off().expect("off passes");
    let err = Lookahead::On.require_off().expect_err("on refuses");
    assert!(
        err.to_string().contains("lookahead"),
        "refusal names the reason: {err}"
    );
}

#[test]
fn bucket_spec_validates_through_execute() {
    use fluent_wvr::prelude::{Component, WorkUnit};
    use protopyne_types::Validate;
    let spec = BucketSpec::new(60_000);
    assert_eq!(spec.grid_ms(), 60_000);
    spec.validate().expect("valid spec validates");
    assert_eq!(spec.field_names(), &["grid_ms"]);
    assert_eq!(spec.get_field("grid_ms").expect("reads"), "60000");
    let ctx = fluent_wvr::prelude::WorkContext::default();
    assert!(spec.execute(&ctx).expect("valid spec executes").success);
    let mut bad = BucketSpec::new(0);
    assert!(bad.validate().is_err(), "dead grid refuses validation");
    assert!(bad.execute(&ctx).is_err(), "invalid spec fails closed");
    assert!(bad.set_field("grid_ms", "1").is_err());
    assert!(bad.get_field("nope").is_err());
    let _ = bad.as_any();
    // Sub-second grids refuse: bucket math assumes whole seconds.
    assert!(BucketSpec::new(1_500).validate().is_err());
    assert!(BucketSpec::new(-60_000).validate().is_err());
}
