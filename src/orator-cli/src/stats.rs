//! Local stat-file rendering: the `stats <file>` branch as a pure,
//! unit-pinned function. Bytes on success equal the daemon path
//! (`BenchLog` histograms + render); failures carry the envelope line
//! (action, path, remedy) instead of a bare OS error.

use std::path::Path;

use orator::bench::{render_stats, BenchEvent, BenchLog};

/// Render the stat file at `path` to its report text. Missing or
/// unreadable files fail with the envelope line naming the path and
/// the remedy; corrupt lines fail naming the path and line number.
pub fn render_stat_file(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| crate::error::file_error("read stat file", path, &error))?;
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: BenchEvent = serde_json::from_str(line).map_err(|error| {
            crate::error::parse_error("parse stat file", path, index + 1, error)
        })?;
        events.push(event);
    }
    let log = BenchLog { events };
    Ok(render_stats(&orator::bench::histograms(&log)))
}

#[cfg(test)]
#[path = "../tests/stats.rs"]
mod tests;
