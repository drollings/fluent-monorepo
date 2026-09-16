//! YagoView — runtime file, safe `std::fs::read` (no `mmap`), TTL parse via the
//! crate's single Turtle reader, `HierarchyIndex` memo.
//!
//! The YaGO class hierarchy is knowledge-half state, owned by the same crate
//! as [`YaGoLoader`](crate::yago_loader). TTL is read through
//! [`crate::turtle`] (the canonical `guidance_rdf` parser) and the closure is
//! the shared [`HierarchyIndex`]; there is no hand-rolled line parser here.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::Path;

use fluent_dag::hierarchy::HierarchyIndex;
use fluent_types::{yago_class_id_for_iri, InterlinguaId};

use crate::normalize::iri_to_curie;
use crate::OntologyError;

/// Runtime YaGO view.
#[derive(Debug)]
pub struct YagoView {
    // fst::Map curie -> InterlinguaId (stored as HashMap for now; fst codegen lands later — O(log n) vs O(1) but correct)
    classes: HashMap<String, InterlinguaId>,
    hierarchy: HierarchyIndex<InterlinguaId>,
}

impl YagoView {
    /// Load from `path` via safe `std::fs::read` + simple TTL parse (hermetic `n2` fixture). Validates `YSM1` header if present; otherwise parses TTL directly.
    pub fn load(path: &Path) -> Result<Self, OntologyError> {
        let data = std::fs::read(path)
            .map_err(|e| OntologyError::YagoView(format!("read yago {}: {e}", path.display())))?;
        // If file starts with YSM1 magic, decode postcard/fst
        // For hermetic n2, file is TTL; parse streaming.
        let text = String::from_utf8_lossy(&data);
        if text.starts_with("@prefix") {
            return Ok(Self::from_ttl_str(&text));
        }
        // Binary leg: a YSM1 magic word routes to the binary decoder, which
        // verifies format version, taxonomy version, and the content digest.
        // Any rejection falls through to the caller's provisional path via
        // the typed error (the TTL leg is never attempted on magic bytes).
        if data.len() >= crate::ysm1::YSM1_HEADER_LEN {
            let magic = u32::from_le_bytes(
                data[0..4]
                    .try_into()
                    .map_err(|_| OntologyError::YagoView("short YaGO header".into()))?,
            );
            if magic == crate::ysm1::YSM1_MAGIC_U32 {
                let decoded = crate::ysm1::decode(&data)?;
                return Ok(Self::from_decoded(decoded));
            }
        }
        Ok(Self::from_ttl_str(&text))
    }

    /// Build from a verified YSM1 payload: the class table plus hierarchy
    /// edges feed the same shared [`HierarchyIndex`] the TTL leg builds, so
    /// `ancestors_of` ordering (nearest first) is identical either way.
    fn from_decoded(decoded: crate::ysm1::Ysm1File) -> Self {
        let mut classes: HashMap<String, InterlinguaId> = HashMap::new();
        for (curie, id) in decoded.classes {
            classes.insert(curie, id);
        }
        let hierarchy = HierarchyIndex::from_edges(decoded.edges);
        Self { classes, hierarchy }
    }

    fn from_ttl_str(ttl: &str) -> Self {
        let (_, edges) = crate::turtle::subclass_edges(ttl);
        let mut classes: HashMap<String, InterlinguaId> = HashMap::new();
        let mut hierarchy_edges: Vec<(InterlinguaId, InterlinguaId)> = Vec::new();
        for (subject_iri, object_iri) in edges {
            let subject_id = yago_class_id_for_iri(&subject_iri);
            let object_id = yago_class_id_for_iri(&object_iri);
            classes
                .entry(iri_to_curie(&subject_iri))
                .or_insert(subject_id);
            classes
                .entry(iri_to_curie(&object_iri))
                .or_insert(object_id);
            hierarchy_edges.push((subject_id, object_id));
        }
        // Build the subclass closure through the shared hierarchy index.
        let hierarchy = HierarchyIndex::from_edges(hierarchy_edges);
        Self { classes, hierarchy }
    }
    pub fn resolve_curie(&self, curie: &str) -> Option<InterlinguaId> {
        self.classes.get(curie).copied()
    }

    /// All transitive superclasses of `id`, nearest first, via the shared
    /// [`HierarchyIndex`] (memoized, cycle-resilient).
    pub fn ancestors_of(&self, id: InterlinguaId) -> Vec<InterlinguaId> {
        self.hierarchy.ancestors(&id)
    }

    pub fn is_subclass_of(&self, child: InterlinguaId, parent: InterlinguaId) -> bool {
        self.hierarchy.is_subclass_of(&child, &parent)
    }

    pub fn class_count(&self) -> usize {
        self.classes.len()
    }
    pub fn classes_iter(&self) -> impl Iterator<Item = (String, InterlinguaId)> + '_ {
        self.classes.iter().map(|(k, v)| (k.clone(), *v))
    }
}

#[cfg(test)]
#[path = "../tests/yago_view.rs"]
mod tests;
