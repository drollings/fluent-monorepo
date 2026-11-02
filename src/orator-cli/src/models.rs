//! Honest `models verify` reporting: every requested entry gets a
//! line, and nothing verifies silently.
//!
//! The cryptographic pass still lives in `orator_models`; this
//! module only classifies rows into report lines, so the policy — a
//! named entry that cannot be verified fails, the all-models run
//! fails only on a present-and-pinned mismatch or error — stays
//! unit-pinned without touching disk.

use orator_models::ModelStatus;

/// `models verify` outcome: report lines plus how the command exits.
pub struct VerifyReport {
    /// One line per examined entry, in catalog order.
    pub lines: Vec<String>,
    /// Whether the command fails.
    pub failed: bool,
    /// Whether the requested name matches no catalog entry.
    pub unknown: bool,
}

/// Classify every catalog `row` for `models verify`. `name` selects
/// one model (empty verifies all); `check` runs the cryptographic
/// pass for a pinned-and-present row and names its failure.
#[must_use]
pub fn verify_report(
    rows: &[ModelStatus],
    name: &str,
    mut check: impl FnMut(&str) -> Result<bool, String>,
) -> VerifyReport {
    if !name.is_empty() && !rows.iter().any(|row| row.name == name) {
        return VerifyReport {
            lines: Vec::new(),
            failed: true,
            unknown: true,
        };
    }
    let mut report = VerifyReport {
        lines: Vec::new(),
        failed: false,
        unknown: false,
    };
    for row in rows.iter().filter(|row| name.is_empty() || row.name == name) {
        let named = !name.is_empty();
        let (line, failed) = if !row.present {
            (format!("{}: missing", row.name), named)
        } else if !row.pinned {
            (
                format!("{}: unverified (unpinned)", row.name),
                named,
            )
        } else {
            match check(&row.name) {
                Ok(true) => (format!("{}: ok", row.name), false),
                Ok(false) => (format!("{}: MISMATCH", row.name), true),
                Err(detail) => (format!("{}: {detail}", row.name), true),
            }
        };
        report.failed |= failed;
        report.lines.push(line);
    }
    report
}

#[cfg(test)]
#[path = "../tests/models.rs"]
mod tests;
