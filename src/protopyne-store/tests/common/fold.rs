//! The fold-calibration harness: golden loading plus the accept tally.
//!
//! One home for "read the frozen goldens, run every check, count what held",
//! shared by the calibration gate and the stability gate so the two can never
//! disagree about what was measured. The tally mirrors the kernels'
//! `ParityDashboard` shape — rows plus an accept rate plus a single green
//! predicate — rather than inventing a second calibration idiom: green is
//! every row accepted, and the cross-timeframe row carries the measured
//! agreement the fixture froze.
//!
//! Included once from `fold.rs`; consumer suites `use` it, never include it
//! again.
//!
//! `Goldens :: note` and the `note` fields below are record-keeping: they are
//! read by the engineer auditing the fixture and by `Debug`, not by the
//! tally, so dead-code is allowed here rather than threaded through fake
//! uses.

#![allow(dead_code)]

use chrono::DateTime;
use protopyne_types::{Bar, BarWindow, Timeframe};
use serde::Deserialize;

use crate::fold::{bucket_start, fold_bars};

/// The frozen goldens as recorded on disk.
#[derive(Debug, Clone, Deserialize)]
pub struct Goldens {
    /// What the file freezes and why figures stay unrounded.
    pub note: String,
    /// `(timeframe, ts_ms) -> bucket_ms`, integers with no tolerance.
    pub boundaries: Vec<Boundary>,
    /// The bar-complete minute run the gap oracle folds.
    pub complete_run: CompleteRun,
    /// The measured minute-to-fifteen-minute agreement.
    pub agreement: Agreement,
}

/// One frozen boundary expectation.
#[derive(Debug, Clone, Deserialize)]
pub struct Boundary {
    /// The spacing folded at.
    pub timeframe: Timeframe,
    /// The stamp folded.
    pub ts_ms: i64,
    /// The bucket the stamp must land in.
    pub bucket_ms: i64,
    /// Why this stamp belongs in the table.
    pub note: String,
}

/// The bar-complete run the oracle folds, and the genuine hole that bounds
/// it.
#[derive(Debug, Clone, Deserialize)]
pub struct CompleteRun {
    /// Fixture file under the kernels test-data directory.
    pub fixture: String,
    /// Bars the file holds: the head run plus the torn tail the oracle skips.
    pub total_bars: usize,
    /// Bars the head run holds.
    pub bars: usize,
    /// First stamp of the head run, epoch milliseconds.
    pub first_ms: i64,
    /// Last stamp of the head run, epoch milliseconds.
    pub last_ms: i64,
    /// The spacing the oracle folds the run to.
    pub fold_to: Timeframe,
    /// Buckets the fold must produce.
    pub buckets: usize,
    /// The five-minute bucket the full-file fold must report missing.
    pub genuine_hole_bucket_ms: i64,
    /// Why the oracle folds the head run rather than the file.
    pub note: String,
    /// Why the recorded hole is genuine rather than a fold artifact.
    pub hole_note: String,
}

/// The recorded cross-timeframe agreement.
#[derive(Debug, Clone, Deserialize)]
pub struct Agreement {
    /// Minute fixture folded.
    pub from: String,
    /// Fifteen-minute fixture compared against.
    pub to: String,
    /// Fields compared exactly: prices as decimals, volume as integer.
    pub fields: Vec<String>,
    /// Fifteen-minute buckets the vendor fixture holds.
    pub vendor_buckets: usize,
    /// Buckets fully covered by minute bars and therefore comparable.
    pub comparable: usize,
    /// Comparable buckets agreeing exactly.
    pub matched: usize,
    /// Why the comparable set is smaller than the vendor set.
    pub reason: String,
}

/// One measured row: the check, what was recorded, what was measured.
#[derive(Debug, Clone, PartialEq)]
pub struct FoldRow {
    /// The check, as recorded.
    pub name: String,
    /// The recorded expectation, rendered.
    pub expected: String,
    /// The measured value, rendered.
    pub actual: String,
    /// Whether the measurement matched the record.
    pub accepted: bool,
}

/// The accept tally over every fold check.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FoldTally {
    /// One row per check, in measurement order.
    pub rows: Vec<FoldRow>,
}

