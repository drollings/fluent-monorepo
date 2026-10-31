use bitvec::vec::BitVec;
use bon::Builder;
pub use fluent_types::{ExecutorKind, TargetType};
use internment::ArcIntern;
use std::collections::{HashMap, HashSet};

use crate::error::RegistryError;

/// Resolver-plane record: bitset capabilities plus identity.
///
/// `depends`/`provides` are `BitVec` indexed by [`CapabilityRegistry`]
/// bit index (compact, O(1) membership). Execution-plane [`WorkUnit`]s use
/// `Vec<ArcIntern<str>>` for the same names instead — convert at the bridge
/// (`TargetWorkUnit::from_target` maps each set bit through the registry;
/// unmapped bits warn and skip). `id` is the target's bit index in the
/// *target-id* space, which shares the `usize` representation with the
/// capability-bit space but is never interchangeable: use [`get`](Self::get)
/// (by name), [`get_by_index`](Self::get_by_index) (Vec position), or
/// [`get_by_bit_index`](Self::get_by_bit_index) (by `id`) accordingly.
/// `executor` records the intended runtime (`Native` for yamake-loaded
/// targets); dispatch reads it, the resolver itself does not execute.
#[derive(Debug, Clone, Builder)]
#[builder(start_fn = new)]
pub struct Target {
    pub id: i64,
    pub name: ArcIntern<str>,
    pub target_type: TargetType,
    pub executor: ExecutorKind,
    pub depends: BitVec,
    pub provides: BitVec,
    #[builder(default)]
    pub command: String,
    #[builder(default = false)]
    pub essential: bool,
}

#[derive(Debug, Clone)]
pub struct TargetRegistry {
    targets: Vec<Target>,
    by_name: HashMap<ArcIntern<str>, usize>,
    by_bit_index: HashMap<usize, usize>,
    providers: HashMap<usize, Vec<usize>>,
}

impl TargetRegistry {
    pub fn new() -> Self {
        Self {
            targets: Vec::new(),
            by_name: HashMap::new(),
            by_bit_index: HashMap::new(),
            providers: HashMap::new(),
        }
    }

    pub fn register(&mut self, target: Target) -> Result<(), RegistryError> {
        if self.by_name.contains_key(&target.name) {
            return Err(RegistryError::DuplicateTarget {
                name: target.name.to_string(),
            });
        }
        let idx = self.targets.len();
        let bit_idx = target.id as usize;
        self.by_name.insert(target.name.clone(), idx);
        self.by_bit_index.insert(bit_idx, idx);
        // Store Vec position (not target id): capability and target-id spaces
        // are both `usize` and must never be conflated.
        for cap_idx in target.provides.iter_ones() {
            self.providers.entry(cap_idx).or_default().push(idx);
        }
        self.targets.push(target);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Target> {
        let interned: ArcIntern<str> = ArcIntern::from(name);
        self.by_name.get(&interned).map(|&idx| &self.targets[idx])
    }

    pub fn get_by_index(&self, idx: usize) -> Option<&Target> {
        self.targets.get(idx)
    }

    pub fn get_by_bit_index(&self, bit_idx: usize) -> Option<&Target> {
        self.by_bit_index
            .get(&bit_idx)
            .map(|&idx| &self.targets[idx])
    }

    pub fn get_providers(&self, capability_bit_index: usize) -> Vec<&Target> {
        self.providers
            .get(&capability_bit_index)
            .map(|indices| {
                indices
                    .iter()
                    .filter_map(|idx| self.targets.get(*idx))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn find_providers(&self, required: &BitVec) -> Vec<&Target> {
        // Intersect the `providers` posting lists (smallest first) instead of
        // scanning every target: the work is proportional to the candidate
        // set, and no `BitVec` is cloned. An empty requirement matches every
        // target (vacuous superset). A required bit with no posting list is
        // unsatisfiable (strict superset semantics — out-of-range bits
        // disqualify rather than being silently dropped).
        let mut bits: Vec<usize> = required.iter_ones().collect();
        if bits.is_empty() {
            return self.targets.iter().collect();
        }
        if bits
            .iter()
            .any(|b| self.providers.get(b).is_none_or(Vec::is_empty))
        {
            return Vec::new();
        }
        bits.sort_by_key(|b| self.providers.get(b).map_or(0, Vec::len));
        let mut candidates: HashSet<usize> = self
            .providers
            .get(&bits[0])
            .map(|post| post.iter().copied().collect())
            .unwrap_or_default();
        for b in &bits[1..] {
            if let Some(post) = self.providers.get(b) {
                // `post` is a Vec posting list; build a set once so the
                // retain is O(candidates) instead of O(candidates × post).
                let post_set: HashSet<usize> = post.iter().copied().collect();
                candidates.retain(|c| post_set.contains(c));
            }
            if candidates.is_empty() {
                return Vec::new();
            }
        }
        // Insertion order (as before); the per-candidate superset check is a
        // zero-alloc verification of the posting-list invariant that also
        // tolerates provides vectors shorter than the requirement (`get`
        // returns `None` instead of panicking).
        self.targets
            .iter()
            .enumerate()
            .filter(|(idx, t)| {
                candidates.contains(idx)
                    && bits.iter().all(|b| t.provides.get(*b).is_some_and(|x| *x))
            })
            .map(|(_, t)| t)
            .collect()
    }

    pub fn list_names(&self) -> Vec<ArcIntern<str>> {
        self.targets.iter().map(|t| t.name.clone()).collect()
    }

    pub fn essential_targets(&self) -> Vec<&Target> {
        self.targets.iter().filter(|t| t.essential).collect()
    }

    pub fn abstract_targets(&self) -> Vec<&Target> {
        self.targets
            .iter()
            .filter(|t| t.target_type == TargetType::Abstract)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.targets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

impl Default for TargetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub use common_core::interner::CapabilityRegistry;

#[cfg(test)]
#[path = "../tests/target.rs"]
mod tests;
