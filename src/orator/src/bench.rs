//! Bench log and render: timestamped stage events plus the local
//! histogram render for `orator stats`.
//!
//! Single home: every stage crossing emits `InputPcm` / `Step` /
//! `StepPostSampling` / `SendPcm` with logical timestamps, so a
//! `--stat-file` recorded anywhere replays identically. The file is
//! JSON lines: one event per line, no header, appendable across runs.
//! Rendering prints counts plus p50/p95/max so a regression shows
//! where time went. The replay harness re-exports these items.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use common_core::metrics::LatencyHistogram;

/// Event kinds per stage crossing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// Event kinds per stage crossing.
pub enum BenchKind {
    /// Audio entered the stage.
    InputPcm,
    /// The bounded compute step ran.
    Step,
    /// Post-sampling work (resample, trim, mark) finished.
    StepPostSampling,
    /// Audio left the stage downstream.
    SendPcm,
}

/// One timestamped event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One timestamped event.
pub struct BenchEvent {
    /// Event kind.
    pub kind: BenchKind,
    /// Stage name.
    pub stage: String,
    /// Logical timestamp in ms.
    pub t_ms: u64,
}

/// An ordered event log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// An ordered event log.
pub struct BenchLog {
    /// Events in emission order.
    pub events: Vec<BenchEvent>,
}

impl BenchLog {
    /// Empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one event.
    pub fn push(&mut self, kind: BenchKind, stage: &str, t_ms: u64) {
        self.events.push(BenchEvent {
            kind,
            stage: stage.to_string(),
            t_ms,
        });
    }

    /// Write the `--stat-file` (JSON lines) atomically: readers see the
    /// old file or the new one, never a half-written mix.
    pub fn write_stat_file(&self, path: &std::path::Path) -> Result<(), String> {
        let mut out = String::new();
        for event in &self.events {
            out.push_str(&serde_json::to_string(event).map_err(|error| error.to_string())?);
            out.push('\n');
        }
        common_core::io::write_atomic(path, out.as_bytes()).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Read a `--stat-file` back.
    pub fn read_stat_file(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
        let mut events = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            events.push(serde_json::from_str(line).map_err(|error| error.to_string())?);
        }
        Ok(Self { events })
    }
}

/// Per-stage histograms plus the end-to-end total.
#[derive(Default)]
/// Per-stage histograms plus the end-to-end total.
pub struct Stats {
    /// One histogram per stage.
    pub stages: HashMap<String, LatencyHistogram>,
    /// Samples per stage.
    pub counts: HashMap<String, u64>,
}

/// Build histograms from `log`. Each stage feeds its
/// `SendPcm − InputPcm` durations into the shared histogram; only
/// closed spans count (open inputs never render).
#[must_use]
/// Build histograms from `log`. Each stage feeds its
/// `SendPcm − InputPcm` durations into the shared histogram; only
/// closed spans count (open inputs never render).
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
/// Render histograms for humans.
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
#[path = "../tests/bench.rs"]
mod tests;