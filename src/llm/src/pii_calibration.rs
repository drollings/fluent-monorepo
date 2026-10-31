//! PII calibration harness — control group plus must-fire group.
//!
//! No production firing change. This module documents precision/recall of the
//! current 5-row detector table: calibration measures confidence-axis firing
//! (does the pattern match?), never redaction task-value.

#[cfg(test)]
#[path = "../tests/pii_calibration.rs"]
mod tests;