impl FoldTally {
    /// Every spacing the sweep covers: the full span table, stated
    /// explicitly so a new variant is a visible addition here rather than a
    /// silent absence.
    const SWEEP_SPACINGS: [Timeframe; 15] = [
        Timeframe::S5,
        Timeframe::S10,
        Timeframe::S15,
        Timeframe::S30,
        Timeframe::M1,
        Timeframe::M3,
        Timeframe::M5,
        Timeframe::M10,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H2,
        Timeframe::H4,
        Timeframe::D1,
        Timeframe::W1,
    ];

    /// Measure every check against the frozen goldens: boundary exactness,
    /// sweep idempotence, the oracle in both directions, the genuine hole,
    /// and the cross-timeframe agreement. Pure and deterministic: file reads
    /// plus arithmetic, no clock, no randomness.
    #[must_use]
    pub fn measure(goldens: &Goldens) -> Self {
        let mut tally = Self::default();
        tally.check_boundaries(&goldens.boundaries);
        tally.check_idempotence();
        let minute = load_spy(&goldens.complete_run.fixture);
        let run = head_run(&minute, &goldens.complete_run);
        tally.check_oracle_positive(&run, &goldens.complete_run);
        tally.check_oracle_negative(&run, &goldens.complete_run);
        tally.check_genuine_hole(&minute, &goldens.complete_run);
        tally.check_agreement(&minute, &goldens.agreement);
        tally
    }

    /// Fraction of checks accepted.
    #[must_use]
    pub fn accept_rate(&self) -> f64 {
        if self.rows.is_empty() {
            return 0.0;
        }
        self.rows.iter().filter(|row| row.accepted).count() as f64 / self.rows.len() as f64
    }

    /// Whether the fold may be trusted: every check accepted.
    #[must_use]
    pub fn is_green(&self) -> bool {
        !self.rows.is_empty() && (self.accept_rate() - 1.0).abs() < f64::EPSILON
    }

    fn push(&mut self, name: &str, expected: String, actual: String) {
        let accepted = expected == actual;
        self.rows.push(FoldRow {
            name: name.to_owned(),
            expected,
            actual,
            accepted,
        });
    }

    fn check_boundaries(&mut self, boundaries: &[Boundary]) {
        for boundary in boundaries {
            let actual = bucket_start(boundary.timeframe, boundary.ts_ms);
            self.push(
                &format!("{:?} @ {}", boundary.timeframe, boundary.ts_ms),
                format!("bucket {}", boundary.bucket_ms),
                format!("bucket {actual}"),
            );
        }
    }

    fn check_idempotence(&mut self) {
        let stamps = sweep_stamps();
        let mut fixed = 0_usize;
        let mut first_drift = None;
        for timeframe in Self::SWEEP_SPACINGS {
            for ts in &stamps {
                let once = bucket_start(timeframe, *ts);
                if bucket_start(timeframe, once) == once {
                    fixed += 1;
                } else if first_drift.is_none() {
                    first_drift = Some((timeframe, *ts, once));
                }
            }
        }
        let total = Self::SWEEP_SPACINGS.len() * stamps.len();
        self.push(
            "idempotence sweep",
            format!("{total}/{total} fixed points"),
            match first_drift {
                None => format!("{fixed}/{total} fixed points"),
                Some((timeframe, ts, once)) => {
                    format!("{fixed}/{total} fixed points; {timeframe:?} @ {ts} drifts via {once}")
                }
            },
        );
    }

    fn check_oracle_positive(&mut self, run: &[Bar], spec: &CompleteRun) {
        let folded = fold_bars(run, spec.fold_to).expect("the head run folds");
        let mut window = BarWindow::new(spec.fold_to);
        for bar in folded.iter().cloned() {
            window.push(bar).expect("folded bars arrive ascending");
        }
        self.push(
            "oracle accepts the complete fold",
            bucket_report(spec.buckets, 0),
            bucket_report(folded.len(), window.gaps().len()),
        );
    }

