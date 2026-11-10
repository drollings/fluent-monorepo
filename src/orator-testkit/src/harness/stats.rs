//! Compatibility re-export: histograms and render live in
//! `orator::bench` (single home); the stats CLI composes them
//! from here.

pub use orator::bench::{histograms, render_stats, Stats};

#[cfg(test)]
#[path = "../../tests/harness_stats.rs"]
mod tests;
