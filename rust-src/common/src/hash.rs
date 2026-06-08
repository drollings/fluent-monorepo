use blake3::Hasher;
use sha2::{Digest, Sha256, Sha512};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    Sha256,
    Sha512,
    Blake3,
}

impl HashAlgorithm {
    pub fn digest_length(self) -> usize {
        match self {
            HashAlgorithm::Sha256 => 32,
            HashAlgorithm::Sha512 => 64,
            HashAlgorithm::Blake3 => 32,
        }
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex_encode(&result)
}

pub fn blake3_hash(data: &[u8]) -> [u8; 32] {
    blake3::hash(data).into()
}

pub fn blake3_hex(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

pub fn fnv1a64(input: &[u8]) -> u64 {
    const OFFSET: u64 = 14695981039346656037;
    const PRIME: u64 = 1099511628211;
    let mut hash: u64 = OFFSET;
    for &byte in input {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

pub fn content_hash_with_model(content: &str, model: &str) -> [u8; 16] {
    let mut hasher = Sha256::new();
    hasher.update(model.as_bytes());
    hasher.update(b"\x00");
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&result[..16]);
    out
}

pub struct HashState {
    algorithm: HashAlgorithm,
    sha256: Option<Sha256>,
    sha512: Option<Sha512>,
    blake3: Option<Hasher>,
}

impl HashState {
    pub fn new(algorithm: HashAlgorithm) -> Self {
        match algorithm {
            HashAlgorithm::Sha256 => Self {
                algorithm,
                sha256: Some(Sha256::new()),
                sha512: None,
                blake3: None,
            },
            HashAlgorithm::Sha512 => Self {
                algorithm,
                sha256: None,
                sha512: Some(Sha512::new()),
                blake3: None,
            },
            HashAlgorithm::Blake3 => Self {
                algorithm,
                sha256: None,
                sha512: None,
                blake3: Some(Hasher::new()),
            },
        }
    }

    pub fn update(&mut self, data: &[u8]) {
        if let Some(ref mut h) = self.sha256 {
            h.update(data);
        }
        if let Some(ref mut h) = self.sha512 {
            h.update(data);
        }
        if let Some(ref mut h) = self.blake3 {
            h.update(data);
        }
    }

    pub fn finalize(&self) -> Vec<u8> {
        if let Some(ref h) = self.sha256 {
            h.clone().finalize().to_vec()
        } else if let Some(ref h) = self.sha512 {
            h.clone().finalize().to_vec()
        } else if let Some(ref h) = self.blake3 {
            h.clone().finalize().as_bytes().to_vec()
        } else {
            Vec::new()
        }
    }

    pub fn digest_hex(&self) -> String {
        hex_encode(&self.finalize())
    }
}

pub struct QueryCache {
    entries: std::collections::HashMap<String, CacheEntry>,
    max_entries: usize,
}

struct CacheEntry {
    result: String,
    lower_query: String,
}

impl QueryCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            max_entries,
        }
    }

    pub fn get(&self, query: &str) -> Option<&str> {
        let key = fnv1a64(query.to_lowercase().as_bytes());
        let key_str = format!("{:016x}", key);
        self.entries.get(&key_str).map(|e| e.result.as_str())
    }

    pub fn put(&mut self, query: &str, result: String) {
        if self.entries.len() >= self.max_entries {
            self.entries.clear();
        }
        let key = fnv1a64(query.to_lowercase().as_bytes());
        let key_str = format!("{:016x}", key);
        self.entries.insert(
            key_str,
            CacheEntry {
                result,
                lower_query: query.to_lowercase(),
            },
        );
    }
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_correct_length() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn sha256_hex_deterministic() {
        assert_eq!(sha256_hex(b"hello"), sha256_hex(b"hello"));
    }

    #[test]
    fn sha256_hex_differs_for_different_inputs() {
        assert_ne!(sha256_hex(b"hello"), sha256_hex(b"world"));
    }

    #[test]
    fn blake3_hash_length() {
        let h = blake3_hash(b"test");
        assert_eq!(h.len(), 32);
    }

    #[test]
    fn blake3_hex_length() {
        let h = blake3_hex(b"test");
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn fnv1a64_basic() {
        let h = fnv1a64(b"hello");
        assert_ne!(h, 0);
        assert_eq!(fnv1a64(b"hello"), fnv1a64(b"hello"));
    }

    #[test]
    fn digest_length_values() {
        assert_eq!(HashAlgorithm::Sha256.digest_length(), 32);
        assert_eq!(HashAlgorithm::Sha512.digest_length(), 64);
        assert_eq!(HashAlgorithm::Blake3.digest_length(), 32);
    }

    #[test]
    fn hash_state_incremental_sha256() {
        let mut state = HashState::new(HashAlgorithm::Sha256);
        state.update(b"hello ");
        state.update(b"world");
        let hex = state.digest_hex();
        assert_eq!(hex.len(), 64);
    }

    #[test]
    fn content_hash_deterministic() {
        let h1 = content_hash_with_model("hello", "model");
        let h2 = content_hash_with_model("hello", "model");
        assert_eq!(h1, h2);
    }

    #[test]
    fn content_hash_model_sensitive() {
        let h1 = content_hash_with_model("hello", "model-a");
        let h2 = content_hash_with_model("hello", "model-b");
        assert_ne!(h1, h2);
    }

    #[test]
    fn query_cache_hit_miss() {
        let mut cache = QueryCache::new(10);
        assert!(cache.get("hello").is_none());
        cache.put("hello", "world".to_string());
        assert_eq!(cache.get("hello"), Some("world"));
    }
}
