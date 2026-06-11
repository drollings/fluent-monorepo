//! Capability-gated I/O primitive engines (fs, net, db).

use std::io;

use fluent_wvr::{Capability, ConcurrencyError};

use crate::scope::CURRENT_CAPS;

/// Validates that the current task-local `CapabilitySet` contains the requested capability.
/// Returns a `PermissionDenied` error if the capability is absent.
pub(crate) fn check_capability<C: Capability>(cap: &C) -> Result<(), ConcurrencyError> {
    let present = CURRENT_CAPS
        .try_with(|caps| caps.get::<C>().is_some())
        .unwrap_or(false);
    if present {
        Ok(())
    } else {
        Err(ConcurrencyError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("missing capability: {}", cap.name()),
        )))
    }
}

pub mod db;
pub mod fs;
pub mod net;
