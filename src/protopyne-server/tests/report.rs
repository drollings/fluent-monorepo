use super::*;
use rust_decimal::Decimal;

use protopyne_store::{FoldPolicy, ReadRequest, SymbolId, TapeKey, TimeRange};
use protopyne_types::{Bar, Money, Timeframe};

use crate::backtest::HistorySource;

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
        data_digest: None,
        data_first_ts: None,
        data_last_ts: None,
        data_bar_count: None,
        data_provenance: None,
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
        None,
    );
    assert_eq!(report.bars, 4);
    assert_eq!(report.fill_rate, Some(0.5));
    assert_eq!(report.drawdown, money("6"));
    assert_eq!(report.tail_loss, money("6"));
    assert_eq!(report.capital_efficiency, Some(0.5));
    assert!(
        report.greeks_note.contains("unavailable"),
        "greeks must be marked unavailable, got: {}",
        report.greeks_note
    );
    assert_eq!(report.data_limits, vec!["no chain data".to_owned()]);
}

#[test]
fn report_with_no_proposals_has_no_fill_rate() {
    let report = build_report(
        manifest(0),
        Vec::new(),
        Vec::new(),
        0,
        0,
        money("0"),
        money("0"),
        money("0"),
        Vec::new(),
        None,
    );
    assert_eq!(report.fill_rate, None);
    assert_eq!(report.drawdown, money("0"));
}

#[test]
fn manifest_canonical_json_reports_loudly() {
    // The canonical pin must carry the pinned data, never an empty object:
    // a serialization failure reports inside the payload instead.
    let json = manifest(384).to_canonical_json();
    assert!(json.contains("spy-15m-v1"), "pin data present, got: {json}");
    assert!(
        !json.contains("serialization_error"),
        "clean pins serialize cleanly"
    );
}

#[test]
fn fill_rate_survives_counts_past_u32() {
    // Counts widen directly to f64: the old `as u32` detour truncated
    // anything past 2^32 into a wrong rate (2^32 fills over 2^32 proposals
    // read NaN).
    let even = build_report(
        manifest(1),
        Vec::new(),
        Vec::new(),
        1_u64 << 32,
        1_u64 << 32,
        money("0"),
        money("0"),
        money("0"),
        Vec::new(),
        None,
    );
    assert_eq!(even.fill_rate, Some(1.0));
    let half = build_report(
        manifest(1),
        Vec::new(),
        Vec::new(),
        1_u64 << 33,
        1_u64 << 32,
        money("0"),
        money("0"),
        money("0"),
        Vec::new(),
        None,
    );
    assert_eq!(half.fill_rate, Some(0.5));
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

#[test]
fn return_conversions_round_trip() {
    use crate::report::{log_to_simple_return, simple_to_log_return};
    for rate in ["0.0", "0.1", "-0.1", "1.5", "-0.5"] {
        let simple = decimal(rate);
        let log = simple_to_log_return(simple).expect("defined");
        let back = log_to_simple_return(log).expect("defined");
        let drift = (back - simple).abs();
        assert!(
            drift < decimal("0.000000000001"),
            "round-trip drifts {drift} on {rate}"
        );
    }
    // log(1 + rate) is undefined at and below -100%.
    assert!(simple_to_log_return(decimal("-1")).is_none());
    assert!(simple_to_log_return(decimal("-2")).is_none());
    // exp overflows to infinity: honest None, never an infinite Decimal.
    assert!(log_to_simple_return(decimal("1000")).is_none());
}

#[test]
fn cum_return_on_known_curve() {
    use crate::report::cum_return;
    assert_eq!(
        cum_return(&[decimal("100"), decimal("120"), decimal("90")]),
        Some(decimal("-0.1"))
    );
    assert_eq!(
        cum_return(&[decimal("100"), decimal("100")]),
        Some(Decimal::ZERO)
    );
    // Leading idle zeros are skipped like the reference skips nulls: the
    // base is the first deployed capital.
    assert_eq!(
        cum_return(&[Decimal::ZERO, Decimal::ZERO, decimal("120"), decimal("90")]),
        Some(decimal("-0.25"))
    );
    assert!(cum_return(&[]).is_none());
    assert!(cum_return(&[decimal("50")]).is_none());
    // No deployed base at all: nothing to compound from.
    assert!(cum_return(&[Decimal::ZERO, Decimal::ZERO]).is_none());
}

#[test]
fn report_carries_cum_return_and_rsrs_as_json_keys() {
    let curve = vec![money("100"), money("120"), money("90")];
    let report = build_report(
        manifest(3),
        Vec::new(),
        Vec::new(),
        0,
        0,
        money("0"),
        money("0"),
        money("0"),
        curve,
        Some(2.5),
    );
    assert!((report.cum_return.expect("cum_return") - -0.1).abs() < 1e-12);
    assert_eq!(report.rsrs, Some(2.5));
    let json = serde_json::to_string(&report).expect("serializable");
    assert!(
        json.contains("\"cum_return\""),
        "sole serialization change carries cum_return, got: {json}"
    );
    assert!(
        json.contains("\"rsrs\""),
        "sole serialization change carries rsrs, got: {json}"
    );
    // Honest None survives the round-trip as explicit null.
    let empty = build_report(
        manifest(0),
        Vec::new(),
        Vec::new(),
        0,
        0,
        money("0"),
        money("0"),
        money("0"),
        Vec::new(),
        None,
    );
    assert_eq!(empty.cum_return, None);
    assert_eq!(empty.rsrs, None);
}

fn tape_source() -> HistorySource {
    HistorySource::Tape {
        key: TapeKey {
            symbol_id: SymbolId(1),
            timeframe: Timeframe::M15,
        },
        request: ReadRequest {
            symbol: "SPY".to_owned(),
            venue: "test-venue".to_owned(),
            timeframe: Timeframe::M15,
            range: TimeRange::new(None, None).expect("unbounded range"),
            limit: None,
            policy: FoldPolicy::ExtendToFullBuckets,
        },
    }
}

#[test]
fn each_source_arm_names_its_own_history() {
    // A file run and a store-backed tape must not share one "single CSV" line:
    // the arm the report names is the arm that was replayed.
    let version = "spy-15m-v1";
    assert_eq!(
        history_source_limit(HistoryKind::of(&HistorySource::Files(vec![])), version, 11),
        "file history 'spy-15m-v1', 11 bars"
    );
    assert_eq!(
        history_source_limit(HistoryKind::of(&HistorySource::Dynamic(vec![])), version, 11),
        "in-memory history 'spy-15m-v1', 11 bars"
    );
    assert_eq!(
        history_source_limit(HistoryKind::of(&tape_source()), version, 11),
        "store-backed history 'spy-15m-v1', 11 bars"
    );
}

#[test]
fn a_source_missing_provenance_names_the_omission() {
    let omission = uncertified_coverage_limit(false).expect("an uncertified source names it");
    assert!(
        omission.contains("provenance"),
        "the omission names the missing planner provenance, got: {omission}"
    );
    assert!(
        uncertified_coverage_limit(true).is_none(),
        "a planner-backed source adds no omission line"
    );
}

#[test]
fn absent_capabilities_name_the_prototype_gaps() {
    assert!(ABSENT_CAPABILITIES.contains("option-chain"));
    assert!(ABSENT_CAPABILITIES.contains("corporate-action"));
    assert!(ABSENT_CAPABILITIES.contains("single symbol"));
}
