use bitvec::prelude::*;
use std::collections::HashMap;

#[derive(Debug)]
pub struct TypeInference {
    ancestors: HashMap<i64, BitVec>,
    class_count: usize,
    id_to_bit: HashMap<i64, usize>,
}

impl TypeInference {
    pub fn build(class_ids: &[i64], edges: &[[i64; 2]]) -> Self {
        let class_count = class_ids.len();
        let mut id_to_bit: HashMap<i64, usize> = HashMap::new();
        for (i, &id) in class_ids.iter().enumerate() {
            id_to_bit.insert(id, i);
        }
        let mut ancestors: HashMap<i64, BitVec> = HashMap::new();
        for &id in class_ids {
            let mut bs = BitVec::repeat(false, class_count);
            if let Some(&bit) = id_to_bit.get(&id) {
                bs.set(bit, true);
            }
            ancestors.insert(id, bs);
        }
        let mut changed = true;
        while changed {
            changed = false;
            let mut updates: Vec<(i64, BitVec)> = Vec::new();
            for &[child, parent] in edges {
                let parent_bit = id_to_bit.get(&parent);
                if let Some(&pb) = parent_bit {
                    if let Some(child_ancestors) = ancestors.get(&child) {
                        if !child_ancestors[pb] || {
                            let parent_ancestors = ancestors.get(&parent);
                            parent_ancestors.is_some_and(|pa| {
                                pa.iter().enumerate().any(|(i, b)| *b && !child_ancestors[i])
                            })
                        } {
                            let mut new_bits = child_ancestors.clone();
                            new_bits.set(pb, true);
                            if let Some(parent_ancestors) = ancestors.get(&parent) {
                                for (i, bit) in parent_ancestors.iter().enumerate() {
                                    if *bit && !new_bits[i] {
                                        new_bits.set(i, true);
                                        changed = true;
                                    }
                                }
                            }
                            if new_bits != *child_ancestors {
                                changed = true;
                            }
                            updates.push((child, new_bits));
                        }
                    }
                }
            }
            for (id, bits) in updates {
                ancestors.insert(id, bits);
            }
        }
        Self {
            ancestors,
            class_count,
            id_to_bit,
        }
    }

    pub fn is_subclass_of(&self, child: i64, parent: i64) -> bool {
        if let (Some(_cb), Some(pb)) = (self.id_to_bit.get(&child), self.id_to_bit.get(&parent)) {
            if let Some(child_ancestors) = self.ancestors.get(&child) {
                return child_ancestors[*pb];
            }
        }
        false
    }

    pub fn class_count(&self) -> usize {
        self.class_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ontology() {
        let ti = TypeInference::build(&[], &[]);
        assert_eq!(ti.class_count(), 0);
    }

    #[test]
    fn class_is_subclass_of_itself() {
        let ti = TypeInference::build(&[1], &[]);
        assert!(ti.is_subclass_of(1, 1));
    }

    #[test]
    fn direct_subclass() {
        let ti = TypeInference::build(&[1, 2], &[[2, 1]]);
        assert!(ti.is_subclass_of(2, 1));
    }

    #[test]
    fn transitive_subclass() {
        let ti = TypeInference::build(&[1, 2, 3], &[[2, 1], [3, 2]]);
        assert!(ti.is_subclass_of(2, 1));
        assert!(ti.is_subclass_of(3, 2));
        assert!(ti.is_subclass_of(3, 1));
    }

    #[test]
    fn unknown_class_returns_false() {
        let ti = TypeInference::build(&[1, 2], &[[2, 1]]);
        assert!(!ti.is_subclass_of(99, 1));
        assert!(!ti.is_subclass_of(2, 99));
    }
}
