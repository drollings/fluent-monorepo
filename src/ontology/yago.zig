/// yago.zig — YAGO 4.5 Ontology Schema Definition
///
/// Defines YAGO 4.5 classes and properties as Zig structs for compile-time
/// validation and runtime lookup.
///
/// Key exports:
///   lookupClass(iri)    → ?*const OntologyClass
///   lookupProperty(iri) → ?*const OntologyProperty
///   YAGO_VERSION        — version string constant
const std = @import("std");

// ---------------------------------------------------------------------------
// YAGO 4.5 namespace prefixes
// ---------------------------------------------------------------------------
pub const NS_YAGO = "http://yago-knowledge.org/resource/";
pub const NS_RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_RDFS = "http://www.w3.org/2000/01/rdf-schema#";
pub const NS_OWL = "http://www.w3.org/2002/07/owl#";
pub const NS_XSD = "http://www.w3.org/2001/XMLSchema#";
pub const NS_SCHEMA = "http://schema.org/";
pub const NS_SKOS = "http://www.w3.org/2004/02/skos/core#";

pub const YAGO_VERSION = "4.5";

// ---------------------------------------------------------------------------
// Ontology class
// ---------------------------------------------------------------------------

/// Represents a fixed-size buffer pool with ownership and lifecycle management; key invariants include size and allocation limits.
pub const OntologyClass = struct {
    iri: []const u8,
    label: []const u8,
    superclass: ?[]const u8, // null for root class
    properties: []const []const u8, // IRIs of associated properties
};

// ---------------------------------------------------------------------------
// Property range types
// ---------------------------------------------------------------------------
/// Defines a range of property values with fixed bounds; managed centrally; ensures consistent invariants.
pub const PropertyRange = enum {
    iri, // object property (points to another entity)
    string, // plain string literal
    lang_string, // language-tagged string
    integer,
    decimal,
    boolean,
    date_time,
    any, // unconstrained
};

/// Represents a fixed-size buffer structure with ownership and invariants; managed via init/deinit; not thread-safe.
pub const OntologyProperty = struct {
    iri: []const u8,
    label: []const u8,
    domain: ?[]const u8,
    range: PropertyRange,
    transitive: bool,
    symmetric: bool,
    /// Maps to ContextNode LOD level index (0-5)
    lod_target: ?u3,
};

// ---------------------------------------------------------------------------
// YAGO 4.5 class registry (core subset)
// ---------------------------------------------------------------------------

const yago_entity_props = [_][]const u8{
    NS_RDFS ++ "label",
    NS_RDFS ++ "comment",
    NS_SCHEMA ++ "description",
    NS_RDF ++ "type",
};

const yago_person_props = [_][]const u8{
    NS_YAGO ++ "hasGender",
    NS_YAGO ++ "hasNationality",
    NS_YAGO ++ "bornIn",
    NS_YAGO ++ "diedIn",
    NS_YAGO ++ "hasWikipediaArticle",
};

const yago_org_props = [_][]const u8{
    NS_YAGO ++ "hasWikipediaArticle",
};

pub const CLASS_ENTITY = OntologyClass{
    .iri = NS_YAGO ++ "Entity",
    .label = "Entity",
    .superclass = null,
    .properties = &yago_entity_props,
};

pub const CLASS_PERSON = OntologyClass{
    .iri = NS_SCHEMA ++ "Person",
    .label = "Person",
    .superclass = NS_YAGO ++ "Entity",
    .properties = &yago_person_props,
};

pub const CLASS_ORGANIZATION = OntologyClass{
    .iri = NS_SCHEMA ++ "Organization",
    .label = "Organization",
    .superclass = NS_YAGO ++ "Entity",
    .properties = &yago_org_props,
};

pub const CLASS_LOCATION = OntologyClass{
    .iri = NS_SCHEMA ++ "Place",
    .label = "Location",
    .superclass = NS_YAGO ++ "Entity",
    .properties = &.{},
};

pub const CLASS_EVENT = OntologyClass{
    .iri = NS_SCHEMA ++ "Event",
    .label = "Event",
    .superclass = NS_YAGO ++ "Entity",
    .properties = &.{},
};

pub const CLASS_ARTIFACT = OntologyClass{
    .iri = NS_YAGO ++ "Artifact",
    .label = "Artifact",
    .superclass = NS_YAGO ++ "Entity",
    .properties = &.{},
};

pub const CLASS_CONCEPT = OntologyClass{
    .iri = NS_YAGO ++ "Concept",
    .label = "Concept",
    .superclass = NS_YAGO ++ "Entity",
    .properties = &.{},
};

/// All known classes in definition order.
const ALL_CLASSES = [_]*const OntologyClass{
    &CLASS_ENTITY,
    &CLASS_PERSON,
    &CLASS_ORGANIZATION,
    &CLASS_LOCATION,
    &CLASS_EVENT,
    &CLASS_ARTIFACT,
    &CLASS_CONCEPT,
};

/// Look up a class by IRI. Returns null if unknown.
pub fn lookupClass(iri: []const u8) ?*const OntologyClass {
    for (ALL_CLASSES) |cls| {
        if (std.mem.eql(u8, cls.iri, iri)) return cls;
    }
    return null;
}

