use bitvec::vec::BitVec;
use guidance_common::error::RegistryError;
use guidance_common::registry::Target;
use internment::ArcIntern;
use std::collections::HashMap;

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

        for cap_idx in target.provides.iter_ones() {
            self.providers.entry(cap_idx).or_default().push(bit_idx);
        }

        self.targets.push(target);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Target> {
        let interned: ArcIntern<str> = ArcIntern::from(name);
        self.by_name.get(&interned).map(|&idx| &self.targets[idx])
    }

    pub fn get_by_bit_index(&self, bit_idx: usize) -> Option<&Target> {
        self.by_bit_index.get(&bit_idx).map(|&idx| &self.targets[idx])
    }

    pub fn get_by_index(&self, idx: usize) -> Option<&Target> {
        self.targets.get(idx)
    }

    pub fn find_providers(&self, required: &BitVec) -> Vec<&Target> {
        self.targets
            .iter()
            .filter(|t| {
                let prov = &t.provides;
                let missing: BitVec = required.clone() & !prov.clone();
                missing.not_any()
            })
            .collect()
    }

    pub fn get_providers(&self, capability_bit_index: usize) -> Vec<&Target> {
        self.targets
            .iter()
            .filter(|t| {
                let prov = &t.provides;
                capability_bit_index < prov.len() && prov[capability_bit_index]
            })
            .collect()
    }

    pub fn list_names(&self) -> Vec<ArcIntern<str>> {
        self.targets.iter().map(|t| t.name.clone()).collect()
    }

    pub fn essential_targets(&self) -> Vec<&Target> {
        self.targets.iter().filter(|t| t.essential).collect()
    }

    pub fn targets(&self) -> &[Target] {
        &self.targets
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

#[cfg(test)]
mod tests {
    use super::*;
    use bitvec::prelude::*;
    use guidance_common::types::{ExecutorKind, TargetType};

    #[test]
    fn test_register_and_get() {
        let mut reg = TargetRegistry::new();
        let t = Target::new()
            .id(1)
            .name("build".into())
            .target_type(TargetType::File)
            .executor(ExecutorKind::Native)
            .depends(bitvec![0, 1])
            .provides(bitvec![1, 0])
            .build();
        reg.register(t).unwrap();
        assert_eq!(reg.len(), 1);
        assert!(reg.get("build").is_some());
    }

    #[test]
    fn test_provider_map_consistency() {
        let mut reg = TargetRegistry::new();
        let mut provides = BitVec::new();
        provides.resize(10, false);
        provides.set(3, true);

        let t = Target::new()
            .id(1)
            .name("provider1".into())
            .target_type(TargetType::File)
            .executor(ExecutorKind::Native)
            .depends(BitVec::new())
            .provides(provides)
            .build();
        reg.register(t).unwrap();

        let providers = reg.get_providers(3);
        assert_eq!(providers.len(), 1);

        let providers = reg.get_providers(0);
        assert!(providers.is_empty());
    }

    #[test]
    fn test_find_providers() {
        let mut reg = TargetRegistry::new();
        let mut provides = BitVec::new();
        provides.resize(5, false);
        provides.set(0, true);
        provides.set(1, true);

        let t = Target::new()
            .id(1)
            .name("p".into())
            .target_type(TargetType::File)
            .executor(ExecutorKind::Native)
            .depends(BitVec::new())
            .provides(provides)
            .build();
        reg.register(t).unwrap();

        let mut required = BitVec::new();
        required.resize(5, false);
        required.set(0, true);

        let providers = reg.find_providers(&required);
        assert_eq!(providers.len(), 1);
    }

    #[test]
    fn test_list_names() {
        let mut reg = TargetRegistry::new();
        for i in 0..3 {
            let t = Target::new()
                .id(i)
                .name(ArcIntern::from(format!("n{i}")))
                .target_type(TargetType::File)
                .executor(ExecutorKind::Native)
                .depends(BitVec::new())
                .provides(BitVec::new())
                .build();
            reg.register(t).unwrap();
        }
        assert_eq!(reg.list_names().len(), 3);
    }
}
