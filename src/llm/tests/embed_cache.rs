//! `CachedEmbeddingProvider` behavior: one-inner-call dedup,
//! batch caching with order preservation, empty passthrough, eviction
//! bounds, and single-shard `Arc` sharing.

use super::*;

/// A provider that records every inner call behind the cache decorator.
struct RecordingProvider {
    calls: std::sync::Mutex<Vec<String>>,
    batch_calls: std::sync::Mutex<usize>,
    dims: u32,
}

impl RecordingProvider {
    fn new(dims: u32) -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            batch_calls: std::sync::Mutex::new(0),
            dims,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn batch_calls(&self) -> usize {
        *self.batch_calls.lock().unwrap()
    }
}

impl EmbeddingProvider for RecordingProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn dimensions(&self) -> u32 {
        self.dims
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.calls.lock().unwrap().push(text.to_string());
        Ok(vec![text.len() as f32; self.dims as usize])
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<BatchEmbedding, EmbeddingError> {
        *self.batch_calls.lock().unwrap() += 1;
        let flat: Vec<f32> = texts
            .iter()
            .flat_map(|t| vec![t.len() as f32; self.dims as usize])
            .collect();
        Ok(BatchEmbedding {
            flat,
            count: texts.len(),
            dims: self.dims as usize,
        })
    }
}

#[test]
fn cached_provider_calls_inner_once_per_text() {
    let cached = CachedEmbeddingProvider::new_with_limit(RecordingProvider::new(4), 16);
    let first = cached.embed("hello").unwrap();
    let second = cached.embed("hello").unwrap();
    assert_eq!(first, second);
    assert_eq!(first, vec![5.0; 4]);
    assert_eq!(cached.inner.calls(), vec!["hello".to_string()]);
    // A different text is a different key.
    let _ = cached.embed("world").unwrap();
    assert_eq!(cached.inner.calls().len(), 2);
}

#[tokio::test]
async fn cached_provider_batch_caches_per_text() {
    let cached = CachedEmbeddingProvider::new_with_limit(RecordingProvider::new(3), 16);
    let batch = cached.embed_batch_async(&["a", "bb", "a"]).await.unwrap();
    assert_eq!(batch.count, 3);
    assert_eq!(batch.dims, 3);
    assert_eq!(cached.inner.batch_calls(), 1);
    // Second identical batch is fully cached — no new inner batch call.
    let again = cached.embed_batch_async(&["a", "bb"]).await.unwrap();
    assert_eq!(again.count, 2);
    assert_eq!(cached.inner.batch_calls(), 1);
    // Per-text vectors survive the round trip in request order.
    assert_eq!(again.try_vector(0).unwrap(), &[1.0, 1.0, 1.0]);
    assert_eq!(again.try_vector(1).unwrap(), &[2.0, 2.0, 2.0]);
}

#[test]
fn cached_provider_empty_results_are_never_cached() {
    // The no-op path is passthrough: empty vectors occupy no LRU slots.
    let cached = CachedEmbeddingProvider::new_with_limit(NoopEmbedding::new(8), 16);
    let _ = cached.embed("anything").unwrap();
    let _ = cached.embed("").unwrap();
    let len: usize = cached
        .shards
        .iter()
        .map(|shard| shard.lock().unwrap().len())
        .sum();
    assert_eq!(len, 0);
}

#[test]
fn cached_provider_hits_share_one_allocation() {
    // Repeat hits resolve to the same `Arc` — pointer equality, not just
    // value equality — and the key routes to exactly one shard.
    let cached = CachedEmbeddingProvider::new_with_limit(RecordingProvider::new(4), 64);
    let first = cached.embed("shared").unwrap();
    let second = cached.embed("shared").unwrap();
    assert_eq!(first, second);
    assert_eq!(cached.inner.calls(), vec!["shared".to_string()]);
    // Reach into the shards (same-module tests): the stored `Arc` is one
    // allocation, and only the owning shard holds the key.
    let key = cached.cache_key("shared");
    let shard = super::shard_of(&key);
    let stored = cached.shards[shard]
        .lock()
        .unwrap()
        .get(&key)
        .cloned()
        .expect("key resident in its shard");
    assert_eq!(stored.as_slice(), first.as_slice());
    let elsewhere = cached
        .shards
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != shard)
        .map(|(_, s)| s.lock().unwrap().len())
        .sum::<usize>();
    assert_eq!(elsewhere, 0, "single shard holds the key");
    // Two reads of the stored `Arc` are the same allocation.
    let reread = cached.shards[shard]
        .lock()
        .unwrap()
        .get(&key)
        .cloned()
        .expect("key still resident");
    assert!(std::sync::Arc::ptr_eq(&stored, &reread));
}

#[test]
fn cached_provider_evicts_when_full() {
    // Total capacity is shards × per-shard cap: inserting far more distinct
    // keys than the bound guarantees eviction deterministically (no hash
    // luck involved — `inserted - bound` entries must have been evicted).
    let limit = 160;
    let inserted = 2000;
    let provider = CachedEmbeddingProvider::new_with_limit(RecordingProvider::new(4), limit);
    for i in 0..inserted {
        let text = format!("text_{i}");
        let _ = provider.embed(&text);
    }
    let len: usize = provider
        .shards
        .iter()
        .map(|shard| shard.lock().unwrap().len())
        .sum();
    assert!(
        len <= limit,
        "sharded cache stays within its total bound: {len} <= {limit}"
    );
    assert!(
        len < inserted,
        "eviction happened: {len} resident of {inserted} inserted"
    );
}
