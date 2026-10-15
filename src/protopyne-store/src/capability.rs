//! One capability token per store file: who may touch this tape.
//!
//! [`StoreStorage`] mirrors `venue :: journal :: VenueStorage`. The session
//! mints exactly one per store file and installs it in the batch/scope
//! capability set; every store effect checks it before touching the
//! database, so a task that was never granted the store is denied, not
//! defaulted.
//!
//! Two layers share the work, because the two store surfaces take different
//! shapes. The synchronous [`CandleStore`](crate::store::CandleStore)
//! interface carries no permit parameter, so its implementation holds the
//! permit it was opened with and checks its *presence* in the task-local
//! set: that proves the task was granted store access at all. Methods that
//! do take a permit ([`StoreHandle`](crate::handle::StoreHandle)'s async
//! surface, shaped like the journal's) additionally verify the permit is
//! *bound to this file*, which closes the multi-file case. Either layer
//! refuses with [`StoreError::Denied`](crate::error::StoreError::Denied).

use std::path::{Path, PathBuf};

use fluent_wvr::capability::check_capability;

use crate::error::StoreError;

/// Capability token binding store access to one supervised session's file.
///
/// The session mints exactly one per store file and installs it in the
/// batch/scope capability set; handlers carry it the way journal callers
/// carry `VenueStorage`. For an ephemeral in-memory store the binding is
/// the literal path `":memory:"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreStorage {
    store_path: PathBuf,
}

impl StoreStorage {
    /// Mint the token for `store_path`. Only the store open path calls
    /// this; everyone else borrows the installed token.
    #[must_use]
    pub fn new(store_path: PathBuf) -> Self {
        Self { store_path }
    }

    /// The store file this token is bound to.
    #[must_use]
    pub fn store_path(&self) -> &Path {
        &self.store_path
    }
}

impl fluent_wvr::Capability for StoreStorage {
    fn name(&self) -> &'static str {
        "store-storage"
    }
}

/// Verify `permit` is present in the calling task's capability set.
///
/// This is the presence half of the gate: it proves the task was granted
/// store access. Callers that receive a permit additionally check the
/// binding half with [`check_binding`].
pub(crate) fn check_present(permit: &StoreStorage) -> Result<(), StoreError> {
    check_capability(permit).map_err(|_| {
        StoreError::Denied("no store capability in the calling task's capability set".to_owned())
    })
}

/// Verify `permit` is bound to `store_path`.
///
/// This is the binding half of the gate: it proves the granted access is
/// for *this* file, so a permit minted for one store cannot operate
/// another. Runs before any presence check — a foreign permit is refused
/// on its face, without consulting the task-local set.
pub(crate) fn check_binding(permit: &StoreStorage, store_path: &Path) -> Result<(), StoreError> {
    if permit.store_path != store_path {
        return Err(StoreError::Denied(format!(
            "store permit is bound to {}, not {}",
            permit.store_path.display(),
            store_path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/capability_gate.rs"]
mod tests;
