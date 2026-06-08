use bitvec::vec::BitVec;
use internment::ArcIntern;
use std::collections::HashMap;
use std::sync::RwLock;

pub struct CapabilityRegistry {
    names: RwLock<HashMap<ArcIntern<str>, usize>>,
    indices: RwLock<Vec<ArcIntern<str>>>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self {
            names: RwLock::new(HashMap::new()),
            indices: RwLock::new(Vec::new()),
        }
    }

    pub fn intern(&self, name: &str) -> usize {
        let interned: ArcIntern<str> = ArcIntern::from(name);

        // fast path: read lock
        {
            let names = self.names.read().unwrap();
            if let Some(&idx) = names.get(&interned) {
                return idx;
            }
        }

        // slow path: write lock
        let mut names = self.names.write().unwrap();
        if let Some(&idx) = names.get(&interned) {
            return idx;
        }
        let idx = names.len();
        names.insert(interned.clone(), idx);
        self.indices.write().unwrap().push(interned);
        idx
    }

    pub fn get_index(&self, name: &str) -> Option<usize> {
        let interned: ArcIntern<str> = ArcIntern::from(name);
        self.names.read().unwrap().get(&interned).copied()
    }

    pub fn get_name(&self, idx: usize) -> Option<ArcIntern<str>> {
        self.indices.read().unwrap().get(idx).cloned()
    }

    pub fn intern_list(&self, names: &[&str]) {
        for name in names {
            self.intern(name);
        }
    }

    pub fn to_bitvec(&self, names: &[&str]) -> BitVec {
        let mut bits = BitVec::new();
        for name in names {
            let idx = self.intern(name);
            if idx >= bits.len() {
                bits.resize(idx + 1, false);
            }
            bits.set(idx, true);
        }
        bits
    }

    pub fn bitvec_to_names(&self, bits: &BitVec) -> Vec<ArcIntern<str>> {
        let indices = self.indices.read().unwrap();
        bits.iter_ones()
            .filter_map(|i| indices.get(i).cloned())
            .collect()
    }

    pub fn count(&self) -> usize {
        self.names.read().unwrap().len()
    }
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn basic_intern_and_retrieve() {
        let reg = CapabilityRegistry::new();
        let idx1 = reg.intern("hello");
        let idx2 = reg.intern("world");
        let idx3 = reg.intern("hello");

        assert_eq!(idx1, 0);
        assert_eq!(idx2, 1);
        assert_eq!(idx3, 0);
        assert_eq!(reg.count(), 2);
    }

    #[test]
    fn get_index_returns_none_for_unknown() {
        let reg = CapabilityRegistry::new();
        reg.intern("known");
        assert_eq!(reg.get_index("known"), Some(0));
        assert_eq!(reg.get_index("unknown"), None);
    }

    #[test]
    fn get_name_roundtrip() {
        let reg = CapabilityRegistry::new();
        reg.intern("foo");
        reg.intern("bar");
        assert_eq!(reg.get_name(0).as_deref(), Some("foo"));
        assert_eq!(reg.get_name(1).as_deref(), Some("bar"));
        assert!(reg.get_name(99).is_none());
    }

    #[test]
    fn intern_list() {
        let reg = CapabilityRegistry::new();
        reg.intern_list(&["a", "b", "c"]);
        assert_eq!(reg.count(), 3);
    }

    #[test]
    fn to_bitvec_roundtrip() {
        let reg = CapabilityRegistry::new();
        reg.intern_list(&["compile", "link", "test"]);
        let bits = reg.to_bitvec(&["compile", "test"]);
        assert!(bits[0]);
        assert!(!bits[1]);
        assert!(bits[2]);
        let names = reg.bitvec_to_names(&bits);
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn concurrent_intern_all_same_index() {
        use std::sync::Arc;

        let reg = Arc::new(CapabilityRegistry::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let r = Arc::clone(&reg);
            handles.push(thread::spawn(move || r.intern("hello")));
        }
        for h in handles {
            assert_eq!(h.join().unwrap(), 0);
        }
        assert_eq!(reg.count(), 1);
    }

    #[test]
    fn bitvec_to_names_empty() {
        let reg = CapabilityRegistry::new();
        let bits = BitVec::new();
        let names = reg.bitvec_to_names(&bits);
        assert!(names.is_empty());
    }
}
