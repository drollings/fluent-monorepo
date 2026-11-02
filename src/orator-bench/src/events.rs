//! Timestamped bench events in the reference harness shape.
//!
//! Every stage crossing emits `InputPcm` / `Step` / `StepPostSampling`
//! / `SendPcm` with logical timestamps, so a `--stat-file` recorded
//! anywhere replays identically here. The file is JSON lines: one
//! event per line, no header, appendable across runs.

use serde::{Deserialize, Serialize};

/// Event kinds per stage crossing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[cfg(test)]
#[path = "../tests/events.rs"]
mod tests;
