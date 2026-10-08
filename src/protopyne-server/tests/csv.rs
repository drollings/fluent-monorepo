use super::*;
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
    // The source file holds 384 data rows under one header line; the golden
    // pins the true row count, not the line count.
    assert_eq!(bars.len(), 384);
    let first = &bars[0];
    assert_eq!(first.ts.to_rfc3339(), "2026-01-02T09:00:00+00:00");
    assert_eq!(first.o, "686.03".parse().expect("decimal"));
    assert_eq!(first.h, "686.62".parse().expect("decimal"));
    assert_eq!(first.l, "683.82".parse().expect("decimal"));
    assert_eq!(first.c, "686.56".parse().expect("decimal"));
    assert_eq!(first.volume, 19_063);
    let last = &bars[bars.len() - 1];
    assert_eq!(last.ts.to_rfc3339(), "2026-01-10T00:45:00+00:00");
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
