//! Capability-gated I/O primitive engines (fs, net, db).

use fluent_wvr::Capability;

use crate::scope::CURRENT_CAPS;

/// Why a capability request was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    /// The capability was not present in the current task-local `CapabilitySet`.
    Missing { name: &'static str },
    /// The capability is present, but the underlying resource is exhausted.
    Exhausted { name: &'static str, detail: String },
}

impl std::fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { name } => write!(f, "missing capability: {name}"),
            Self::Exhausted { name, detail } => {
                write!(f, "capability exhausted: {name} — {detail}")
            }
        }
    }
}

impl std::error::Error for CapabilityError {}

impl From<CapabilityError> for std::io::Error {
    fn from(err: CapabilityError) -> Self {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, err)
    }
}

/// Validates that the current task-local `CapabilitySet` contains the requested capability.
/// Returns `Err(PermissionDenied)` if the capability is absent.
pub(crate) fn check_capability<C: Capability>(cap: &C) -> Result<(), std::io::Error> {
    let present = CURRENT_CAPS
        .try_with(|caps| caps.get::<C>().is_some())
        .unwrap_or(false);
    if present {
        Ok(())
    } else {
        Err(CapabilityError::Missing { name: cap.name() }.into())
    }
}

pub mod db;
pub mod fs;
pub mod net;
