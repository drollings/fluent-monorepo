//! Compatibility re-export: histograms and render live in
//! `orator-core::bench` (single home); the stats CLI composes them
//! from here.

pub use orator_core::bench::{histograms, render_stats, Stats};

#[cfg(test)]
#[path = "../tests/stats.rs"]
mod tests;