/// Collect superclass chain for a given class IRI (including self).
/// Out slice must have enough capacity. Returns number written.
pub fn superclassChain(iri: []const u8, out: [][]const u8) usize {
    var count: usize = 0;
    var current: ?[]const u8 = iri;
    while (current) |cur| {
        if (count >= out.len) break;
        out[count] = cur;
        count += 1;
        const cls = lookupClass(cur) orelse break;
        current = cls.superclass;
    }
    return count;
}

// ---------------------------------------------------------------------------
// YAGO 4.5 property registry (core subset)
// ---------------------------------------------------------------------------

pub const PROP_LABEL = OntologyProperty{
    .iri = NS_RDFS ++ "label",
    .label = "label",
    .domain = null,
    .range = .lang_string,
    .transitive = false,
    .symmetric = false,
    .lod_target = 4,
};

pub const PROP_COMMENT = OntologyProperty{
    .iri = NS_RDFS ++ "comment",
    .label = "comment",
    .domain = null,
    .range = .lang_string,
    .transitive = false,
    .symmetric = false,
    .lod_target = 0,
};

pub const PROP_DESCRIPTION = OntologyProperty{
    .iri = NS_SCHEMA ++ "description",
    .label = "description",
    .domain = null,
    .range = .lang_string,
    .transitive = false,
    .symmetric = false,
    .lod_target = 1,
};

pub const PROP_PREF_LABEL = OntologyProperty{
    .iri = NS_SKOS ++ "prefLabel",
    .label = "prefLabel",
    .domain = null,
    .range = .lang_string,
    .transitive = false,
    .symmetric = false,
    .lod_target = 4,
};

pub const PROP_TYPE = OntologyProperty{
    .iri = NS_RDF ++ "type",
    .label = "type",
    .domain = null,
    .range = .iri,
    .transitive = false,
    .symmetric = false,
    .lod_target = null,
};

pub const PROP_SUBCLASS = OntologyProperty{
    .iri = NS_RDFS ++ "subClassOf",
    .label = "subClassOf",
    .domain = null,
    .range = .iri,
    .transitive = true,
    .symmetric = false,
    .lod_target = null,
};

pub const PROP_HAS_GENDER = OntologyProperty{
    .iri = NS_YAGO ++ "hasGender",
    .label = "hasGender",
    .domain = NS_SCHEMA ++ "Person",
    .range = .iri,
    .transitive = false,
    .symmetric = false,
    .lod_target = null,
};

pub const PROP_HAS_NATIONALITY = OntologyProperty{
    .iri = NS_YAGO ++ "hasNationality",
    .label = "hasNationality",
    .domain = NS_SCHEMA ++ "Person",
    .range = .iri,
    .transitive = false,
    .symmetric = false,
    .lod_target = null,
};

pub const PROP_BORN_IN = OntologyProperty{
    .iri = NS_YAGO ++ "bornIn",
    .label = "bornIn",
    .domain = NS_SCHEMA ++ "Person",
    .range = .iri,
    .transitive = false,
    .symmetric = false,
    .lod_target = null,
};

pub const PROP_DIED_IN = OntologyProperty{
    .iri = NS_YAGO ++ "diedIn",
    .label = "diedIn",
    .domain = NS_SCHEMA ++ "Person",
    .range = .iri,
    .transitive = false,
    .symmetric = false,
    .lod_target = null,
};

pub const PROP_WIKIPEDIA = OntologyProperty{
    .iri = NS_YAGO ++ "hasWikipediaArticle",
    .label = "hasWikipediaArticle",
    .domain = null,
    .range = .iri,
    .transitive = false,
    .symmetric = false,
    .lod_target = null,
};

const ALL_PROPERTIES = [_]*const OntologyProperty{
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
};

/// Look up a property by IRI.
pub fn lookupProperty(iri: []const u8) ?*const OntologyProperty {
    for (ALL_PROPERTIES) |prop| {
        if (std.mem.eql(u8, prop.iri, iri)) return prop;
    }
    return null;
}

// =============================================================================
// Tests — Milestone 2.1
// =============================================================================

const testing = std.testing;

test "class lookup by IRI" {
    const cls = lookupClass(NS_SCHEMA ++ "Person");
    try testing.expect(cls != null);
    try testing.expectEqualStrings("Person", cls.?.label);
}

test "class lookup unknown IRI returns null" {
    try testing.expect(lookupClass("http://unknown.example/") == null);
}

test "property lookup by IRI" {
    const prop = lookupProperty(NS_RDFS ++ "label");
    try testing.expect(prop != null);
    try testing.expectEqual(@as(u3, 4), prop.?.lod_target.?);
}

test "property lookup rdf:type" {
    const prop = lookupProperty(NS_RDF ++ "type");
    try testing.expect(prop != null);
    try testing.expect(prop.?.lod_target == null);
}

test "superclass chain for Person" {
    var chain: [8][]const u8 = undefined;
    const n = superclassChain(NS_SCHEMA ++ "Person", &chain);
    try testing.expect(n >= 2);
    try testing.expectEqualStrings(NS_SCHEMA ++ "Person", chain[0]);
    try testing.expectEqualStrings(NS_YAGO ++ "Entity", chain[1]);
}

test "domain validation on hasGender" {
    const prop = lookupProperty(NS_YAGO ++ "hasGender");
    try testing.expect(prop != null);
    try testing.expectEqualStrings(NS_SCHEMA ++ "Person", prop.?.domain.?);
}

test "transitive property subClassOf" {
    const prop = lookupProperty(NS_RDFS ++ "subClassOf");
    try testing.expect(prop.?.transitive);
}



