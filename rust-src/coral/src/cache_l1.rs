use dashmap::DashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheTier {
    L1Memory,
    L2WasmWorkflow,
    L3Graph,
    L4Semantic,
    L4_5Decompose,
    L5Frontier,
}

impl std::fmt::Display for CacheTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheTier::L1Memory => write!(f, "L1"),
            CacheTier::L2WasmWorkflow => write!(f, "L2"),
            CacheTier::L3Graph => write!(f, "L3"),
            CacheTier::L4Semantic => write!(f, "L4"),
            CacheTier::L4_5Decompose => write!(f, "L4.5"),
            CacheTier::L5Frontier => write!(f, "L5"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingResult {
    pub query: String,
    pub result: String,
    pub tier: CacheTier,
}

pub struct L1Cache {
    store: DashMap<String, RoutingResult>,
}

impl L1Cache {
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
        }
    }

    pub fn get(&self, query: &str) -> Option<RoutingResult> {
        self.store.get(query).map(|r| r.clone())
    }

    pub fn set(&self, query: String, result: RoutingResult) {
        self.store.insert(query, result);
    }

    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }
}

impl Default for L1Cache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l1_cache_set_and_get() {
        let cache = L1Cache::new();
        let result = RoutingResult {
            query: "hello".into(),
            result: "world".into(),
            tier: CacheTier::L1Memory,
        };
        cache.set("hello".into(), result.clone());
        let cached = cache.get("hello").expect("should exist");
        assert_eq!(cached.result, "world");
        assert_eq!(cached.tier, CacheTier::L1Memory);
    }

    #[test]
    fn test_l1_cache_miss() {
        let cache = L1Cache::new();
        assert!(cache.get("nonexistent").is_none());
    }

    #[test]
    fn test_l1_cache_empty() {
        let cache = L1Cache::new();
        assert!(cache.is_empty());
    }
}
