//! Concrete capability tokens for filesystem, network, and database access.
//! Used with `CapabilitySet` to gate I/O operations.

use fluent_wvr::Capability;
use fluent_wvr::CapabilitySet;

/// Capability token for filesystem read/write/metadata operations.
pub struct FsCapability;

impl Capability for FsCapability {
    fn name(&self) -> &'static str {
        "fs"
    }
}

/// Capability token for network operations (TCP connect, HTTP).
pub struct NetCapability;

impl Capability for NetCapability {
    fn name(&self) -> &'static str {
        "net"
    }
}

/// Capability token for database queries (placeholder).
pub struct DbCapability;

impl Capability for DbCapability {
    fn name(&self) -> &'static str {
        "db"
    }
}

/// Returns a `CapabilitySet` pre-populated with Fs, Net, and Db capabilities.
pub fn default_capability_set() -> CapabilitySet {
    CapabilitySet::new()
        .with(FsCapability)
        .with(NetCapability)
        .with(DbCapability)
}
