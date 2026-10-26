use super::*;
use crate::history::load_bars_candidates;
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
            assert!(
                reason.contains("high"),
                "refusal names the rule, got: {reason}"
            );
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
fn fallback_stops_on_a_structurally_invalid_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad_path = dir.path().join("bad.csv");
    std::fs::write(
        &bad_path,
        "time,open,high,low,close,Volume\n1767344400,686.03,683.82,686.62,686.56,19063",
    )
    .expect("write bad csv");
    assert!(matches!(
        load_bars_csv(&bad_path),
        Err(ServerError::BadRow { .. })
    ));
    let err = load_bars_candidates(&[bad_path, test_data_path()])
        .expect_err("a corrupt artifact stops the walk instead of serving the next file");
    assert!(
        matches!(err, ServerError::BadRow { .. }),
        "the ladder stops in the row shape, got: {err:?}"
    );
}

#[test]
fn shared_decimal_field_reads_valid_and_refuses_malformed() {
    use crate::csv::parse_decimal_field;
    assert_eq!(
        parse_decimal_field("693.98", 2, "close").expect("valid decimal"),
        "693.98".parse().expect("decimal")
    );
    let Err(ServerError::BadRow { line, reason }) = parse_decimal_field("abc", 4, "close") else {
        panic!("malformed decimal refuses as BadRow");
    };
    assert_eq!(line, 4);
    assert!(
        reason.contains("close") && reason.contains("abc"),
        "refusal names the column and the value, got: {reason}"
    );
}

#[test]
fn shared_timestamp_field_reads_epoch_and_refuses_out_of_range() {
    use crate::csv::parse_timestamp_field;
    let ts = parse_timestamp_field("1767344400", 2).expect("valid epoch");
    assert_eq!(ts.to_rfc3339(), "2026-01-02T09:00:00+00:00");
    assert!(
        matches!(
            parse_timestamp_field("not-a-time", 3),
            Err(ServerError::BadRow { .. })
        ),
        "malformed epoch refuses"
    );
    let Err(ServerError::BadRow { line, reason }) = parse_timestamp_field("9223372036854775807", 5)
    else {
        panic!("out-of-range epoch refuses as BadRow");
    };
    assert_eq!(line, 5);
    assert!(
        reason.contains("out of range"),
        "refusal names the range failure, got: {reason}"
    );
}

#[test]
fn shared_int_field_parses_each_width_with_its_column_named() {
    use crate::csv::parse_int_field;
    assert_eq!(
        parse_int_field::<u32>("12", 2, "bid_size").expect("u32"),
        12
    );
    assert_eq!(
        parse_int_field::<u64>("19063", 2, "open_interest").expect("u64"),
        19063
    );
    let Err(ServerError::BadRow { line, reason }) = parse_int_field::<u32>("12.5", 3, "bid_size")
    else {
        panic!("fractional integer refuses as BadRow");
    };
    assert_eq!(line, 3);
    assert!(
        reason.contains("bid_size"),
        "refusal names the column, got: {reason}"
    );
}

#[test]
fn shared_header_positions_map_required_order_and_name_gaps() {
    use crate::csv::header_positions;
    let record = csv::StringRecord::from(vec!["TS", "Strike", "Bid"]);
    let positions =
        header_positions(&record, &["ts", "strike", "bid"]).expect("present columns map");
    assert_eq!(positions, vec![0, 1, 2]);
    let Err(ServerError::BadHeader(message)) = header_positions(&record, &["ts", "strike", "ask"])
    else {
        panic!("missing column refuses as BadHeader");
    };
    assert!(
        message.contains("ask"),
        "refusal names the gap, got: {message}"
    );
}

#[test]
fn shared_optional_column_matches_case_insensitively_or_absent() {
    use crate::csv::optional_column;
    let record = csv::StringRecord::from(vec!["TS", "Bid_Size", "IV"]);
    assert_eq!(optional_column(&record, "ts"), Some(0));
    assert_eq!(optional_column(&record, "bid_size"), Some(1));
    assert_eq!(optional_column(&record, "iv"), Some(2));
    assert_eq!(optional_column(&record, "ask_size"), None);
}
