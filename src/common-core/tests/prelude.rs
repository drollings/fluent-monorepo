//! Prelude completeness: every symbol the prelude promises for the 80% case
//! resolves through `common_core::prelude::*` and behaves identically to the
//! direct path.

use common_core::prelude::*;
use std::sync::{Mutex, RwLock};

#[test]
fn prelude_exposes_backoff_schedule_math() {
    assert_eq!(backoff_ms(100, 1, 0), 100);
    assert_eq!(backoff_ms(100, 3, 0), 400);
    assert_eq!(
        capped_backoff_ms(5_000, 0, 12),
        common_core::retry::capped_backoff_ms(5_000, 0, 12)
    );
    assert_eq!(
        capped_exp_backoff_ms(1_000, 3, 6),
        common_core::retry::capped_exp_backoff_ms(1_000, 3, 6)
    );
}

#[test]
fn prelude_exposes_poison_safe_locks() {
    let m = Mutex::new(41u32);
    *lock(&m) += 1;
    assert_eq!(*lock(&m), 42);

    let rw = RwLock::new(vec![1u8]);
    assert_eq!(lock_read(&rw).len(), 1);
    lock_write(&rw).push(2);
    assert_eq!(lock_read(&rw).len(), 2);
}

#[test]
fn prelude_exposes_load_cache() {
    let cache: LoadCache<String, u32, String> =
        LoadCache::new(8, |k: &String| k.len().try_into().map_err(|e| format!("{e}"))).unwrap();
    cache.insert("hello".to_string(), 5);
    assert_eq!(cache.get("hello"), Some(5));
    assert_eq!(cache.get_or_load("world".to_string()).unwrap(), 5);
}

#[test]
fn prelude_exposes_now_secs() {
    assert!(
        now_secs() > 1_577_836_800,
        "now_secs returned {}, expected > 2020-01-01",
        now_secs()
    );
}
