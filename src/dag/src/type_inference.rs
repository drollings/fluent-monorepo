//! Ontology type-hierarchy inference.
//!
//! Thin, `i64`-keyed façade over the shared [`HierarchyIndex`] — the single
//! hierarchy-closure implementation in the workspace. Each class occupies an
//! `i64` id; edges are `[child, parent]` subclass pairs. Once built, the
//! index is read-only.
//!
//! Semantics: a registered class is its own subclass; transitive subclass
//! checks walk the closure; edges with an unregistered endpoint are ignored.
//!
//! # Example
//!
//! ```
//! use fluent_dag::type_inference::TypeInference;
//!
//! // Animal (1) ← Mammal (2) ← Cat (3)
//! let ti = TypeInference::build(&[1, 2, 3], &[[2, 1], [3, 2]]);
//! assert!(ti.is_subclass_of(3, 1));  // Cat → Animal (transitive)
//! assert!(ti.is_subclass_of(2, 1));  // Mammal → Animal (direct)
//! assert!(!ti.is_subclass_of(1, 3)); // Animal is not a subclass of Cat
//! ```

use std::collections::HashSet;

use crate::hierarchy::HierarchyIndex;

/// A precomputed subclass-closure index over an ontology's inheritance graph.
///
/// Construct via [`TypeInference::build`], then query with
/// [`TypeInference::is_subclass_of`]. Once built the struct is read-only.
#[derive(Debug)]
pub struct TypeInference {
    index: HierarchyIndex<i64>,
    class_count: usize,
}

impl TypeInference {
    /// Build the transitive subclass closure from a set of class ids and
    /// `[child, parent]` subclass edges.
    ///
    /// Every registered class is its own ancestor. Edges referencing an id
    /// that is not in `class_ids` are silently skipped. An empty `class_ids`
    /// slice produces a valid (empty) graph. Never panics.
    pub fn build(class_ids: &[i64], edges: &[[i64; 2]]) -> Self {
        let registered: HashSet<i64> = class_ids.iter().copied().collect();
        let mut index = HierarchyIndex::new();
        for &id in class_ids {
            index.add_node(id);
        }
        for &[child, parent] in edges {
            if registered.contains(&child) && registered.contains(&parent) {
                index.add_edge(&child, &parent);
            }
        }
        Self {
            index,
            class_count: class_ids.len(),
        }
    }

    /// Returns `true` when `child` is a transitive subclass of `parent`.
    ///
    /// Every registered class is its own subclass. Unregistered ids return
    /// `false` for every pair (including `(x, x)`), matching the original
    /// bitvector index.
    pub fn is_subclass_of(&self, child: i64, parent: i64) -> bool {
        if !self.index.contains(&child) || !self.index.contains(&parent) {
            return false;
        }
        self.index.is_subclass_of(&child, &parent)
    }

    /// Number of classes registered during [`TypeInference::build`].
    pub fn class_count(&self) -> usize {
        self.class_count
    }
}

#[cfg(test)]
#[path = "../tests/type_inference.rs"]
mod tests;
