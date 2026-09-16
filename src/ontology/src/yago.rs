pub const NS_YAGO: &str = "http://yago-knowledge.org/resource/";
// Re-homed in `guidance_rdf` (single source for the RDF/RDFS/XSD namespaces).
pub use guidance_rdf::{NS_RDF, NS_RDFS, NS_XSD};
pub const NS_OWL: &str = "http://www.w3.org/2002/07/owl#";
pub const NS_SCHEMA: &str = "http://schema.org/";
pub const NS_SKOS: &str = "http://www.w3.org/2004/02/skos/core#";

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use fluent_dag::hierarchy::HierarchyIndex;
use fluent_types::{local_id_of, property_id_for_iri, InterlinguaId};
use guidance_rdf::normalize::hash_iri;

pub const YAGO_VERSION: &str = "4.5";

#[derive(Debug, Clone, Copy)]
pub struct OntologyClass {
    pub iri: &'static str,
    pub label: &'static str,
    pub superclass: Option<&'static str>,
    pub properties: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PropertyRange {
    Iri,
    String,
    LangString,
    Integer,
    Decimal,
    Boolean,
    DateTime,
    Any,
}

#[derive(Debug, Clone, Copy)]
pub struct OntologyProperty {
    pub iri: &'static str,
    pub label: &'static str,
    pub domain: Option<&'static str>,
    pub range: PropertyRange,
    pub transitive: bool,
    pub symmetric: bool,
    pub lod_target: Option<usize>,
}

const YAGO_ENTITY_PROPS: &[&str] = &[
    "http://www.w3.org/2000/01/rdf-schema#label",
    "http://www.w3.org/2000/01/rdf-schema#comment",
    "http://schema.org/description",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
];

const YAGO_PERSON_PROPS: &[&str] = &[
    "http://yago-knowledge.org/resource/hasGender",
    "http://yago-knowledge.org/resource/hasNationality",
    "http://yago-knowledge.org/resource/bornIn",
    "http://yago-knowledge.org/resource/diedIn",
    "http://yago-knowledge.org/resource/hasWikipediaArticle",
];

const YAGO_ORG_PROPS: &[&str] = &["http://yago-knowledge.org/resource/hasWikipediaArticle"];

pub const CLASS_ENTITY: OntologyClass = OntologyClass {
    iri: "http://yago-knowledge.org/resource/Entity",
    label: "Entity",
    superclass: None,
    properties: YAGO_ENTITY_PROPS,
};

pub const CLASS_PERSON: OntologyClass = OntologyClass {
    iri: "http://schema.org/Person",
    label: "Person",
    superclass: Some("http://yago-knowledge.org/resource/Entity"),
    properties: YAGO_PERSON_PROPS,
};

pub const CLASS_ORGANIZATION: OntologyClass = OntologyClass {
    iri: "http://schema.org/Organization",
    label: "Organization",
    superclass: Some("http://yago-knowledge.org/resource/Entity"),
    properties: YAGO_ORG_PROPS,
};

pub const CLASS_LOCATION: OntologyClass = OntologyClass {
    iri: "http://schema.org/Place",
    label: "Location",
    superclass: Some("http://yago-knowledge.org/resource/Entity"),
    properties: &[],
};

pub const CLASS_EVENT: OntologyClass = OntologyClass {
    iri: "http://schema.org/Event",
    label: "Event",
    superclass: Some("http://yago-knowledge.org/resource/Entity"),
    properties: &[],
};

pub const CLASS_ARTIFACT: OntologyClass = OntologyClass {
    iri: "http://yago-knowledge.org/resource/Artifact",
    label: "Artifact",
    superclass: Some("http://yago-knowledge.org/resource/Entity"),
    properties: &[],
};

pub const CLASS_CONCEPT: OntologyClass = OntologyClass {
    iri: "http://yago-knowledge.org/resource/Concept",
    label: "Concept",
    superclass: Some("http://yago-knowledge.org/resource/Entity"),
    properties: &[],
};

pub const ALL_CLASSES: &[&OntologyClass] = &[
    &CLASS_ENTITY,
    &CLASS_PERSON,
    &CLASS_ORGANIZATION,
    &CLASS_LOCATION,
    &CLASS_EVENT,
    &CLASS_ARTIFACT,
    &CLASS_CONCEPT,
];

pub fn lookup_class(iri: &str) -> Option<&'static OntologyClass> {
    static BY_IRI: LazyLock<HashMap<&'static str, &'static OntologyClass>> =
        LazyLock::new(|| ALL_CLASSES.iter().map(|cls| (cls.iri, *cls)).collect());
    BY_IRI.get(iri).copied()
}

