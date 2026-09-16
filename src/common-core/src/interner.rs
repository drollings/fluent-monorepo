//! Capability registry: thread-safe string ↔ BitVec index for capability bitsets.

use bitvec::vec::BitVec;
use internment::ArcIntern;
use std::collections::HashMap;
use std::sync::RwLock;

use crate::sync::{lock_read, lock_write};

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
        if let Some(&idx) = lock_read(&self.names).get(&interned) {
            return idx;
        }
        let mut names = lock_write(&self.names);
        // Re-check under the write lock: another thread may have interned
        // the same name between the read and the write.
        if let Some(&idx) = names.get(&interned) {
            return idx;
        }
        let idx = names.len();
        names.insert(interned.clone(), idx);
        lock_write(&self.indices).push(interned);
        idx
    }

    pub fn get_index(&self, name: &str) -> Option<usize> {
        let interned: ArcIntern<str> = ArcIntern::from(name);
        lock_read(&self.names).get(&interned).copied()
    }

    pub fn get_name(&self, idx: usize) -> Option<ArcIntern<str>> {
        lock_read(&self.indices).get(idx).cloned()
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
        let indices = lock_read(&self.indices);
        bits.iter_ones()
            .filter_map(|i| indices.get(i).cloned())
            .collect()
    }

    pub fn count(&self) -> usize {
        lock_read(&self.names).len()
    }

    /// Test-only probe: take the `names` write lock and panic while holding
    /// it, deliberately poisoning the lock so the poison-safe
    /// [`crate::sync`] recovery can be exercised. Hidden from the public
    /// API surface.
    #[doc(hidden)]
    pub fn poison_names_for_test(&self) {
        let _guard = self.names.write().unwrap();
        panic!("intentional interner lock poison (test)");
    }
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

