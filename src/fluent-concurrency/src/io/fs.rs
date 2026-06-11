//! Capability-gated filesystem I/O (read, write, metadata).

use std::path::Path;

use fluent_wvr::{Capability, ConcurrencyError};

use crate::io::check_capability;

/// Capability-gated filesystem operations.
/// Cannot be constructed directly outside this crate; use `FsCapability::new()`.
pub struct FsCapability {
    _priv: (),
}

impl FsCapability {
    pub fn new() -> Self {
        Self { _priv: () }
    }
}

impl Default for FsCapability {
    fn default() -> Self {
        Self::new()
    }
}

impl Capability for FsCapability {
    fn name(&self) -> &'static str {
        "fs"
    }
}

impl FsCapability {
    pub async fn read(&self, path: impl AsRef<Path>) -> Result<Vec<u8>, ConcurrencyError> {
        check_capability(self)?;
        Ok(tokio::fs::read(path).await?)
    }

    pub async fn write(
        &self,
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<(), ConcurrencyError> {
        check_capability(self)?;
        Ok(tokio::fs::write(path, contents).await?)
    }

    pub async fn metadata(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<std::fs::Metadata, ConcurrencyError> {
        check_capability(self)?;
        Ok(tokio::fs::metadata(path).await?)
    }
}
