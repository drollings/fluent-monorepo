//! Placeholder database capability (returns unsupported errors).

use fluent_wvr::{Capability, ConcurrencyError};

/// Placeholder database capability — no backend is currently wired.
pub struct DbCapability;

impl Capability for DbCapability {
    fn name(&self) -> &'static str {
        "db"
    }
}

impl DbCapability {
    pub fn query(&self, _sql: &str) -> Result<Vec<std::collections::HashMap<String, String>>, ConcurrencyError> {
        Err(ConcurrencyError::Io(std::io::Error::other(
            "DbCapability is a placeholder - no database configured",
        )))
    }

    pub fn execute(&self, _sql: &str) -> Result<usize, ConcurrencyError> {
        Err(ConcurrencyError::Io(std::io::Error::other(
            "DbCapability is a placeholder - no database configured",
        )))
    }
}
