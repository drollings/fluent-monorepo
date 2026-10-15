//! Seal economics: the `min_part_rows` cost bound, measured and frozen.
//!
//! This is a **cost** measurement, not an accuracy experiment. Sealing the
//! same corpus at several `min_part_rows` thresholds changes how the rows are
//! chunked into Parquet row groups, and therefore the artifact's byte count —
//! too many tiny trailing groups cost metadata and open IO for no data. The
//! frozen default is the smallest threshold beyond which merging more rows
//! stops reducing the artifact. Wall-clock write time is recorded for the
//! audit but never asserted: it is not deterministic, and this is not an
//! accuracy experiment.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use super::*;
use crate::catalog::{AssetClass, Market, PriceScale};
use crate::key::SymbolId;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use protopyne_types::Timeframe;

/// The chunk size the harness holds fixed while it sweeps the minimum; a
/// production seal uses [`crate::store::DEFAULT_APPEND_CHUNK`].
const CHUNK_ROWS: usize = 32;

/// Rows in the measured corpus.
const CORPUS_ROWS: usize = 200;

/// The thresholds swept, ascending.
const THRESHOLDS: &[usize] = &[1, 8, 16, 32];

/// One recorded cell: artifact size and its row-group count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct Measure {
    /// Artifact size in bytes.
    bytes: u64,
    /// Parquet row groups written.
    groups: usize,
}

/// The frozen economics record.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct Fixture {
    /// The frozen default `min_part_rows` (equals the crate constant).
    frozen_min_part_rows: usize,
    /// Chunk size the sweep held fixed.
    chunk_rows: usize,
    /// Corpus row count.
    corpus_rows: usize,
    /// One cell per swept threshold, keyed by threshold.
    measurements: BTreeMap<String, Measure>,
}

fn market() -> Market {
    Market {
        symbol_id: SymbolId(1),
        symbol: "ECON".to_owned(),
        venue: "test-venue".to_owned(),
        asset_class: AssetClass::Stock,
        price_scale: PriceScale::of(2).expect("valid scale"),
        timeframe: Timeframe::M1,
        time_zone: "UTC".to_owned(),
    }
}

fn measure(corpus: &[protopyne_types::Bar], threshold: usize) -> Measure {
    let dir = tempfile::tempdir().expect("tempdir");
    let market = market();
    let key = TapeKey {
        symbol_id: market.symbol_id,
        timeframe: Timeframe::M1,
    };
    let options = SealOptions::new()
        .row_group_rows(CHUNK_ROWS)
        .min_part_rows(threshold)
        .build();
    let sealed = SnapshotWriter::new(dir.path(), options)
        .write(key, &market, corpus)
        .expect("seal succeeds");
    Measure {
        bytes: sealed.byte_len,
        groups: row_groups(&sealed.path),
    }
}

fn row_groups(path: &Path) -> usize {
    let file = File::open(path).expect("artifact opens");
    ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("parquet metadata reads")
        .metadata()
        .num_row_groups()
}

fn load_fixture() -> Fixture {
    serde_json::from_str(include_str!("data/seal_economics.json"))
        .expect("seal_economics.json parses")
}

#[test]
fn frozen_default_equals_the_crate_constant() {
    let fixture = load_fixture();
    assert_eq!(
        crate::snapshot::DEFAULT_MIN_PART_ROWS, fixture.frozen_min_part_rows,
        "the frozen measurement and the crate default are one value read from \
         two places"
    );
}

#[test]
fn measurements_match_the_recorded_sweep() {
    let fixture = load_fixture();
    assert_eq!(fixture.chunk_rows, CHUNK_ROWS);
    assert_eq!(fixture.corpus_rows, CORPUS_ROWS);
    let corpus = crate::snapshot::test_snapshot::varied_bars(CORPUS_ROWS);
    for &threshold in THRESHOLDS {
        let measured = measure(&corpus, threshold);
        let recorded = fixture
            .measurements
            .get(&threshold.to_string())
            .unwrap_or_else(|| panic!("no recorded measurement for {threshold}"));
        assert_eq!(
            measured, *recorded,
            "threshold {threshold} drifted from the recorded cost measurement"
        );
    }
}

#[test]
fn chunk_plan_matches_the_written_groups() {
    let corpus = crate::snapshot::test_snapshot::varied_bars(CORPUS_ROWS);
    for &threshold in THRESHOLDS {
        let measured = measure(&corpus, threshold);
        let planned = plan_parts(CORPUS_ROWS, CHUNK_ROWS, threshold);
        assert_eq!(
            measured.groups,
            planned.len(),
            "threshold {threshold}: chunks equal row groups"
        );
    }
}
