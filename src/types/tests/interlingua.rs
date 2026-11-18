use super::*;

fn example_hash(s: &str) -> i64 {
    // A deterministic stand-in for the real content hashes (Murmur64A /
    // BLAKE3 live in spacy-rs and guidance-rdf respectively). The layout
    // under test is hash-agnostic.
    let mut h: u64 = 0x9e37_79b9_7f4a_7c15;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h as i64
}

#[test]
fn new_packs_namespace_and_truncates_local() {
    let local = example_hash("http://schema.org/Person");
    let id = InterlinguaId::new(InterlinguaNamespace::YagoClass, local);
    assert_eq!(id.namespace(), InterlinguaNamespace::YagoClass);
    assert_eq!(id.local_id(), local_id_of(local));
    assert!(id.is_yago());
    assert!(!id.is_spacy_lemma());
}

#[test]
fn local_id_of_truncates_to_48_bits() {
    let full = 0xABCD_EF01_2345_6789u64 as i64;
    let local = local_id_of(full);
    assert_eq!(local, 0x0000_EF01_2345_6789);
    assert_eq!(local & !LOCAL_MASK as i64, 0);
}

#[test]
fn roundtrip_via_u64_and_i64() {
    let id = InterlinguaId::new(InterlinguaNamespace::SpacyLemma, 42);
    assert_eq!(InterlinguaId::from_stored_u64(id.as_u64()), id);
    assert_eq!(InterlinguaId::from_i64(id.as_i64()), Some(id));
    // Negative input never reconstructs.
    assert_eq!(InterlinguaId::from_i64(-1), None);
}

/// The deprecated `from_u64` spelling still resolves while the shim lives.
#[test]
#[allow(deprecated)]
fn deprecated_from_u64_still_resolves() {
    let id = InterlinguaId::new(InterlinguaNamespace::SpacyLemma, 42);
    assert_eq!(InterlinguaId::from_u64(id.as_u64()), id);
}

#[test]
fn from_sql_roundtrip() {
    let id = InterlinguaId::new(InterlinguaNamespace::YagoClass, 0x1234_5678_9abc);
    let back = InterlinguaId::from_sql(id.namespace() as u16, id.sql_local());
    assert_eq!(back, id);
}

#[test]
fn namespace_from_u16_unknown_is_reserved() {
    assert_eq!(
        InterlinguaNamespace::from_u16(0x9999),
        InterlinguaNamespace::Reserved
    );
    assert_eq!(
        InterlinguaNamespace::from_u16(0x0300),
        InterlinguaNamespace::SpacyLemma
    );
}

#[test]
fn serde_roundtrip() {
    let id = InterlinguaId::new(InterlinguaNamespace::YagoEntity, 7);
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(serde_json::from_str::<InterlinguaId>(&json).unwrap(), id);
    // 0x0200 << 48 | 7 == 2^57 + 7
    assert_eq!(json, "144115188075855879");
}

#[test]
fn display_is_namespace_colon_hex() {
    let id = InterlinguaId::new(InterlinguaNamespace::YagoClass, 0xabc);
    assert_eq!(id.to_string(), "YagoClass:000000000abc");
}

#[test]
fn concept_metadata_serde_roundtrip() {
    let meta = ConceptMetadata {
        id: InterlinguaId::new(InterlinguaNamespace::YagoClass, 1),
        canonical_name: "schema:Person".into(),
        namespace: InterlinguaNamespace::YagoClass,
        yago_iri: Some("http://yago-knowledge.org/resource/Person".into()),
        yago_class_iri: Some("http://schema.org/Person".into()),
        label: Some("person".into()),
        node_id: Some(NodeId::from_int(0x1234_5678_9abc_def0)),
        parent_class_id: Some(InterlinguaId::new(InterlinguaNamespace::YagoClass, 2)),
    };
    let json = serde_json::to_string(&meta).unwrap();
    let back: ConceptMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, meta.id);
    assert_eq!(back.canonical_name, meta.canonical_name);
    assert_eq!(back.node_id, meta.node_id);
    assert_eq!(back.yago_iri, meta.yago_iri);
    assert_eq!(back.parent_class_id, meta.parent_class_id);
    // The field is serde-defaulted: an old JSON without it deserializes.
    let old = r#"{"id":281474976710657,"canonical_name":"schema:Person","namespace":"YagoClass","yago_iri":"http://yago-knowledge.org/resource/Person","yago_class_iri":"http://schema.org/Person","label":"person","node_id":1311768467463790320}"#;
    let legacy: ConceptMetadata = serde_json::from_str(old).unwrap();
    assert_eq!(legacy.parent_class_id, None);
}