    fn check_oracle_negative(&mut self, run: &[Bar], spec: &CompleteRun) {
        let folded = fold_bars(run, spec.fold_to).expect("the head run folds");
        let removed = folded.len() / 2;
        let removed_ts = folded[removed].ts.timestamp_millis();
        let torn: Vec<Bar> = folded
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != removed)
            .map(|(_, bar)| bar.clone())
            .collect();
        let mut window = BarWindow::new(spec.fold_to);
        for bar in torn.iter().cloned() {
            window
                .push(bar)
                .expect("the torn fold still arrives ascending");
        }
        let gaps = window.gaps();
        let expecting = gaps
            .first()
            .map_or(-1, |gap| gap.expected_ts.timestamp_millis());
        self.push(
            "oracle names the removed bar",
            gap_report(1, removed_ts),
            gap_report(gaps.len(), expecting),
        );
    }

    fn check_genuine_hole(&mut self, minute: &[Bar], spec: &CompleteRun) {
        let folded = fold_bars(minute, spec.fold_to).expect("the file folds");
        let mut window = BarWindow::new(spec.fold_to);
        for bar in folded.iter().cloned() {
            window.push(bar).expect("folded bars arrive ascending");
        }
        let gaps = window.gaps();
        let expecting = gaps
            .first()
            .map_or(-1, |gap| gap.expected_ts.timestamp_millis());
        self.push(
            "the fold reproduces the genuine hole",
            gap_report(1, spec.genuine_hole_bucket_ms),
            gap_report(gaps.len(), expecting),
        );
    }

    fn check_agreement(&mut self, minute: &[Bar], agreement: &Agreement) {
        let vendor = load_spy(&agreement.to);
        let (comparable, matched) = agreement_against(minute, &vendor);
        self.push(
            "cross-timeframe agreement",
            format!(
                "{}/{} of {}",
                agreement.matched, agreement.comparable, agreement.vendor_buckets
            ),
            format!("{matched}/{comparable} of {}", vendor.len()),
        );
    }
}

/// Render a fold outcome the way the tally compares it: bucket count plus
/// the gap verdict, so the recorded and measured sides share one spelling.
fn bucket_report(buckets: usize, gaps: usize) -> String {
    if gaps == 0 {
        format!("{buckets} buckets, no gaps")
    } else {
        format!("{buckets} buckets, {gaps} gaps")
    }
}

/// Render a gap verdict the way the tally compares it: gap count plus the
/// expected stamp, so the recorded and measured sides share one spelling.
fn gap_report(gaps: usize, expecting: i64) -> String {
    format!("{gaps} gap(s) expecting {expecting}")
}

/// The idempotence sweep: boundary-adjacent stamps, the epoch in both
/// directions, the DST instants, and a present-day stamp — all far inside
/// the saturating rails, where buckets are promises rather than approximations.
fn sweep_stamps() -> Vec<i64> {
    let mut stamps = vec![-1, 0, 1, -60_000, 60_000, -604_800_000, 604_800_000];
    for anchor in [
        1_706_745_600_000, // 2024-02-01T00:00Z, the month line
        1_704_067_200_000, // 2024-01-01T00:00Z, the year line (a Monday)
        1_710_054_000_000, // 2024-03-10T07:00Z, the spring-forward instant
        1_730_613_600_000, // 2024-11-03T06:00Z, the fall-back instant
        1_767_974_400_000, // the fixture's first minute bar
    ] {
        stamps.extend([anchor - 1, anchor, anchor + 1]);
    }
    stamps
}

/// Read the frozen goldens.
pub fn load_goldens() -> Goldens {
    serde_json::from_str(include_str!("../data/fold_goldens.json"))
        .expect("fold_goldens.json parses")
}

/// Parse a frozen kernels history fixture into bars.
///
/// A frozen-shape loader, not a second CSV ingest: the header is asserted
/// byte-exact so a fixture change fails loudly here instead of misparsing a
/// column downstream.
pub fn load_spy(name: &str) -> Vec<Bar> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let content = std::fs::read_to_string(root.join("../protopyne-kernels/tests/data").join(name))
        .unwrap_or_else(|e| panic!("fixture {name} reads: {e}"));
    let mut lines = content.lines();
    let header = lines.next().expect("the fixture has a header row");
    assert_eq!(
        header, "time,open,high,low,close,Volume",
        "the fixture shape is frozen; a changed header is a changed fixture"
    );
    lines
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            let mut cells = line.split(',');
            let secs: i64 = cells
                .next()
                .expect("every row stamps")
                .parse()
                .unwrap_or_else(|e| panic!("row {}: bad stamp: {e}", index + 2));
            let dec = |cell: Option<&str>| {
                cell.unwrap_or_else(|| panic!("row {}: short row", index + 2))
                    .parse()
                    .unwrap_or_else(|e| panic!("row {}: bad decimal: {e}", index + 2))
            };
            let bar = Bar::new()
                .ts(DateTime::from_timestamp(secs, 0).expect("fixture stamps are in range"))
                .o(dec(cells.next()))
                .h(dec(cells.next()))
                .l(dec(cells.next()))
                .c(dec(cells.next()))
                .volume(
                    cells
                        .next()
                        .unwrap_or_else(|| panic!("row {}: no volume", index + 2))
                        .parse()
                        .unwrap_or_else(|e| panic!("row {}: bad volume: {e}", index + 2)),
                )
                .maybe_adj(None)
                .build();
            bar.validate()
                .unwrap_or_else(|e| panic!("row {}: invalid bar: {e}", index + 2));
            bar
        })
        .collect()
}

