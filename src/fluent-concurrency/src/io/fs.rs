//! Capability-gated filesystem I/O (read, write, metadata).

use std::path::Path;

use fluent_wvr::{Capability, ConcurrencyError};

/// Capability-gated filesystem operations.
pub struct FsCapability;

impl Capability for FsCapability {
    fn name(&self) -> &'static str {
        "fs"
    }
}

impl FsCapability {
    pub async fn read(&self, path: impl AsRef<Path>) -> Result<Vec<u8>, ConcurrencyError> {
        Ok(tokio::fs::read(path).await?)
    }

    pub async fn write(
        &self,
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> Result<(), ConcurrencyError> {
        Ok(tokio::fs::write(path, contents).await?)
    }

    pub async fn metadata(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<std::fs::Metadata, ConcurrencyError> {
        Ok(tokio::fs::metadata(path).await?)
    }
}
