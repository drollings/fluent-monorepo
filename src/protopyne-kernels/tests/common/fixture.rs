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
    ClosureEntry, LibraryId, LinkedModule, ModuleId, ModuleInput, REAL_LIBRARIES,
    TASK_INDICATORS, closure_data_root, load_listed_closure, load_pine_source,
    parse_pine_source, parse_program, resolve_imports,
};
use protopyne_types::Bar;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// The single data root every Pine fixture reads from.
#[must_use]
pub fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs")
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

/// One task closure linked dependency-first: the same loader, parser, and
/// linker the closure suites use, mapped to alias tables for runs. `name`
/// is the [`TASK_INDICATORS`] entry name (`DeltaVolumeProfileProtopyne`,
/// `ApexPPAS`, `ApexRSIMD`).
#[must_use]
pub fn linked_closure(name: &str) -> Vec<LinkedModule> {
    let indicator: &ClosureEntry = TASK_INDICATORS
        .iter()
        .find(|entry| entry.name == name)
        .expect("task indicator entry");
    let root = closure_data_root(env!("CARGO_MANIFEST_DIR"));
    let sources = load_listed_closure(&root, REAL_LIBRARIES, indicator).expect("closure loads");
    let modules: Vec<crate::ModuleInput> = sources
        .into_iter()
        .map(|(key, source)| {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("closure file {key} parses: {err}"));
            let (publisher, name) = key.split_once('/').expect("key shape");
            let version = if publisher == "local" {
                indicator.version
            } else {
                REAL_LIBRARIES
                    .iter()
                    .find(|entry| entry.publisher == publisher && entry.name == name)
                    .expect("listed library")
                    .version
            };
            crate::ModuleInput::new(library_id(publisher, name, version), source, program)
        })
        .collect();
    linked_modules(modules)
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
            let secs = cells.next().expect("time").parse::<i64>().expect("int time");
            Bar::new()
                .ts(
                    chrono::TimeZone::timestamp_opt(&chrono::Utc, secs, 0)
                        .single()
                        .expect("bar time in range"),
                )
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
                .ts(
                    chrono::DateTime::from_timestamp_millis(key * DAY_MS)
                        .expect("bucket start in range"),
                )
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
