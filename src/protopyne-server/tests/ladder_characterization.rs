//! Ladder characterization: the one stop/skip split.
//!
//! The candidate ladder skips a missing or unreadable file to the next
//! candidate and stops on every parse or structural refusal: a bad header, a
//! bad row, or a corrupt Parquet artifact. A corrupt artifact is
//! investigated, not skipped over.

use super::*;
use std::path::PathBuf;

use crate::history::load_bars_candidates;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join(name)
}

fn month_parquet() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs/historical/SPY/2026/03/SPY-2026-03-1M.parquet")
}

#[test]
fn missing_csv_candidate_skips_to_the_next() {
    let missing = data("does-not-exist.csv");
    let bars = load_bars_candidates(&[missing, data("SPY_15M.csv")]).expect("skips missing");
    assert_eq!(bars.len(), 11, "falls through to the usable file");
}

#[test]
fn bad_header_csv_candidate_stops_the_walk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad_path = dir.path().join("bad.csv");
    std::fs::write(&bad_path, "time,open,high\n1,2,3").expect("write");
    assert!(
        matches!(
            crate::csv::load_bars_csv(&bad_path),
            Err(ServerError::BadHeader(_))
        ),
        "a bad header reports BadHeader"
    );
    let err = load_bars_candidates(&[bad_path, data("SPY_15M.csv")])
        .expect_err("a bad header stops the walk");
    assert!(
        matches!(err, ServerError::BadHeader(_)),
        "the ladder stops on BadHeader, got: {err:?}"
    );
}

#[test]
fn bad_row_csv_candidate_stops_the_walk() {
    // A corrupt artifact is investigated, not skipped over: the ladder stops
    // on the row refusal instead of serving the next candidate.
    let dir = tempfile::tempdir().expect("tempdir");
    let bad_path = dir.path().join("bad.csv");
    std::fs::write(
        &bad_path,
        "time,open,high,low,close,Volume\n1767344400,686.03,683.82,686.62,686.56,19063",
    )
    .expect("write bad csv");
    assert!(matches!(
        crate::csv::load_bars_csv(&bad_path),
        Err(ServerError::BadRow { .. })
    ));
    let err = load_bars_candidates(&[bad_path, data("SPY_15M.csv")])
        .expect_err("a bad row stops the walk");
    assert!(
        matches!(err, ServerError::BadRow { .. }),
        "the ladder stops on BadRow, got: {err:?}"
    );
}

#[test]
fn parquet_ladder_skips_missing_and_stops_on_corrupt() {
    let month = month_parquet();
    let missing = month.with_file_name("does-not-exist.parquet");
    let bars = load_bars_candidates(&[missing, month.clone()]).expect("missing skips");
    assert_eq!(bars.len(), 20434);
    let dir = tempfile::tempdir().expect("tempdir");
    let corrupt_path = dir.path().join("corrupt.parquet");
    std::fs::write(&corrupt_path, "this is not parquet").expect("write");
    let err = load_bars_candidates(&[corrupt_path, month]).expect_err("corrupt stops");
    assert!(
        matches!(err, ServerError::Parquet { .. }),
        "corrupt parquet stops in the Parquet shape, got: {err:?}"
    );
}