#[test]
fn node_id_is_stored_never_derived() {
    // The full 64-bit NodeId and the truncated 48-bit local are different
    // values; the metadata stores the former verbatim.
    let full = 0xABCD_EF01_2345_6789u64 as i64;
    let meta = ConceptMetadata {
        id: InterlinguaId::new(InterlinguaNamespace::YagoClass, local_id_of(full)),
        canonical_name: "x".into(),
        namespace: InterlinguaNamespace::YagoClass,
        yago_iri: None,
        yago_class_iri: None,
        label: None,
        node_id: Some(NodeId::from_int(full)),
        parent_class_id: None,
    };
    assert_ne!(meta.node_id.unwrap().as_int(), meta.id.local_id());
}

// ─── Helper characterization tests (C1.M0/M1) ─────────────────────────

#[test]
fn lemma_id_for_str_is_deterministic_and_namespaced() {
    let a = lemma_id_for_str("report");
    let b = lemma_id_for_str("report");
    assert_eq!(a, b, "deterministic across calls");
    assert_eq!(a.namespace(), InterlinguaNamespace::SpacyLemma);
    assert!(a.is_spacy_lemma());
    // Equivalence to the manual construction.
    let manual = InterlinguaId::new(
        InterlinguaNamespace::SpacyLemma,
        local_id_of(hash_utf8("report") as i64),
    );
    assert_eq!(a, manual);
    assert_eq!(a.local_id(), local_id_of(hash_utf8("report") as i64));
}

#[test]
fn yago_class_id_for_iri_is_deterministic_and_namespaced() {
    let iri = "http://schema.org/Person";
    let a = yago_class_id_for_iri(iri);
    assert_eq!(a.namespace(), InterlinguaNamespace::YagoClass);
    assert!(a.is_yago());
    let manual = InterlinguaId::new(InterlinguaNamespace::YagoClass, local_id_of(hash_iri(iri)));
    assert_eq!(a, manual);
}

#[test]
fn yago_entity_id_for_iri_is_deterministic() {
    let iri = "http://yago-knowledge.org/resource/Paris";
    let a = yago_entity_id_for_iri(iri);
    assert_eq!(a.namespace(), InterlinguaNamespace::YagoEntity);
    let manual = InterlinguaId::new(InterlinguaNamespace::YagoEntity, local_id_of(hash_iri(iri)));
    assert_eq!(a, manual);
}

#[test]
fn property_id_for_iri_is_deterministic_and_namespaced() {
    let iri = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
    let a = property_id_for_iri(iri);
    assert_eq!(a.namespace(), InterlinguaNamespace::RdfProperty);
    let manual = InterlinguaId::new(
        InterlinguaNamespace::RdfProperty,
        local_id_of(hash_iri(iri)),
    );
    assert_eq!(a, manual);
}

#[test]
fn id_for_namespace_truncation_and_generic() {
    let hash = i64::MAX;
    let id = id_for_namespace(hash, InterlinguaNamespace::UserDefined);
    assert_eq!(id.namespace(), InterlinguaNamespace::UserDefined);
    assert_eq!(id.local_id(), local_id_of(hash));
    assert_eq!(id.local_id() & !LOCAL_MASK as i64, 0);
    // Must match the per-kind helper's truncation behaviour.
    assert_eq!(
        id_for_namespace(hash_utf8("café") as i64, InterlinguaNamespace::SpacyLemma),
        lemma_id_for_str("café")
    );
}

