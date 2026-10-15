//! Bar-folding basics: coarser spacings derived on read, never stored.
//!
//! The store holds one spacing per symbol; every coarser spacing is folded
//! from it at read time over absolute buckets. This suite pins the fold
//! arithmetic itself — first/last/max/min/sum over a hand-built series —
//! and the ordering contract the read boundary relies on: input must arrive
//! strictly ascending, because folding a shuffled series would mint
//! plausible-looking buckets out of misordered bars.

use super::*;
use protopyne_types::Timeframe;

use crate::commit::test_tapes::{bar, ohlc};
use crate::fold::{FOLD_ORIGIN_MS, WEEK_FOLD_ORIGIN_MS, bucket_start, fold_bars, fold_origin_ms};

/// Ten minute bars over two five-minute buckets. The fixture base sits on a
/// five-minute grid line, so steps 0–4 fill the first bucket exactly and
/// steps 5–9 the second.
fn ten_minutes() -> Vec<Bar> {
    vec![
        ohlc(0, "100", "101", "99", "100.5"),
        ohlc(1, "100.5", "102", "100", "101"),
        ohlc(2, "101", "101.5", "98", "99"),
        ohlc(3, "99", "100", "97.5", "98.5"),
        ohlc(4, "98.5", "99.5", "98", "99"),
        ohlc(5, "200", "205", "199", "201"),
        ohlc(6, "201", "203", "200", "202.5"),
        ohlc(7, "202.5", "210", "202", "209"),
        ohlc(8, "209", "209.5", "207", "208"),
        ohlc(9, "208", "208.5", "206", "207"),
    ]
}

#[test]
fn fold_reports_first_last_extremes_and_sums() {
    let folded = fold_bars(&ten_minutes(), Timeframe::M5).expect("aligned fold");
    assert_eq!(
        folded.len(),
        2,
        "ten minute bars fill two five-minute buckets"
    );
    let first = &folded[0];
    assert_eq!(first.o.to_string(), "100", "open is the first bar's open");
    assert_eq!(first.c.to_string(), "99", "close is the last bar's close");
    assert_eq!(first.h.to_string(), "102", "high is the bucket maximum");
    assert_eq!(first.l.to_string(), "97.5", "low is the bucket minimum");
    assert_eq!(first.volume, 5_000, "volume is the bucket sum");
    let second = &folded[1];
    assert_eq!(second.o.to_string(), "200");
    assert_eq!(second.c.to_string(), "207");
    assert_eq!(second.h.to_string(), "210");
    assert_eq!(second.l.to_string(), "199");
    assert_eq!(second.volume, 5_000);
}

#[test]
fn folded_bars_stamp_their_bucket_starts() {
    let bars = ten_minutes();
    let folded = fold_bars(&bars, Timeframe::M5).expect("aligned fold");
    for (bucket, source) in folded.iter().zip([bars[0].ts, bars[5].ts]) {
        assert_eq!(
            bucket.ts.timestamp_millis(),
            bucket_start(Timeframe::M5, source.timestamp_millis()),
            "a folded bar is stamped at its bucket's grid line"
        );
    }
}

#[test]
fn folding_at_the_stored_spacing_is_the_identity() {
    let bars = ten_minutes();
    let folded = fold_bars(&bars, Timeframe::M1).expect("same-spacing fold");
    assert_eq!(folded, bars, "one bar per bucket, values untouched");
}

#[test]
fn unsorted_input_refuses_rather_than_folding_shuffled_buckets() {
    let mut bars = ten_minutes();
    bars.swap(2, 7);
    let err = fold_bars(&bars, Timeframe::M5).expect_err("shuffled input refuses");
    assert!(
        matches!(err, StoreError::InvalidBar(_)),
        "misordered input is a caller bug, permanent: {err:?}"
    );
}

#[test]
fn a_duplicate_stamp_refuses() {
    let mut bars = ten_minutes();
    bars[3] = bars[2].clone();
    let err = fold_bars(&bars, Timeframe::M5).expect_err("duplicate stamp refuses");
    assert!(
        matches!(err, StoreError::InvalidBar(_)),
        "an equal stamp is a reorder, never a new bar: {err:?}"
    );
}

#[test]
fn an_invalid_bar_refuses_before_any_bucket_forms() {
    let mut bars = ten_minutes();
    bars[0] = ohlc(0, "100", "90", "99", "95");
    let err = fold_bars(&bars, Timeframe::M5).expect_err("invalid bar refuses");
    assert!(
        matches!(err, StoreError::InvalidBar(_)),
        "fold output answers to the same validator as stored bars: {err:?}"
    );
}

#[test]
fn folding_nothing_yields_nothing() {
    assert_eq!(
        fold_bars(&[], Timeframe::M5).expect("empty fold"),
        Vec::new()
    );
}

#[test]
fn folded_bars_carry_no_adjustment() {
    let folded = fold_bars(&ten_minutes(), Timeframe::M5).expect("aligned fold");
    assert!(
        folded.iter().all(|bar| bar.adj.is_none()),
        "adjustment-aware reads are out of scope: folds never invent one"
    );
}

#[test]
fn sub_weekly_folds_anchor_at_the_epoch() {
    assert_eq!(FOLD_ORIGIN_MS, 0);
    for tf in [
        Timeframe::S5,
        Timeframe::M1,
        Timeframe::M5,
        Timeframe::H1,
        Timeframe::H4,
        Timeframe::D1,
    ] {
        assert_eq!(fold_origin_ms(tf), 0, "{tf} anchors at the epoch");
    }
}

#[test]
fn weekly_folds_anchor_on_the_monday_of_the_epoch_week() {
    assert_eq!(fold_origin_ms(Timeframe::W1), WEEK_FOLD_ORIGIN_MS);
    assert_eq!(
        WEEK_FOLD_ORIGIN_MS, -259_200_000,
        "three days before the epoch"
    );
    let anchor =
        chrono::DateTime::from_timestamp_millis(WEEK_FOLD_ORIGIN_MS).expect("representable anchor");
    assert_eq!(
        anchor.format("%Y-%m-%d %A").to_string(),
        "1969-12-29 Monday",
        "the epoch is a Thursday; weekly buckets start on the Monday"
    );
}

#[test]
fn one_bar_folds_to_itself_at_any_coarser_spacing() {
    let single = vec![bar(0, "100")];
    for tf in [Timeframe::M5, Timeframe::H1, Timeframe::D1] {
        let folded = fold_bars(&single, tf).expect("single-bar fold");
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].o.to_string(), "100");
        assert_eq!(folded[0].c.to_string(), "100");
        assert_eq!(folded[0].volume, 1_000);
    }
}
