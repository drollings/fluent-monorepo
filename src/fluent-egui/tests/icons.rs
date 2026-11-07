use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn counting_cache(cap: usize) -> (IconCache, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&calls);
    let cache = IconCache::new(cap, move |name: &str, px: u32| {
        probe.fetch_add(1, Ordering::SeqCst);
        Ok(format!("{name}:{px}").into_bytes())
    })
    .expect("valid cache");
    (cache, calls)
}

#[test]
fn theme_switch_never_serves_stale_entries() {
    let (cache, calls) = counting_cache(16);
    let first = cache.get("bolt", 16, 0xF1).expect("renders");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "a miss renders once");
    let second = cache.get("bolt", 16, 0xF2).expect("renders");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the F2 key never serves the F1 entry"
    );
    assert_eq!(first, second, "same vector, freshly rendered");
    let again = cache.get("bolt", 16, 0xF1).expect("hits");
    assert_eq!(again, first, "the F1 entry is intact");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "hits never re-render");
    assert_eq!(cache.len(), 2, "both fingerprints cache side by side");
}

#[test]
fn concurrent_keys_stay_isolated() {
    let (cache, _) = counting_cache(256);
    std::thread::scope(|scope| {
        for worker in 0..8u32 {
            let cache = &cache;
            scope.spawn(move || {
                for index in 0..16u32 {
                    let name = format!("icon-{worker}-{index}");
                    let bytes = cache.get(&name, 16, 0xF1).expect("renders");
                    assert_eq!(
                        bytes,
                        format!("{name}:16").into_bytes(),
                        "every key reads its own bytes"
                    );
                }
            });
        }
    });
    assert_eq!(cache.len(), 128, "every key cached exactly once");
}

#[test]
fn inserts_past_cap_evict_oldest_first() {
    let (cache, calls) = counting_cache(8);
    for index in 0..32u32 {
        cache
            .get(&format!("icon-{index}"), 16, 0xF1)
            .expect("renders");
    }
    assert_eq!(cache.len(), 8, "inserts past cap stay bounded");
    assert!(
        cache.get("icon-0", 16, 0xF1).is_ok(),
        "evicted keys re-render on demand"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        33,
        "exactly one re-render for the evicted key"
    );
    assert_eq!(cache.len(), 8, "the re-render keeps the bound");
}

#[test]
fn zero_cap_and_render_failures_refuse() {
    assert!(
        IconCache::new(0, |_: &str, _: u32| Ok(Vec::new())).is_err(),
        "an unbounded cache is a leak, not a default"
    );
    let cache = IconCache::new(4, |name: &str, _: u32| {
        if name == "bad" {
            return Err(EguiError::InvalidParam {
                what: name.to_owned(),
                reason: "no such icon".to_owned(),
            });
        }
        Ok(name.as_bytes().to_vec())
    })
    .expect("valid cache");
    assert!(cache.get("bad", 16, 0xF1).is_err(), "failures surface");
    assert!(cache.is_empty(), "failures never populate the cache");
}
