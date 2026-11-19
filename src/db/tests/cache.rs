use super::*;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn put_and_get() {
    let dir = tempdir();
    let cache = SqliteTtlCache::open(&dir.path().join("cache.db"), std::time::Duration::from_secs(3600)).unwrap();
    cache.put("test query", "test result").unwrap();
    let entry = cache.get("test query").unwrap().unwrap();
    assert_eq!(entry.result_summary, "test result");
}

#[test]
fn stats_work() {
    let dir = tempdir();
    let cache = SqliteTtlCache::open(&dir.path().join("cache.db"), std::time::Duration::from_secs(3600)).unwrap();
    let (count, ttl, _expired) = cache.stats().unwrap();
    assert_eq!(count, 0);
    assert_eq!(ttl, std::time::Duration::from_secs(3600));
    cache.put("q", "r").unwrap();
    let (count, _, _) = cache.stats().unwrap();
    assert_eq!(count, 1);
}

#[test]
fn clear_works() {
    let dir = tempdir();
    let cache = SqliteTtlCache::open(&dir.path().join("cache.db"), std::time::Duration::from_secs(3600)).unwrap();
    cache.put("q", "r").unwrap();
    cache.clear().unwrap();
    assert!(cache.get("q").unwrap().is_none());
}

#[test]
fn oldest_eviction_works() {
    let dir = tempdir();
    let cache = SqliteTtlCache::open(&dir.path().join("cache.db"), std::time::Duration::from_secs(3600))
        .unwrap()
        .with_cap(3);
    cache.put("q1", "r1").unwrap();
    cache.put("q2", "r2").unwrap();
    cache.put("q3", "r3").unwrap();
    cache.put("q4", "r4").unwrap(); // should evict q1
    let (count, _, _) = cache.stats().unwrap();
    assert!(
        count <= 3,
        "expected <= 3 entries after oldest-first eviction, got {count}"
    );
}

#[test]
fn in_memory_cap_is_configurable() {
    fn identity(key: &str) -> String {
        key.to_string()
    }
    let cache = SqliteTtlCache::open_in_memory(std::time::Duration::from_secs(3600))
        .unwrap()
        .with_key(identity)
        .with_cap(2);
    cache.put("q1", "r1").unwrap();
    cache.put("q2", "r2").unwrap();
    cache.put("q3", "r3").unwrap();
    let (count, _, _) = cache.stats().unwrap();
    assert_eq!(count, 2, "entries over the configured cap must evict");
}

#[test]
fn expired_entry_returns_none() {
    let dir = tempdir();
    // TTL of 0 seconds means immediately expired.
    let cache = SqliteTtlCache::open(&dir.path().join("cache.db"), std::time::Duration::ZERO).unwrap();
    cache.put("expired", "data").unwrap();
    let entry = cache.get("expired").unwrap();
    assert!(entry.is_none(), "expired entry should return None");
}

#[test]
fn evict_expired_works() {
    let dir = tempdir();
    let cache = SqliteTtlCache::open(&dir.path().join("cache.db"), std::time::Duration::ZERO).unwrap();
    cache.put("a", "1").unwrap();
    cache.put("b", "2").unwrap();
    let evicted = cache.evict_expired().unwrap();
    assert!(evicted >= 2, "should evict expired entries");
    let (count, _, _) = cache.stats().unwrap();
    assert_eq!(count, 0);
}

/// Reads never refresh recency: eviction is write-order FIFO, not LRU.
/// `q1` is read repeatedly after `q2` lands, yet the over-cap `put`
/// still evicts `q1` first.
#[test]
fn reads_do_not_refresh_fifo_order() {
    let cache = SqliteTtlCache::open_in_memory(std::time::Duration::from_secs(3600)).unwrap().with_cap(2);
    cache.put("q1", "r1").unwrap();
    cache.put("q2", "r2").unwrap();
    // Pin distinct write timestamps (same-second puts would tie).
    let now = common_core::time::now_secs();
    cache
        .store
        .execute(
            "UPDATE query_cache SET timestamp = ?1 WHERE key = 'q1'",
            rusqlite::params![now - 10],
        )
        .unwrap();
    assert!(cache.get("q1").unwrap().is_some());
    assert!(cache.get("q1").unwrap().is_some());
    cache.put("q3", "r3").unwrap();
    assert!(
        cache.get("q1").unwrap().is_none(),
        "reads must not refresh FIFO recency"
    );
    assert!(cache.get("q2").unwrap().is_some());
    assert!(cache.get("q3").unwrap().is_some());
}

#[test]
fn open_in_memory_round_trips() {
    let cache = SqliteTtlCache::open_in_memory(std::time::Duration::from_secs(3600)).unwrap();
    cache.put("mem", "value").unwrap();
    let entry = cache.get("mem").unwrap().unwrap();
    assert_eq!(entry.result_summary, "value");
}

#[test]
fn hash_key_is_configurable() {
    let dir = tempdir();
    fn upper_key(key: &str) -> String {
        key.to_uppercase()
    }
    let cache = SqliteTtlCache::open(&dir.path().join("cache.db"), std::time::Duration::from_secs(3600))
        .unwrap()
        .with_key(upper_key);
    cache.put("query", "r").unwrap();
    // Key derivation is applied symmetrically: any input that derives to
    // the same key resolves the entry, and the stored key is the derived
    // form, not the raw query.
    let entry = cache.get("QUERY").unwrap().unwrap();
    assert_eq!(entry.key, "QUERY");
    assert_eq!(entry.result_summary, "r");
    let entry2 = cache.get("query").unwrap().unwrap();
    assert_eq!(entry2.result_summary, "r");
}
