//! Shared Pine link-fixture constructors: one home for the
//! `shipped_module` / `driver_module` / `linked` helpers previously copied
//! across the `pine_lib*` suites. Every constructor is pure over values past
//! the file read: identities from the header scan, structure from the parser,
//! over the same byte-identical text.
//!
//! Single home included once from `pine` (`pub(crate) mod test_fixture`);
//! consumer suites `use` it, never include it again.
//!
//! Each consumer uses a subset of these constructors; dead-code is allowed
//! here so the shared home stays single without per-consumer shims.

#![allow(dead_code)]

use crate::{
    closure_inputs, discover_closures, load_pine_source, parse_pine_source, parse_program,
    resolve_imports, ClosureOutcome, LibraryId, LinkedModule, LtfDataset, ManifestFile, ModuleId,
    ModuleInput,
};
use protopyne_types::Bar;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// The single data root every live-directory probe reads from. Volatile
/// input (externally synced, git-ignored): probes over this root must
/// enumerate, never assume membership.
#[must_use]
pub fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
}

/// The checked-in hermetic fixture root: the canonical
/// `protopyne-server/tests/data/pine_closure` tree (`indicators/` +
/// `libraries/` with the same layout as the live data root), so the same
/// loader and discovery code serve both. Fixture pins exact file bytes;
/// the live root pins only properties.
#[must_use]
pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../protopyne-server/tests/data/pine_closure")
}

/// Raw fixture files as manifest rows.
#[must_use]
pub fn fixture_sources() -> (Vec<ManifestFile>, Vec<ManifestFile>) {
    crate::manifest_files::load_pine_sources(&fixture_root()).expect("fixture root loads")
}

/// Discovery outcomes over the hermetic fixtures.
#[must_use]
pub fn fixture_outcomes() -> Vec<ClosureOutcome> {
    let (indicators, libraries) = fixture_sources();
    discover_closures(&indicators, &libraries)
}

/// Discovery outcomes over the live data root (volatile: enumerate only).
#[must_use]
pub fn discovered_outcomes() -> Vec<ClosureOutcome> {
    let (indicators, libraries) =
        crate::manifest_files::load_pine_sources(&data_root()).expect("live root loads");
    discover_closures(&indicators, &libraries)
}

/// Claimed library identity: publisher/name with the pinned version retained
/// as metadata, never enforced on load.
#[must_use]
pub fn library_id(publisher: &str, name: &str, version: u64) -> LibraryId {
    LibraryId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
        version,
    }
}

/// Graph identity: publisher and name only.
#[must_use]
pub fn module_key(publisher: &str, name: &str) -> ModuleId {
    ModuleId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
    }
}

/// One shipped Pine file as a graph input: identity from the header scan,
/// structure from the parser, over the same byte-identical text.
#[must_use]
pub fn shipped_module(file: &str, publisher: &str, name: &str, version: u64) -> ModuleInput {
    let path = data_root().join(file);
    let source = load_pine_source(&path).expect("shipped library loads");
    let program = parse_program(&source.text).expect("shipped library parses");
    ModuleInput::new(library_id(publisher, name, version), source, program)
}

/// One fixture Pine file as a graph input: identity from the header scan,
/// structure from the parser, over the checked-in byte-identical text.
#[must_use]
pub fn fixture_module(file: &str, publisher: &str, name: &str, version: u64) -> ModuleInput {
    let path = fixture_root().join(file);
    let source = load_pine_source(&path).expect("fixture file loads");
    let program = parse_program(&source.text).expect("fixture file parses");
    ModuleInput::new(library_id(publisher, name, version), source, program)
}

/// One synthetic driver script as a graph input.
#[must_use]
pub fn driver_module(text: &str) -> ModuleInput {
    let source = parse_pine_source("local/Driver", text);
    let program = parse_program(text).expect("driver parses");
    ModuleInput::new(library_id("local", "Driver", 1), source, program)
}

/// One synthetic library as a graph input: parsed under its
/// `publisher/name` key so a driver can import it by that path.
#[must_use]
pub fn synthetic_library(publisher: &str, name: &str, text: &str) -> ModuleInput {
    let source = parse_pine_source(&format!("{publisher}/{name}"), text);
    let program = parse_program(text).expect("synthetic library parses");
    ModuleInput::new(library_id(publisher, name, 1), source, program)
}

/// Link `modules` in dependency-first order, mapping each ordered entry to
/// its alias table for linked execution.
#[must_use]
pub fn linked_modules(modules: Vec<ModuleInput>) -> Vec<LinkedModule> {
    let graph = resolve_imports(modules).expect("modules resolve");
    graph
        .order()
        .iter()
        .map(|id| {
            let entry = graph.get(id).expect("ordered module resolves");
            let aliases = graph
                .alias_table(id)
                .unwrap_or_default()
                .into_iter()
                .map(|(alias, target)| (alias, target.to_string()))
                .collect::<HashMap<String, String>>();
            LinkedModule {
                key: id.to_string(),
                program: entry.program.clone(),
                aliases,
            }
        })
        .collect()
}

