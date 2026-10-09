use super::*;
use protopyne_types::Bar;
use std::path::PathBuf;

fn sample_bars() -> Vec<Bar> {
    use chrono::TimeZone as _;
    use rust_decimal::Decimal;
    (0..12)
        .map(|i| {
            let close = Decimal::from(100 + i);
            Bar::new()
                .ts(chrono::Utc
                    .with_ymd_and_hms(2026, 1, 2, 9, 0, i)
                    .single()
                    .expect("valid timestamp"))
                .o(close)
                .h(close)
                .l(close)
                .c(close)
                .volume(1_000_u64)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name)
}

#[test]
fn identity_expr_round_trips_closes() {
    let bars = sample_bars();
    let got = expr_fixture(&bars, col("close")).expect("identity evaluates");
    assert_eq!(got.len(), bars.len());
    for (i, value) in got.iter().enumerate() {
        assert_eq!(*value, Some(100.0 + i as f64), "close drift at {i}");
    }
}

#[test]
fn arithmetic_expr_evaluates_row_wise() {
    let bars = sample_bars();
    let got = expr_fixture(&bars, (col("close") * lit(2.0)).alias("doubled"))
        .expect("arithmetic evaluates");
    assert_eq!(got.len(), bars.len());
    for (i, value) in got.iter().enumerate() {
        assert_eq!(*value, Some(200.0 + 2.0 * i as f64), "doubled drift at {i}");
    }
}

#[test]
fn null_literal_maps_to_none_never_nan() {
    let bars = sample_bars();
    // Height-preserving conditional null: every close exceeds zero, so the
    // null branch wins on every row and the output is full-height null.
    let expr = when(col("close").gt(lit(0.0)))
        .then(lit(NULL))
        .otherwise(col("close"))
        .alias("n");
    let got = expr_fixture(&bars, expr).expect("null evaluates");
    assert_eq!(got.len(), bars.len());
    assert!(got.iter().all(Option::is_none));
    // No NaN leak: every slot is a genuine `None`, never `Some(NaN)`.
    assert!(
        got.iter().all(|v| v.is_none_or(f64::is_finite)),
        "NaN leaked through the null mapping: {got:?}"
    );
}

#[test]
fn parity_match_is_ok_including_warmup_nones() {
    let series = vec![None, None, Some(1.5)];
    assert!(assert_expr_parity(&series, &series, 1e-9, "match").is_ok());
}

#[test]
fn parity_mismatch_names_every_index_not_just_first() {
    let actual = vec![
        Some(1.0),
        Some(2.0),
        Some(99.0),
        Some(4.0),
        Some(77.0),
    ];
    let expected = vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)];
    let err =
        assert_expr_parity(&actual, &expected, 1e-9, "planted").expect_err("must report drift");
    // Both drifted positions are named, not just the first one.
    assert!(err.contains("position 2"), "missing index 2: {err}");
    assert!(err.contains("position 4"), "missing index 4: {err}");
    // Both sides of each drift are named.
    assert!(err.contains("99") && err.contains("3"), "missing sides: {err}");
    // Largest drift |99 - 3| = 96 is named.
    assert!(err.contains("96"), "missing max drift: {err}");
}

#[test]
fn parity_none_vs_some_is_missing_not_silent() {
    let err = assert_expr_parity(&[Some(1.0)], &[None], 1e-9, "na")
        .expect_err("None vs Some must report");
    assert!(err.contains("position 0"), "missing index: {err}");
}

#[test]
fn fixture_csv_header_convention_pinned() {
    let text =
        std::fs::read_to_string(fixture_path("parity_sma10.csv")).expect("fixture present");
    let mut lines = text.lines();
    assert_eq!(lines.next().expect("header line"), "bar,close,sma10");
    let rows: Vec<&str> = lines.collect();
    assert_eq!(rows.len(), 12);
    assert_eq!(rows[0], "0,100.0,");
    assert_eq!(rows[9], "9,109.0,104.5");
}
