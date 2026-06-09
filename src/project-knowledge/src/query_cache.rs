use project_common::hash::fnv1a64;
use rusqlite::{params, Connection};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct Entry {
    pub query: String,
    pub result_summary: String,
    pub timestamp: u64,
    pub ttl_seconds: u64,
}

pub struct QueryCache {
    db: Connection,
    default_ttl_seconds: u64,
    #[allow(dead_code)]
    max_entries: usize,
}

impl QueryCache {
    pub fn new(db_path: &Path, default_ttl_seconds: u64) -> rusqlite::Result<Self> {
        let db = Connection::open(db_path)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS query_cache (
                key TEXT PRIMARY KEY,
                result_json TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                ttl_seconds INTEGER NOT NULL
            )",
        )?;
        Ok(Self {
            db,
            default_ttl_seconds,
            max_entries: 4096,
        })
    }

    pub fn new_in_memory(default_ttl_seconds: u64) -> rusqlite::Result<Self> {
        let db = Connection::open_in_memory()?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS query_cache (
                key TEXT PRIMARY KEY,
                result_json TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                ttl_seconds INTEGER NOT NULL
            )",
        )?;
        Ok(Self {
            db,
            default_ttl_seconds,
            max_entries: 4096,
        })
    }

    fn query_key(query: &str) -> String {
        let hash = fnv1a64(query.to_lowercase().as_bytes());
        format!("{hash:016x}")
    }

    pub fn get(&self, query: &str) -> rusqlite::Result<Option<Entry>> {
        let key = Self::query_key(query);
        let mut stmt = self.db.prepare(
            "SELECT key, result_json, timestamp, ttl_seconds FROM query_cache WHERE key = ?1",
        )?;
        let result = stmt.query_row(params![key], |row| {
            Ok(Entry {
                query: row.get(0)?,
                result_summary: row.get(1)?,
                timestamp: row.get(2)?,
                ttl_seconds: row.get(3)?,
            })
        });
        match result {
            Ok(entry) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if now > entry.timestamp + entry.ttl_seconds {
                    return Ok(None);
                }
                Ok(Some(entry))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn put(&self, query: &str, result: &str) -> rusqlite::Result<()> {
        let key = Self::query_key(query);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.db.execute(
            "INSERT OR REPLACE INTO query_cache (key, result_json, timestamp, ttl_seconds) VALUES (?1, ?2, ?3, ?4)",
            params![key, result, now, self.default_ttl_seconds],
        )?;
        Ok(())
    }

    pub fn clear(&self) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM query_cache", [])?;
        Ok(())
    }

    pub fn stats(&self) -> rusqlite::Result<(usize, u64)> {
        let count: usize = self
            .db
            .query_row("SELECT COUNT(*) FROM query_cache", [], |row| row.get(0))?;
        Ok((count, self.default_ttl_seconds))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (QueryCache, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("cache.db");
        let cache = QueryCache::new(&db_path, 3600).unwrap();
        (cache, dir)
    }

    #[test]
    fn put_and_get() {
        let (cache, _dir) = setup();
        cache.put("test query", "test result").unwrap();
        let entry = cache.get("test query").unwrap().unwrap();
        assert_eq!(entry.result_summary, "test result");
    }

    #[test]
    fn stats_work() {
        let (cache, _dir) = setup();
        let (count, ttl) = cache.stats().unwrap();
        assert_eq!(count, 0);
        assert_eq!(ttl, 3600);
        cache.put("q", "r").unwrap();
        let (count, _) = cache.stats().unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn clear_works() {
        let (cache, _dir) = setup();
        cache.put("q", "r").unwrap();
        cache.clear().unwrap();
        assert!(cache.get("q").unwrap().is_none());
    }
}