/// The static class hierarchy, built once from [`ALL_CLASSES`] through the
/// shared [`HierarchyIndex`] (the workspace's single closure implementation).
static CLASS_HIERARCHY: LazyLock<HierarchyIndex<String>> = LazyLock::new(|| {
    let mut index = HierarchyIndex::new();
    for cls in ALL_CLASSES {
        index.add_node(cls.iri.to_string());
        if let Some(parent) = cls.superclass {
            index.add_edge(&cls.iri.to_string(), &parent.to_string());
        }
    }
    index
});

pub fn superclass_chain(iri: &str) -> Vec<String> {
    let mut chain: Vec<String> = vec![iri.to_string()];
    chain.extend(CLASS_HIERARCHY.ancestors(&iri.to_string()));
    chain
}

pub fn is_subclass_of(child_iri: &str, parent_iri: &str) -> bool {
    CLASS_HIERARCHY.is_subclass_of(&child_iri.to_string(), &parent_iri.to_string())
}

pub const WHITELIST_IRIS: &[&str] = &[
    "http://yago-knowledge.org/resource/Entity",
    "http://schema.org/Person",
    "http://schema.org/Organization",
    "http://schema.org/Place",
    "http://schema.org/Event",
    "http://yago-knowledge.org/resource/Artifact",
    "http://yago-knowledge.org/resource/Concept",
];

/// Whitelist membership, precomputed once from [`ALL_CLASSES`] (the single
/// source — `WHITELIST_IRIS` above is the boot-visible spelling of the same
/// set, pinned equal by test).
static WHITELIST_SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ALL_CLASSES.iter().map(|cls| cls.iri).collect());

/// Truncated 48-bit locals of the whitelist, precomputed once for the
/// id/hash checks below (no per-call rehash of the whitelist).
static WHITELIST_LOCALS: LazyLock<HashSet<i64>> = LazyLock::new(|| {
    WHITELIST_SET
        .iter()
        .map(|iri| local_id_of(hash_iri(iri)))
        .collect()
});

pub fn is_whitelisted(iri: &str) -> bool {
    WHITELIST_SET.contains(iri)
}

/// Whether an [`InterlinguaId`] names a whitelisted YaGO class (ROADMAP
/// §13.6 — namespace-aware, truncation-correct). Compares the **48-bit local**
/// against the truncated whitelist hashes.
pub fn is_whitelisted_id(id: InterlinguaId) -> bool {
    id.is_yago() && WHITELIST_LOCALS.contains(&id.local_id())
}

/// Truncation-aware hash whitelist check. Legacy callers pass a full-width
/// `hash_iri` value; comparing full hashes silently misses a *truncated*
/// interlingua id, so both sides are reduced to the 48-bit local (DRY — the
/// same truncation `is_whitelisted_id` uses).
pub fn is_whitelisted_hash(hash: i64) -> bool {
    WHITELIST_LOCALS.contains(&local_id_of(hash))
}

/// The deterministic `RDF_PROPERTY` interlingua id for an ontology property
/// (ROADMAP §13.4). Lets the ledger reference typed relations by id.
pub fn property_interlingua_id(prop: &OntologyProperty) -> InterlinguaId {
    property_id_for_iri(prop.iri)
}

/// The `RDF_PROPERTY` id for a property IRI, when it is one of the known
/// [`OntologyProperty`] constants.
pub fn property_interlingua_id_by_iri(iri: &str) -> Option<InterlinguaId> {
    lookup_property(iri).map(property_interlingua_id)
}

/// The `RDF_PROPERTY` id for the `rdfs:subClassOf` relation.
pub fn subclass_property_id() -> InterlinguaId {
    property_interlingua_id(&PROP_SUBCLASS)
}

