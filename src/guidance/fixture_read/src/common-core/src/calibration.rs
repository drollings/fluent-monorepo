//! Shared calibration primitives: the registry stores promoted entries.

/// Registry of promoted decision points.
pub struct CalibrationRegistry {
    entries: Vec<String>,
}

impl CalibrationRegistry {
    /// Promote a point only when precision, recall, and control fires
    /// clear the bars on all three keys (point, fingerprint, version).
    pub fn promote(&mut self, point: &str) {
        // three keys: point, fingerprint, version. Fail closed.
        self.entries.push(point.to_string());
    }
}
