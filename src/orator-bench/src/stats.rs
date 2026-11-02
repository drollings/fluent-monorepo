//! `orator stats`: latency histograms from a stat file.
//!
//! Each stage feeds its `SendPcm − InputPcm` durations into a shared
//! histogram; rendering prints counts plus p50/p95/max so a
//! regression shows where time went.

use std::collections::HashMap;

use common_core::metrics::LatencyHistogram;

use crate::events::{BenchKind, BenchLog};

/// Per-stage histograms plus the end-to-end total.
#[derive(Default)]
pub struct Stats {
    /// One histogram per stage.
    pub stages: HashMap<String, LatencyHistogram>,
    /// Samples per stage.
    pub counts: HashMap<String, u64>,
}

/// Build histograms from `log`.
#[must_use]
pub fn histograms(log: &BenchLog) -> Stats {
    let mut stats = Stats::default();
    let mut open: HashMap<String, u64> = HashMap::new();
    for event in &log.events {
        match event.kind {
            BenchKind::InputPcm => {
                open.insert(event.stage.clone(), event.t_ms);
            }
            BenchKind::SendPcm => {
                if let Some(start) = open.remove(&event.stage) {
                    let spent = event.t_ms.saturating_sub(start);
                    let histogram = stats.stages.entry(event.stage.clone()).or_default();
                    histogram.observe(spent);
                    *stats.counts.entry(event.stage.clone()).or_insert(0) += 1;
                }
            }
            BenchKind::Step | BenchKind::StepPostSampling => {}
        }
    }
    stats
}

/// Render histograms for humans.
#[must_use]
pub fn render_stats(stats: &Stats) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("orator stats\n");
    let mut stages: Vec<&String> = stats.stages.keys().collect();
    stages.sort();
    for stage in stages {
        let histogram = &stats.stages[stage];
        let count = stats.counts.get(stage).copied().unwrap_or(0);
        let _ = writeln!(
            out,
            "  {stage}: n={count} p50={}ms p95={}ms max={}ms",
            histogram.estimate_percentile(50.0),
            histogram.estimate_percentile(95.0),
            histogram.estimate_percentile(100.0),
        );
    }
    out
}

#[cfg(test)]
#[path = "../tests/stats.rs"]
mod tests;
