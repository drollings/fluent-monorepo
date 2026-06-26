//! Capability-gated filesystem I/O (read, write, metadata).

use std::path::Path;

use common_core::error::IoError;
use fluent_wvr::Capability;

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
    pub async fn read(&self, path: impl AsRef<Path>) -> Result<Vec<u8>, IoError> {
        check_capability(self)?;
        Ok(tokio::fs::read(path).await?)
    }

    pub async fn write(
        &self,
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<(), IoError> {
        check_capability(self)?;
        Ok(tokio::fs::write(path, contents).await?)
    }

    pub async fn metadata(&self, path: impl AsRef<Path>) -> Result<std::fs::Metadata, IoError> {
        check_capability(self)?;
        Ok(tokio::fs::metadata(path).await?)
    }
}
