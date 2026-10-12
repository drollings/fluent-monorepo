use super::*;
use crate::load_bars_with_fallback;
use std::path::PathBuf;

fn test_data_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv")
}

#[test]
fn spy_15m_ingest_golden() {
    let bars = load_bars_csv(&test_data_path()).expect("vendored CSV loads");
    // The trimmed fixture holds 11 data rows (trailing window cut at 15m
    // candle opens, the highest timeframe here) under one header line;
    // the golden pins the true row count, not the line count.
    assert_eq!(bars.len(), 11);
    let first = &bars[0];
    assert_eq!(first.ts.to_rfc3339(), "2026-01-09T22:00:00+00:00");
    assert_eq!(first.o, "693.98".parse().expect("decimal"));
    assert_eq!(first.h, "694.04".parse().expect("decimal"));
    assert_eq!(first.l, "693.82".parse().expect("decimal"));
    assert_eq!(first.c, "693.97".parse().expect("decimal"));
    assert_eq!(first.volume, 1_062);
    let last = &bars[bars.len() - 1];
    assert_eq!(last.ts.to_rfc3339(), "2026-01-10T00:30:00+00:00");
}

#[test]
fn spy_5m_ingest_golden() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_5M.csv");
    let bars = load_bars_csv(&path).expect("vendored SPY_5M loads");
    // Header + 32 trailing-window rows starting exactly on a 15m candle
    // open (three 5m children per kept 15m candle, one genuine tape gap
    // inside). The upstream export carries a trailing `,Plot`
    // header with no data values in any row; the vendored copy drops that
    // empty artifact header (data rows byte-identical) so strict ingest
    // stays strict. See the roadmap findings note.
    assert_eq!(bars.len(), 32);
    let first = &bars[0];
    assert_eq!(first.ts.to_rfc3339(), "2026-01-09T22:00:00+00:00");
    assert_eq!(first.o, "693.98".parse().expect("decimal"));
    assert_eq!(first.h, "694.04".parse().expect("decimal"));
    assert_eq!(first.l, "693.82".parse().expect("decimal"));
    assert_eq!(first.c, "693.99".parse().expect("decimal"));
    assert_eq!(first.volume, 375);
    let last = &bars[bars.len() - 1];
    assert_eq!(last.ts.to_rfc3339(), "2026-01-10T00:40:00+00:00");
    assert_eq!(last.c, "694.38".parse().expect("decimal"));
    assert_eq!(last.volume, 30);
}

#[test]
fn rejects_bad_row_with_line_number() {
    let content = [
        "time,open,high,low,close,Volume",
        "1767344400,686.03,686.62,683.82,686.56,19063",
        "1767345300,686.56,686.57,686.24,686.57,4823",
        "1767346200,not-a-number,686.62,686.38,686.60,1880",
    ]
    .join("\n");
    let err = parse_bars_csv(&content).expect_err("bad row must fail");
    let message = err.to_string();
    assert!(message.contains('4'), "error names line 4, got: {message}");
}

#[test]
fn normalizes_lowercase_volume_header() {
    let content = [
        "time,open,high,low,close,volume",
        "1767344400,686.03,686.62,683.82,686.56,19063",
    ]
    .join("\n");
    let bars = parse_bars_csv(&content).expect("lowercase header accepted");
    assert_eq!(bars.len(), 1);
    assert_eq!(bars[0].volume, 19_063);
}

#[test]
fn rejects_missing_column() {
    let content = ["time,open,high,low,close", "1767344400,1,2,3,4"].join("\n");
    assert!(parse_bars_csv(&content).is_err());
}

#[test]
fn rejects_inverted_ohlc_row_with_line_and_rule() {
    let content = [
        "time,open,high,low,close,Volume",
        "1767344400,686.03,686.62,683.82,686.56,19063",
        "1767345300,686.56,686.24,686.57,686.57,4823",
    ]
    .join("\n");
    let err = parse_bars_csv(&content).expect_err("high below low must refuse");
    match err {
        ServerError::BadRow { line, reason } => {
            assert_eq!(line, 3);
            assert!(reason.contains("high"), "refusal names the rule, got: {reason}");
        }
        other => panic!("must refuse in the BadRow shape, got: {other:?}"),
    }
}

#[test]
fn rejects_duplicate_timestamp_row_with_line_and_rule() {
    let content = [
        "time,open,high,low,close,Volume",
        "1767344400,686.03,686.62,683.82,686.56,19063",
        "1767344400,686.56,686.90,686.24,686.57,4823",
    ]
    .join("\n");
    let err = parse_bars_csv(&content).expect_err("duplicate timestamp must refuse");
    match err {
        ServerError::BadRow { line, reason } => {
            assert_eq!(line, 3);
            assert!(
                reason.contains("strictly increasing"),
                "refusal names the ordering rule, got: {reason}"
            );
        }
        other => panic!("must refuse in the BadRow shape, got: {other:?}"),
    }
}

#[test]
fn rejects_out_of_order_row_with_line_and_rule() {
    let content = [
        "time,open,high,low,close,Volume",
        "1767345300,686.56,686.90,686.24,686.57,4823",
        "1767344400,686.03,686.62,683.82,686.56,19063",
    ]
    .join("\n");
    let err = parse_bars_csv(&content).expect_err("out-of-order timestamp must refuse");
    match err {
        ServerError::BadRow { line, reason } => {
            assert_eq!(line, 3);
            assert!(
                reason.contains("strictly increasing"),
                "refusal names the ordering rule, got: {reason}"
            );
        }
        other => panic!("must refuse in the BadRow shape, got: {other:?}"),
    }
}

#[test]
fn fallback_advances_past_structurally_invalid_file() {
    use std::io::Write as _;
    let mut bad = tempfile::NamedTempFile::new().expect("temp file");
    writeln!(
        bad,
        "time,open,high,low,close,Volume\n1767344400,686.03,683.82,686.62,686.56,19063"
    )
    .expect("write bad csv");
    let bad_path = bad.path().to_path_buf();
    assert!(matches!(
        load_bars_csv(&bad_path),
        Err(ServerError::BadRow { .. })
    ));
    let bars = load_bars_with_fallback(&[bad_path, test_data_path()]).expect("falls through to good file");
    // Same trimmed fixture as `spy_15m_ingest_golden` above: 11 data rows.
    assert_eq!(bars.len(), 11);
}