#[test]
fn helpers_edge_inputs_empty_unicode_large() {
    // Empty string and single-char.
    let empty_a = lemma_id_for_str("");
    let empty_b = InterlinguaId::new(
        InterlinguaNamespace::SpacyLemma,
        local_id_of(hash_utf8("") as i64),
    );
    assert_eq!(empty_a, empty_b);
    let one = lemma_id_for_str("X");
    assert_eq!(one.namespace(), InterlinguaNamespace::SpacyLemma);

    // Unicode: café and Beijing.
    for s in ["café", "北京"] {
        let id = lemma_id_for_str(s);
        assert_eq!(id.namespace(), InterlinguaNamespace::SpacyLemma);
        assert_eq!(id, lemma_id_for_str(s), "deterministic for {s:?}");
    }

    // 64 KiB string: deterministic and truncated.
    let large = "a".repeat(64 * 1024);
    let large_id = lemma_id_for_str(&large);
    assert_eq!(large_id.namespace(), InterlinguaNamespace::SpacyLemma);
    assert_eq!(large_id, lemma_id_for_str(&large));

    // YAGO IRI edge: empty IRI and unicode IRI produce valid ids.
    let y_empty = yago_class_id_for_iri("");
    assert_eq!(y_empty.namespace(), InterlinguaNamespace::YagoClass);
    let prop_empty = property_id_for_iri("");
    assert_eq!(prop_empty.namespace(), InterlinguaNamespace::RdfProperty);
}

#[test]
fn helpers_parity_with_murmur_golden_vectors() {
    // Golden hash values from spacy-rs/src/hash.rs `hash_parity_with_spacy_murmur64a`.
    let a = lemma_id_for_str("hello");
    let expected = InterlinguaId::new(
        InterlinguaNamespace::SpacyLemma,
        local_id_of(5983625672228268878u64 as i64),
    );
    assert_eq!(a, expected, "lemma_id for 'hello' must match murmur golden");

    let b = lemma_id_for_str("");
    let expected_empty = InterlinguaId::new(
        InterlinguaNamespace::SpacyLemma,
        local_id_of(14313749767032693980u64 as i64),
    );
    assert_eq!(b, expected_empty);
}

#[test]
fn no_from_raw_usage_at_runtime() {
    // Runtime assertion that the helpers produce non-zero, correctly-namespaced ids
    // and that `from_u64` is only for reconstruction. This test guards the F1
    // invariant: ids are content-derived. We grep for `from_raw` at review time;
    // here we at least ensure helpers never produce a Reserved namespace.
    for iri in [
        "http://schema.org/Person",
        "http://yago-knowledge.org/resource/Dog",
    ] {
        let id = yago_class_id_for_iri(iri);
        assert_ne!(id.namespace(), InterlinguaNamespace::Reserved);
    }
    assert_ne!(
        lemma_id_for_str("report").namespace(),
        InterlinguaNamespace::Reserved
    );
}

/// M12.1: `local_id_of` keeps the bottom 48 bits and drops the top 16 —
/// the single truncation discipline every namespace helper shares.
/// Masking twice is the identity (no second truncation site may narrow
/// further), and all-ones / negative inputs truncate identically.
#[test]
fn local_id_of_keeps_bottom_48_bits() {
    assert_eq!(local_id_of(0), 0);
    assert_eq!(
        local_id_of(0xFFFF_FFFF_FFFF_FFFFu64 as i64),
        0x0000_FFFF_FFFF_FFFFu64 as i64
    );
    assert_eq!(local_id_of(-1), 0x0000_FFFF_FFFF_FFFFu64 as i64);
    // Low 48 bits survive verbatim; high 16 never leak into the local id.
    assert_eq!(
        local_id_of(0x1234_5678_9ABC_DEF0u64 as i64) as u64,
        0x5678_9ABC_DEF0
    );
    let once = local_id_of(example_hash("report"));
    assert_eq!(local_id_of(once), once, "masking is idempotent");
}

/// M12.1: namespace placement — top 16 bits carry the namespace, bottom
/// 48 the truncated local id, round-tripping through both accessors.
#[test]
fn namespace_occupies_top_16_bits() {
    let id = InterlinguaId::new(InterlinguaNamespace::SpacyLemma, local_id_of(-1));
    assert_eq!(id.namespace(), InterlinguaNamespace::SpacyLemma);
    assert_eq!(id.local_id() as u64, 0x0000_FFFF_FFFF_FFFF);
    assert_eq!(id.as_u64() >> LOCAL_BITS, 0x0300);
    // A full-width local is truncated by construction, never stored raw.
    let wide = InterlinguaId::new(
        InterlinguaNamespace::YagoEntity,
        0xFFFF_FFFF_FFFF_FFFFu64 as i64,
    );
    assert_eq!(wide.local_id() as u64, 0x0000_FFFF_FFFF_FFFF);
    assert_eq!(wide.namespace(), InterlinguaNamespace::YagoEntity);
}
