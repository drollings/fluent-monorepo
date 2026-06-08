pub const NS_YAGO: &str = "http://yago-knowledge.org/resource/";
pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub const NS_OWL: &str = "http://www.w3.org/2002/07/owl#";
pub const NS_XSD: &str = "http://www.w3.org/2001/XMLSchema#";
pub const NS_SCHEMA: &str = "http://schema.org/";
pub const NS_SKOS: &str = "http://www.w3.org/2004/02/skos/core#";

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
    ALL_CLASSES.iter().copied().find(|cls| cls.iri == iri)
}

pub fn superclass_chain(iri: &str) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    let mut current: Option<&str> = Some(iri);
    while let Some(cur) = current {
        chain.push(cur.to_string());
        match lookup_class(cur) {
            Some(cls) => current = cls.superclass,
            None => break,
        }
    }
    chain
}

pub fn is_subclass_of(child_iri: &str, parent_iri: &str) -> bool {
    if child_iri == parent_iri {
        return true;
    }
    let mut current: Option<&str> = Some(child_iri);
    while let Some(cur) = current {
        match lookup_class(cur) {
            Some(cls) => match cls.superclass {
                Some(superclass) if superclass == parent_iri => return true,
                Some(superclass) => current = Some(superclass),
                None => break,
            },
            None => break,
        }
    }
    false
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

pub fn is_whitelisted(iri: &str) -> bool {
    WHITELIST_IRIS.contains(&iri)
}

pub fn is_whitelisted_hash(hash: i64) -> bool {
    let whitelist_hashes: std::collections::HashSet<i64> = WHITELIST_IRIS
        .iter()
        .map(|iri| guidance_rdf::normalize::hash_iri(iri))
        .collect();
    whitelist_hashes.contains(&hash)
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
mod tests {
    use super::*;

    #[test]
    fn test_class_lookup() {
        let cls = lookup_class("http://schema.org/Person");
        assert!(cls.is_some());
        assert_eq!(cls.unwrap().label, "Person");
    }

    #[test]
    fn test_class_lookup_unknown() {
        assert!(lookup_class("http://unknown.example/").is_none());
    }

    #[test]
    fn test_property_lookup() {
        let prop = lookup_property("http://www.w3.org/2000/01/rdf-schema#label");
        assert!(prop.is_some());
        assert_eq!(prop.unwrap().lod_target, Some(4));
    }

    #[test]
    fn test_property_lookup_type() {
        let prop = lookup_property("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        assert!(prop.is_some());
        assert_eq!(prop.unwrap().lod_target, None);
    }

    #[test]
    fn test_superclass_chain_person() {
        let chain = superclass_chain("http://schema.org/Person");
        assert!(chain.len() >= 2, "chain length: {}", chain.len());
        assert_eq!(chain[0], "http://schema.org/Person");
        assert_eq!(chain[1], "http://yago-knowledge.org/resource/Entity");
    }

    #[test]
    fn test_domain_validation() {
        let prop = lookup_property("http://yago-knowledge.org/resource/hasGender");
        assert!(prop.is_some());
        assert_eq!(prop.unwrap().domain, Some("http://schema.org/Person"));
    }

    #[test]
    fn test_transitive_property() {
        let prop = lookup_property("http://www.w3.org/2000/01/rdf-schema#subClassOf");
        assert!(prop.unwrap().transitive);
    }

    #[test]
    fn test_is_subclass_of_identity() {
        assert!(is_subclass_of(
            "http://schema.org/Person",
            "http://schema.org/Person"
        ));
    }

    #[test]
    fn test_is_subclass_of_direct() {
        assert!(is_subclass_of(
            "http://schema.org/Person",
            "http://yago-knowledge.org/resource/Entity"
        ));
    }

    #[test]
    fn test_is_subclass_of_unrelated() {
        assert!(!is_subclass_of(
            "http://schema.org/Person",
            "http://schema.org/Product"
        ));
    }

    #[test]
    fn test_is_subclass_of_unknown_child() {
        assert!(!is_subclass_of(
            "http://unknown/Foo",
            "http://yago-knowledge.org/resource/Entity"
        ));
    }

    #[test]
    fn test_whitelist() {
        assert!(is_whitelisted("http://schema.org/Person"));
        assert!(is_whitelisted("http://yago-knowledge.org/resource/Entity"));
        assert!(!is_whitelisted("http://unknown/Foo"));
    }

    #[test]
    fn test_all_classes_count() {
        assert_eq!(ALL_CLASSES.len(), 7);
    }

    #[test]
    fn test_all_properties_count() {
        assert_eq!(ALL_PROPERTIES.len(), 11);
    }
}