pub const PROP_LABEL: OntologyProperty = OntologyProperty {
    iri: "http://www.w3.org/2000/01/rdf-schema#label",
    label: "label",
    domain: None,
    range: PropertyRange::LangString,
    transitive: false,
    symmetric: false,
    lod_target: Some(4),
};

pub const PROP_COMMENT: OntologyProperty = OntologyProperty {
    iri: "http://www.w3.org/2000/01/rdf-schema#comment",
    label: "comment",
    domain: None,
    range: PropertyRange::LangString,
    transitive: false,
    symmetric: false,
    lod_target: Some(0),
};

pub const PROP_DESCRIPTION: OntologyProperty = OntologyProperty {
    iri: "http://schema.org/description",
    label: "description",
    domain: None,
    range: PropertyRange::LangString,
    transitive: false,
    symmetric: false,
    lod_target: Some(1),
};

pub const PROP_PREF_LABEL: OntologyProperty = OntologyProperty {
    iri: "http://www.w3.org/2004/02/skos/core#prefLabel",
    label: "prefLabel",
    domain: None,
    range: PropertyRange::LangString,
    transitive: false,
    symmetric: false,
    lod_target: Some(4),
};

pub const PROP_TYPE: OntologyProperty = OntologyProperty {
    iri: "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
    label: "type",
    domain: None,
    range: PropertyRange::Iri,
    transitive: false,
    symmetric: false,
    lod_target: None,
};

pub const PROP_SUBCLASS: OntologyProperty = OntologyProperty {
    iri: "http://www.w3.org/2000/01/rdf-schema#subClassOf",
    label: "subClassOf",
    domain: None,
    range: PropertyRange::Iri,
    transitive: true,
    symmetric: false,
    lod_target: None,
};

pub const PROP_HAS_GENDER: OntologyProperty = OntologyProperty {
    iri: "http://yago-knowledge.org/resource/hasGender",
    label: "hasGender",
    domain: Some("http://schema.org/Person"),
    range: PropertyRange::Iri,
    transitive: false,
    symmetric: false,
    lod_target: None,
};

pub const PROP_HAS_NATIONALITY: OntologyProperty = OntologyProperty {
    iri: "http://yago-knowledge.org/resource/hasNationality",
    label: "hasNationality",
    domain: Some("http://schema.org/Person"),
    range: PropertyRange::Iri,
    transitive: false,
    symmetric: false,
    lod_target: None,
};

pub const PROP_BORN_IN: OntologyProperty = OntologyProperty {
    iri: "http://yago-knowledge.org/resource/bornIn",
    label: "bornIn",
    domain: Some("http://schema.org/Person"),
    range: PropertyRange::Iri,
    transitive: false,
    symmetric: false,
    lod_target: None,
};

pub const PROP_DIED_IN: OntologyProperty = OntologyProperty {
    iri: "http://yago-knowledge.org/resource/diedIn",
    label: "diedIn",
    domain: Some("http://schema.org/Person"),
    range: PropertyRange::Iri,
    transitive: false,
    symmetric: false,
    lod_target: None,
};

pub const PROP_WIKIPEDIA: OntologyProperty = OntologyProperty {
    iri: "http://yago-knowledge.org/resource/hasWikipediaArticle",
    label: "hasWikipediaArticle",
    domain: None,
    range: PropertyRange::Iri,
    transitive: false,
    symmetric: false,
    lod_target: None,
};

pub const ALL_PROPERTIES: &[&OntologyProperty] = &[
    &PROP_LABEL,
    &PROP_COMMENT,
    &PROP_DESCRIPTION,
    &PROP_PREF_LABEL,
    &PROP_TYPE,
    &PROP_SUBCLASS,
    &PROP_HAS_GENDER,
    &PROP_HAS_NATIONALITY,
    &PROP_BORN_IN,
    &PROP_DIED_IN,
    &PROP_WIKIPEDIA,
];

pub fn lookup_property(iri: &str) -> Option<&'static OntologyProperty> {
    ALL_PROPERTIES.iter().copied().find(|prop| prop.iri == iri)
}

#[cfg(test)]
#[path = "../tests/yago.rs"]
mod tests;