/// The bar-complete head run the oracle folds: the recorded prefix, pinned
/// by its stamps so a fixture change that moves the tear fails here rather
/// than folding a torn run quietly.
fn head_run(minute: &[Bar], spec: &CompleteRun) -> Vec<Bar> {
    assert_eq!(
        minute.len(),
        spec.total_bars,
        "the fixture length is frozen; a changed file is a changed corpus"
    );
    let run = &minute[..spec.bars];
    assert_eq!(
        run.first()
            .expect("the run is nonempty")
            .ts
            .timestamp_millis(),
        spec.first_ms,
        "the head run starts where recorded"
    );
    assert_eq!(
        run.last()
            .expect("the run is nonempty")
            .ts
            .timestamp_millis(),
        spec.last_ms,
        "the head run ends where recorded"
    );
    run.to_vec()
}

/// Compare a minute tape folded to fifteen minutes against the vendor
/// fifteen-minute fixture: a vendor bucket is comparable only when all
/// fifteen minute prints are present, and agreement is exact on prices and
/// volume. Returns `(comparable, matched)`.
fn agreement_against(minute: &[Bar], vendor: &[Bar]) -> (usize, usize) {
    use std::collections::BTreeMap;
    let prints: BTreeMap<i64, &Bar> = minute
        .iter()
        .map(|bar| (bar.ts.timestamp_millis(), bar))
        .collect();
    let mut comparable = 0_usize;
    let mut matched = 0_usize;
    for fifteen in vendor {
        let start = fifteen.ts.timestamp_millis();
        let legs: Vec<&Bar> = (0..15)
            .map(|i| start + i * 60_000)
            .map(|ts| prints.get(&ts).copied())
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        if legs.len() < 15 {
            continue;
        }
        comparable += 1;
        let high = legs.iter().map(|bar| bar.h).max().expect("fifteen legs");
        let low = legs.iter().map(|bar| bar.l).min().expect("fifteen legs");
        if legs[0].o == fifteen.o
            && high == fifteen.h
            && low == fifteen.l
            && legs[14].c == fifteen.c
            && legs.iter().map(|bar| bar.volume).sum::<u64>() == fifteen.volume
        {
            matched += 1;
        }
    }
    (comparable, matched)
}

/// Seed `bars` on an in-memory tape at a three-decimal scale, which covers
/// the fixtures' sub-penny prints: a two-decimal market would refuse those
/// rows and tear the seeded tape before the test begins. Fails loudly when
/// a row is refused, so a fixture edit that outgrows the scale cannot pass
/// quietly.
pub async fn seed_spy_slice(
    symbol: &str,
    bars: &[Bar],
) -> (crate::tape::MarketTape, crate::key::TapeKey) {
    use crate::catalog::{AssetClass, PriceScale};
    use crate::store::{CandleStore as _, WritePolicy};
    use crate::tape::test_setup::{m1_key, memory_tape, with_registry_caps, VENUE};

    with_registry_caps(async {
        let tape = memory_tape();
        let symbol_id = tape
            .register(
                symbol,
                VENUE,
                AssetClass::Stock,
                PriceScale::of(3).expect("valid scale"),
                "UTC",
                Timeframe::M1,
            )
            .expect("registration succeeds");
        let key = m1_key(symbol_id);
        if !bars.is_empty() {
            let outcome = tape
                .append(key, bars, WritePolicy::InsertIfAbsent)
                .expect("seed append succeeds");
            assert_eq!(
                outcome.rejected, 0,
                "the seeded slice stores whole: a refused row is a torn tape"
            );
        }
        (tape, key)
    })
    .await
}
