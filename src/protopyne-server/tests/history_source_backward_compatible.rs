//! The control group for the tape arm: the files and dynamic arms are
//! unchanged, and the ingest goldens still hold.
//!
//! Adding a third arm must not move the two that already existed. The files
//! arm walks the one candidate ladder through the trimmed ingest goldens
//! (11-row 15M, 32-row 5M); the dynamic arm replays verbatim; both carry no
//! planner provenance; and the new tape arm refuses loudly when it is asked
//! to read without a store rather than falling back.

use std::path::PathBuf;

use crate::backtest::HistorySource;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join(name)
}

#[test]
fn files_arm_matches_the_candidate_loader_and_carries_no_provenance() {
    let candidates = vec![data("SPY_15M.csv")];
    let via_source =
        crate::load_history(&HistorySource::Files(candidates.clone()), None).expect("files load");
    let via_loader =
        crate::history::load_bars_candidates(&candidates).expect("candidates load");
    assert_eq!(via_source.bars, via_loader, "the files arm is the candidate loader");
    assert!(via_source.provenance.is_empty(), "a file arm names no planner span");
}

#[test]
fn the_ingest_goldens_still_hold() {
    // The 15M fixture is 11 bars; the 5M fixture is 32, the trimmed ingest
    // goldens pinned by `csv.rs`.
    let m15 = crate::load_history(&HistorySource::Files(vec![data("SPY_15M.csv")]), None)
        .expect("15m loads")
        .bars;
    let m5 = crate::load_history(&HistorySource::Files(vec![data("SPY_5M.csv")]), None)
        .expect("5m loads")
        .bars;
    assert_eq!(m15.len(), 11, "the 11-row 15M golden is unchanged");
    assert_eq!(m5.len(), 32, "the 32-row 5M golden is unchanged");
}

#[test]
fn dynamic_arm_replays_verbatim_and_refuses_empty() {
    let bars = crate::csv::load_bars_csv(&data("SPY_15M.csv")).expect("loads");
    let replayed =
        crate::load_history(&HistorySource::Dynamic(bars.clone()), None).expect("dynamic loads");
    assert_eq!(replayed.bars, bars, "the dynamic arm replays verbatim");
    assert!(replayed.provenance.is_empty(), "an in-memory arm names no planner span");
    assert!(
        crate::load_history(&HistorySource::Dynamic(Vec::new()), None).is_err(),
        "an empty dynamic tape refuses like an empty candidate set"
    );
}

#[test]
fn tape_arm_never_silently_falls_back_to_another_source() {
    use protopyne_store::{TapeKey, SymbolId};
    use protopyne_types::Timeframe;

    let source = HistorySource::Tape {
        key: TapeKey { symbol_id: SymbolId(1), timeframe: Timeframe::M15 },
        request: protopyne_store::ReadRequest {
            symbol: "SPY".to_owned(),
            venue: "test-venue".to_owned(),
            timeframe: Timeframe::M15,
            range: protopyne_store::TimeRange { start_ms: None, end_ms: None },
            limit: Some(10),
            policy: protopyne_store::FoldPolicy::ExtendToFullBuckets,
        },
    };
    assert!(
        crate::load_history(&source, None).is_err(),
        "a tape read with no store must refuse, never substitute another source"
    );
}
