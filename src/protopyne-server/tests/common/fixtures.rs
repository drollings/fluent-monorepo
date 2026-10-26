//! Shared builders for the server integration suites: one money spelling,
//! one vendored-history load, so the per-file copies cannot drift.
//!
//! Scope is builders, never tapes: hand-tuned bar series stay in the suite
//! that tunes them, and every grant builder already composes the strategy
//! crate's shared fixture (`protopyne_strategy :: fixtures :: grant_with`)
//! with its own floors — there is no second grant shape to unify here.
//! A file migrates by deleting its local copy, importing from here, and
//! spelling integer money as a string literal (exact for integers, so the
//! suite it pins cannot move).

use std::path::PathBuf;

use rust_decimal::Decimal;

use protopyne_types::{Bar, Money};

/// Test money from its decimal spelling. Integer literals spell exactly,
/// so `money("400")` is the same value the old per-file integer helper
/// built — the spelling moves, never the number.
#[must_use]
pub fn money(text: &str) -> Money {
    Money::new(text.parse::<Decimal>().expect("decimal"))
}

/// The vendored 15-minute SPY tape every replay-shaped suite loads.
#[must_use]
pub fn spy_history() -> Vec<Bar> {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("SPY_15M.csv");
    crate::csv::load_bars_csv(&path).expect("vendored history loads")
}
