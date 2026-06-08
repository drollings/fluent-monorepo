#[derive(Debug, Clone)]
pub struct OntologyVersion {
    pub version: String,
    pub loaded_at: f64,
    pub source_url: String,
    pub triple_count: u64,
}

pub type MigrateFn = fn() -> Result<(), String>;

pub struct OntologyMigration {
    pub from_version: &'static str,
    pub to_version: &'static str,
    pub migrate_fn: MigrateFn,
}

fn noop_migration() -> Result<(), String> {
    Ok(())
}

pub const MIGRATIONS: &[OntologyMigration] = &[OntologyMigration {
    from_version: "4.5",
    to_version: "4.6",
    migrate_fn: noop_migration,
}];

pub struct VersionRegistry {
    versions: Vec<OntologyVersion>,
}

impl VersionRegistry {
    pub fn new() -> Self {
        Self {
            versions: Vec::new(),
        }
    }

    pub fn record(&mut self, ver: OntologyVersion) {
        self.versions.push(ver);
    }

    pub fn latest(&self) -> Option<&OntologyVersion> {
        self.versions.last()
    }

    pub fn count(&self) -> usize {
        self.versions.len()
    }
}

impl Default for VersionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_recorded_and_retrieved() {
        let mut reg = VersionRegistry::new();
        reg.record(OntologyVersion {
            version: "4.5".to_string(),
            loaded_at: 1_700_000_000.0,
            source_url: "data/yago-4.5.0.2-tiny/yago-tiny.ttl".to_string(),
            triple_count: 1234,
        });

        let latest = reg.latest().unwrap();
        assert_eq!(latest.version, "4.5");
        assert_eq!(latest.triple_count, 1234);
    }

    #[test]
    fn test_migration_stub_is_noop() {
        assert!((MIGRATIONS[0].migrate_fn)().is_ok());
    }

    #[test]
    fn test_latest_returns_none_when_empty() {
        let reg = VersionRegistry::new();
        assert!(reg.latest().is_none());
    }
}
