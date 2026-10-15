//! The data-limits rendering gate: `build_report` names the tape's coverage
//! and any partial span in `data_limits`, and the tearsheet shows the same
//! facts — two renderings of one struct, never two computations.
//!
//! The manifest here carries hand-pinned tape facts (the digest and span
//! computation is the rerun suite's subject, not this one's): what matters
//! here is that coverage in means coverage out, in both renderings, and
//! that a manifest with no tape adds no line.

use super::*;
use rust_decimal::Decimal;

use protopyne_types::Money;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn taped_manifest() -> RunManifest {
    RunManifest {
        data_version: "spy-15m-v1".to_owned(),
        code_version: env!("CARGO_PKG_VERSION").to_owned(),
        config_json: "{}".to_owned(),
        bar_count: 5,
        first_ts: "2026-01-09T16:00:00+00:00".to_owned(),
        last_ts: "2026-01-09T16:04:00+00:00".to_owned(),
        seed: 7,
        data_digest: Some("0123456789abcdef".to_owned()),
        data_first_ts: Some("2026-01-09T16:00:00+00:00".to_owned()),
        data_last_ts: Some("2026-01-09T16:04:00+00:00".to_owned()),
        data_bar_count: Some(5),
        data_provenance: Some(vec![
            SpanOriginSummary {
                origin: "stored".to_owned(),
                complete: true,
                start_ms: Some(1_767_974_400_000),
                end_ms: Some(1_767_974_700_000),
            },
            SpanOriginSummary {
                origin: "stored".to_owned(),
                complete: false,
                start_ms: Some(1_767_974_700_000),
                end_ms: Some(1_767_975_000_000),
            },
        ]),
    }
}

fn bare_report() -> BacktestReport {
    build_report(
        taped_manifest(),
        vec!["synthetic quotes".to_owned()],
        vec!["no chain data".to_owned()],
        0,
        0,
        money("0"),
        money("0"),
        money("0"),
        Vec::new(),
        None,
    )
}

#[test]
fn build_report_names_tape_coverage_and_partial_spans() {
    let report = bare_report();
    let line = report
        .data_limits
        .iter()
        .find(|limit| limit.starts_with("tape data:"))
        .expect("a tape-data line joins data_limits");
    assert_eq!(
        line,
        "tape data: 5 bars 2026-01-09T16:00:00+00:00..2026-01-09T16:04:00+00:00 digest 01234567 | 2 stored spans (1 whole, 1 partial)",
        "the line names coverage, the digest head, and the uncertified remainder"
    );
    assert_eq!(
        report.data_limits.len(),
        2,
        "the tape line is additive: caller lines stay untouched"
    );
}

#[test]
fn tearsheet_shows_the_same_tape_facts() {
    let report = bare_report();
    let sheet = render_tearsheet(&report);
    assert!(
        sheet.contains("tape data: 5 bars 2026-01-09T16:00:00+00:00..2026-01-09T16:04:00+00:00"),
        "the tearsheet names the same coverage:\n{sheet}"
    );
    assert!(
        sheet.contains("1 partial"),
        "the tearsheet names the same uncertified remainder:\n{sheet}"
    );
}

#[test]
fn manifest_without_tape_adds_no_line() {
    let untaped = RunManifest {
        data_digest: None,
        data_first_ts: None,
        data_last_ts: None,
        data_bar_count: None,
        data_provenance: None,
        ..taped_manifest()
    };
    let report = build_report(
        untaped,
        Vec::new(),
        vec!["no chain data".to_owned()],
        0,
        0,
        money("0"),
        money("0"),
        money("0"),
        Vec::new(),
        None,
    );
    assert_eq!(
        report.data_limits,
        vec!["no chain data".to_owned()],
        "no tape in means no tape line out"
    );
}
