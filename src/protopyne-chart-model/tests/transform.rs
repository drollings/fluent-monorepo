use super::*;
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn dec(s: &str) -> Decimal {
    s.parse().expect("valid test decimal")
}

fn bar_at(day: u32, o: &str, h: &str, l: &str, c: &str) -> Bar {
    Bar::new()
        .ts(chrono::Utc.with_ymd_and_hms(2026, 1, day, 12, 0, 0).single().expect("valid test time"))
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
    let bars =
        vec![bar_at(2, "100", "100", "100", "100"), bar_at(3, "100", "100.4", "99.8", "100.4")];
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
