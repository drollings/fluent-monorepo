use super::*;
use rust_decimal::Decimal;

use protopyne_types::{Bar, Money};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn manifest(bar_count: u64) -> RunManifest {
    RunManifest {
        data_version: "spy-15m-v1".to_owned(),
        code_version: env!("CARGO_PKG_VERSION").to_owned(),
        config_json: "{}".to_owned(),
        bar_count,
        first_ts: "2026-01-02T09:00:00+00:00".to_owned(),
        last_ts: "2026-01-10T00:45:00+00:00".to_owned(),
        seed: 7,
    }
}

fn decimal(value: &str) -> Decimal {
    value.parse::<Decimal>().expect("decimal")
}

#[test]
fn manifest_serialization_is_deterministic() {
    let first = manifest(384);
    let second = manifest(384);
    assert_eq!(first.to_canonical_json(), second.to_canonical_json());
    assert!(first.to_canonical_json().contains("spy-15m-v1"));
    let changed = manifest(100);
    assert_ne!(first.to_canonical_json(), changed.to_canonical_json());
}

#[test]
fn manifest_pins_code_version() {
    let pinned = RunManifest::new("v1", "{}", &[], 0);
    assert_eq!(pinned.code_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(pinned.bar_count, 0);
}

#[test]
fn drawdown_and_tail_loss_match_hand_computation() {
    let equity = vec![
        decimal("0"),
        decimal("100"),
        decimal("60"),
        decimal("120"),
        decimal("90"),
    ];
    assert_eq!(max_drawdown(&equity), decimal("40"));
    assert_eq!(worst_step_loss(&equity), decimal("40"));
    assert_eq!(max_drawdown(&[]), Decimal::ZERO);
    assert_eq!(worst_step_loss(&[decimal("5")]), Decimal::ZERO);
}

#[test]
fn capital_efficiency_reports_none_when_nothing_reserved() {
    assert!(capital_efficiency(decimal("10"), Decimal::ZERO).is_none());
    let efficiency = capital_efficiency(decimal("50"), decimal("100")).expect("ratio");
    assert!((efficiency - 0.5).abs() < 1e-9);
}

#[test]
fn report_states_limits_and_unavailable_greeks() {
    let pinned = manifest(4);
    let curve = vec![money("0"), money("10"), money("4"), money("12")];
    let report = build_report(
        pinned,
        vec!["synthetic quotes".to_owned()],
        vec!["no chain data".to_owned()],
        2,
        1,
        money("0.65"),
        money("100"),
        money("200"),
        curve,
    );
    assert_eq!(report.bars, 4);
    assert_eq!(report.fill_rate, Some(0.5));
    assert_eq!(report.drawdown, money("6"));
    assert_eq!(report.tail_loss, money("6"));
    assert_eq!(report.capital_efficiency, Some(0.5));
    assert!(report.greeks_note.contains("unavailable"), "greeks must be marked unavailable, got: {}", report.greeks_note);
    assert_eq!(report.data_limits, vec!["no chain data".to_owned()]);
}

#[test]
fn report_with_no_proposals_has_no_fill_rate() {
    let report = build_report(manifest(0), Vec::new(), Vec::new(), 0, 0, money("0"), money("0"), money("0"), Vec::new());
    assert_eq!(report.fill_rate, None);
    assert_eq!(report.drawdown, money("0"));
}

#[test]
fn unquotable_bar_counter_matches_zero_closes() {
    use chrono::{TimeZone, Utc};
    let good = Bar::new()
        .ts(Utc.timestamp_opt(1_767_344_400, 0).unwrap())
        .o(decimal("1"))
        .h(decimal("1"))
        .l(decimal("1"))
        .c(decimal("1"))
        .volume(1)
        .maybe_adj(None)
        .build();
    let mut bad = good.clone();
    bad.c = Decimal::ZERO;
    assert_eq!(count_unquotable_bars(&[good, bad]), 1);
}
