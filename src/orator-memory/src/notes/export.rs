//! Meeting file export: deterministic markdown over recorded turns.
//!
//! The renderer is pure: same events in, same bytes out — no clock,
//! no model, no second store. Crash-safety comes from the atomic
//! write at the call site; re-running an export reproduces identical
//! bytes, so a crash mid-render resumes by re-running. Model-backed
//! steps (offline STT over the archive, neural diarization, citable
//! LLM summaries) arrive with the first slow step body, not here.

use std::fmt::Write as _;

use crate::store::StoredEvent;

use crate::notes::facts::roll_summary;

/// Turn-bearing event kinds: spoken or written content. Lifecycle
/// notes ride along as record context; usage counters and unknown
/// kinds never do.
const TURN_KINDS: &[&str] = &["transcript", "reply", "dictation"];

/// Record-worthy kinds: the turn content plus session lifecycle notes.
fn record_worthy(kind: &str) -> bool {
    kind == "note" || TURN_KINDS.contains(&kind)
}

/// Render the session record: header, the session-scoped events in
/// cursor order, and a rolling summary over the turn texts. Empty
/// input still renders the session record — never an empty file.
#[must_use]
pub fn render_meeting_export(session: &str, events: &[StoredEvent]) -> String {
    let turns: Vec<&StoredEvent> = events
        .iter()
        .filter(|event| TURN_KINDS.contains(&event.kind.as_str()))
        .collect();
    let mut out = format!(
        "# meeting {session}\n\nturns: {}\n\n## record\n",
        turns.len()
    );
    let record: Vec<&StoredEvent> = events
        .iter()
        .filter(|event| record_worthy(&event.kind))
        .collect();
    if record.is_empty() {
        out.push_str("*no turns recorded*\n");
    } else {
        for (index, event) in record.iter().enumerate() {
            let _ = writeln!(out, "{}. [{}] {}", index + 1, event.kind, event.payload);
        }
    }
    out.push_str("\n## summary\n");
    let mut summary = String::new();
    for turn in &turns {
        summary = roll_summary(&summary, &turn.payload);
    }
    if summary.is_empty() {
        out.push_str("*no turns recorded*\n");
    } else {
        out.push_str(&summary);
        if !summary.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
#[path = "../../tests/notes_export.rs"]
mod tests;
