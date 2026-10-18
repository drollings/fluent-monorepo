use super::*;
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn dec(s: &str) -> Decimal {
    s.parse().expect("valid test decimal")
}

fn bar_at(day: u32, o: &str, h: &str, l: &str, c: &str) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .with_ymd_and_hms(2026, 1, day, 12, 0, 0)
            .single()
            .expect("valid test time"))
        .o(dec(o))
        .h(dec(h))
        .l(dec(l))
        .c(dec(c))
        .volume(100_u64)
        .maybe_adj(None)
        .build()
}

#[test]
fn heikin_first_bar_seeds_on_own_range() {
    let bars = vec![bar_at(2, "100", "102", "99", "101")];
    let out = heikin_ashi(&bars);
    assert_eq!(out.len(), 1);
    // (100+102+99+101)/4 = 100.5 close; (100+101)/2 = 100.5 open.
    assert_eq!(out[0].c, dec("100.5"));
    assert_eq!(out[0].o, dec("100.5"));
    assert_eq!(out[0].ts, bars[0].ts);
    assert_eq!(out[0].volume, 100);
}

#[test]
fn heikin_smooths_through_trend() {
    let bars = vec![
        bar_at(2, "100", "102", "99", "101"),
        bar_at(3, "101", "103", "100", "102"),
    ];
    let out = heikin_ashi(&bars);
    assert_eq!(out.len(), 2);
    // Second open recurs as (100.5 + 100.5)/2 = 100.5.
    assert_eq!(out[1].o, dec("100.5"));
    // Second close is (101+103+100+102)/4 = 101.5.
    assert_eq!(out[1].c, dec("101.5"));
    // Wicks still cover the source extremes.
    assert!(out[1].h >= dec("103"));
    assert!(out[1].l <= dec("100"));
}

#[test]
fn heikin_empty_is_empty() {
    assert!(heikin_ashi(&[]).is_empty());
}

#[test]
fn renko_emits_fixed_bricks_up() {
    // Closes 100 -> 102 -> 104 with brick 1: four up bricks.
    let bars = vec![
        bar_at(2, "100", "100", "100", "100"),
        bar_at(3, "100", "102", "100", "102"),
        bar_at(4, "102", "104", "102", "104"),
    ];
    let bricks = renko_bricks(&bars, dec("1")).expect("positive brick size builds");
    assert_eq!(bricks.len(), 4);
    assert_eq!(bricks[0].o, dec("100"));
    assert_eq!(bricks[0].c, dec("101"));
    assert_eq!(bricks[3].c, dec("104"));
    for brick in &bricks {
        assert!(brick.h >= brick.l);
        assert_eq!(brick.volume, 0);
    }
}

#[test]
fn renko_ignores_sub_brick_noise() {
    let bars = vec![
        bar_at(2, "100", "100", "100", "100"),
        bar_at(3, "100", "100.4", "99.8", "100.4"),
    ];
    let bricks = renko_bricks(&bars, dec("1")).expect("positive brick size builds");
    assert!(bricks.is_empty());
}

#[test]
fn renko_refuses_non_positive_brick() {
    let bars = vec![bar_at(2, "100", "100", "100", "100")];
    assert!(renko_bricks(&bars, Decimal::ZERO).is_err());
    assert!(renko_bricks(&bars, dec("-1")).is_err());
}

#[test]
fn renko_empty_is_empty() {
    let bricks = renko_bricks(&[], dec("1")).expect("empty input builds");
    assert!(bricks.is_empty());
}

fn trend_rows(count: u32) -> Vec<crate::ChartRow> {
    let bars: Vec<Bar> = (0..count)
        .map(|i| {
            let base = 100 + i64::from(i);
            bar_at(
                2 + i,
                &base.to_string(),
                &(base + 2).to_string(),
                &(base - 1).to_string(),
                &(base + 1).to_string(),
            )
        })
        .collect();
    crate::rows_of(&bars).rows
}

#[test]
fn heikin_rows_match_bar_transform_through_conversion() {
    use crate::heikin_rows_of;
    // Integer legs keep every quarter-point exact in both `Decimal` and
    // `f64`, so the display helper must agree bit-for-bit with the
    // bar-backed transform pushed through conversion.
    let bars: Vec<Bar> = (0..8)
        .map(|i| {
            let base = 100 + i64::from(i);
            bar_at(
                2 + i as u32,
                &base.to_string(),
                &(base + 2).to_string(),
                &(base - 1).to_string(),
                &(base + 1).to_string(),
            )
        })
        .collect();
    let rows = crate::rows_of(&bars).rows;
    let converted = crate::rows_of(&crate::heikin_ashi(&bars)).rows;
    assert_eq!(
        heikin_rows_of(&rows),
        converted,
        "one recurrence, two numeric homes"
    );
}

#[test]
fn heikin_cache_windows_keep_overlaps_bit_identical() {
    use crate::heikin_rows_of;
    let full = trend_rows(10);
    let cached = heikin_rows_of(&full);
    assert_eq!(
        cached.len(),
        full.len(),
        "the transform covers the full history"
    );
    for (index, row) in cached.iter().enumerate() {
        assert_eq!(row.time, full[index].time, "times carry over untouched");
        assert_eq!(
            row.volume, full[index].volume,
            "volumes carry over untouched"
        );
    }
    // Windowing the cached transform is a pure slice: windows opened at
    // different offsets agree bit-for-bit on their overlap.
    let early = &cached[2..6];
    let late = &cached[4..8];
    assert_eq!(
        &early[2..],
        &late[..2],
        "overlapping bars never move with the window"
    );
    // Re-seeding per window (the old paint path) shifts the seam open, and
    // the shift propagates down the recursion — which is why the cache
    // must be built once from row zero.
    let reseeded = heikin_rows_of(&full[4..8]);
    assert_ne!(
        reseeded[0].open.to_bits(),
        cached[4].open.to_bits(),
        "a window-seeded open differs from the full-history open"
    );
    assert_ne!(reseeded, late, "the seed shift propagates past the seam");
}

#[test]
fn heikin_rows_empty_is_empty() {
    use crate::heikin_rows_of;
    assert!(heikin_rows_of(&[]).is_empty());
}
