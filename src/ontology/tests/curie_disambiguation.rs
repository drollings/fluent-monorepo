use crate::taxonomy::normalize_taxonomy_curie;

#[test]
fn taxonomy_name_preserves_shim_divergence_case() {
    assert_eq!(
        normalize_taxonomy_curie("yago:Remix__U0028_Work_U0029__Q113171270"),
        "yago:remix (work)"
    );
    assert_eq!(normalize_taxonomy_curie("YAGO:City"), "yago:city");
}

#[test]
fn lexicon_name_maps_bare_lemma_without_prefix_logic() {
    assert_eq!(
        crate::normalize::normalize_lexicon_name("Hello_World"),
        "hello world"
    );
    assert_eq!(
        crate::normalize::normalize_lexicon_name("yago:Hello_World"),
        "hello world"
    );
}