/// One fixture closure linked dependency-first: the same loader, parser,
/// and linker the closure suites use, mapped to alias tables for runs.
/// `stem` is the fixture indicator stem (`_IntegrationLite`,
/// `_DeltaVolumeProfileProtopyne`, `_FastMAs`).
#[must_use]
pub fn linked_fixture(stem: &str) -> Vec<LinkedModule> {
    let outcome = fixture_outcomes()
        .into_iter()
        .find(|outcome| outcome.stem() == stem)
        .unwrap_or_else(|| panic!("fixture closure {stem} discovered"));
    let closure = match outcome {
        ClosureOutcome::Linked(closure) => closure,
        ClosureOutcome::Refused { reason, .. } => {
            panic!("fixture closure {stem} links: {reason}")
        }
    };
    let inputs = closure_inputs(&closure)
        .unwrap_or_else(|err| panic!("fixture closure {stem} parses: {err}"));
    linked_modules(inputs)
}

/// Every hermetic fixture closure linked: one entry per linkable fixture
/// indicator. The fixture set is fixed, so the count is pinned here.
#[must_use]
pub fn linked_fixtures() -> Vec<(String, Vec<LinkedModule>)> {
    fixture_outcomes()
        .into_iter()
        .map(|outcome| match outcome {
            ClosureOutcome::Linked(closure) => {
                let inputs = closure_inputs(&closure).expect("fixture closure parses");
                (closure.stem.clone(), linked_modules(inputs))
            }
            ClosureOutcome::Refused { stem, reason, .. } => {
                panic!("fixture closure {stem} links: {reason}")
            }
        })
        .collect()
}

/// One trimmed history file as closed bars. The header is pinned: the
/// harness reads the same CSV bytes as the backtest loader through its own
/// tiny reader because it needs engine bars, not venue history.
#[must_use]
pub fn history_bars(file: &str) -> Vec<Bar> {
    let path = data_root().join(file);
    let content = std::fs::read_to_string(&path).expect("history loads");
    let mut lines = content.lines();
    assert_eq!(
        lines.next().expect("header present"),
        "time,open,high,low,close,Volume",
        "history header pinned"
    );
    lines
        .map(|line| {
            let mut cells = line.split(',');
            let secs = cells
                .next()
                .expect("time")
                .parse::<i64>()
                .expect("int time");
            Bar::new()
                .ts(chrono::TimeZone::timestamp_opt(&chrono::Utc, secs, 0)
                    .single()
                    .expect("bar time in range"))
                .o(cells.next().expect("o").parse().expect("decimal open"))
                .h(cells.next().expect("h").parse().expect("decimal high"))
                .l(cells.next().expect("l").parse().expect("decimal low"))
                .c(cells.next().expect("c").parse().expect("decimal close"))
                .volume(cells.next().expect("v").parse().expect("int volume"))
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// Floor-aligned daily buckets over bars (first open, high/low extremes,
/// last close, summed volume), mirroring the server proof's resampler
/// shape for the timeframe with no CSV of its own.
#[must_use]
pub fn daily_of(bars: &[Bar]) -> Vec<Bar> {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    let mut buckets: BTreeMap<i64, Vec<&Bar>> = BTreeMap::new();
    for bar in bars {
        buckets
            .entry(bar.ts.timestamp_millis().div_euclid(DAY_MS))
            .or_default()
            .push(bar);
    }
    buckets
        .into_iter()
        .map(|(key, mut rows)| {
            rows.sort_by_key(|bar| bar.ts);
            let first = rows.first().expect("nonempty bucket");
            let last = rows.last().expect("nonempty bucket");
            Bar::new()
                .ts(chrono::DateTime::from_timestamp_millis(key * DAY_MS)
                    .expect("bucket start in range"))
                .o(first.o)
                .h(rows.iter().map(|bar| bar.h).max().expect("nonempty bucket"))
                .l(rows.iter().map(|bar| bar.l).min().expect("nonempty bucket"))
                .c(last.c)
                .volume(rows.iter().map(|bar| bar.volume).sum())
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// One synthetic daily tape for injected request tests: closes ramp one
/// point per row, volumes one unit per row, every row a day after the last.
/// Shared by the differential and calibration suites so one tape feeds both.
#[must_use]
pub fn daily_tape(rows: usize) -> LtfDataset {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    let first = 1_690_000_000_i64;
    let closes: Vec<f64> = (0..rows).map(|i| 100.0 + i as f64).collect();
    let daily: Vec<Bar> = closes
        .iter()
        .enumerate()
        .map(|(i, close)| {
            let millis = first + i as i64 * DAY_MS;
            Bar::new()
                .ts(chrono::DateTime::from_timestamp(
                    millis / 1000,
                    ((millis % 1000) * 1_000_000) as u32,
                )
                .expect("daily time in range"))
                .o(rust_decimal::Decimal::from_f64_retain(*close).expect("finite"))
                .h(rust_decimal::Decimal::from_f64_retain(close + 1.0).expect("finite"))
                .l(rust_decimal::Decimal::from_f64_retain(close - 1.0).expect("finite"))
                .c(rust_decimal::Decimal::from_f64_retain(*close).expect("finite"))
                .volume((i + 1) as u64)
                .maybe_adj(None)
                .build()
        })
        .collect();
    LtfDataset::of_bars(&daily, protopyne_types::Timeframe::D1)
}
