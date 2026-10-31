//! Stale-name guard for the `guidance-ontology` doc stubs. See the
//! `common-core` `doc_stubs` suite for the contract.

#[allow(unused_imports)]
use guidance_ontology::normalize::{iri_to_curie, normalize_lexicon_name};
#[allow(unused_imports)]
use guidance_ontology::prefix::{split_prefix, CuriePrefix};
#[allow(unused_imports)]
use guidance_ontology::taxonomy::{normalize_taxonomy_curie, TaxonomyEntry};
#[allow(unused_imports)]
use guidance_ontology::yago_loader::{RegistryEntry, YaGoLoader};
#[allow(unused_imports)]
use guidance_ontology::yago_view::YagoView;

const VISION: &str = include_str!("../../../doc/ontology/VISION.md");
const ARCHITECTURE: &str = include_str!("../../../doc/ontology/ARCHITECTURE.md");

#[test]
fn stubs_name_only_real_public_items() {
    for name in [
        "YaGoLoader",
        "TaxonomyEntry",
        "RegistryEntry",
        "CuriePrefix",
        "YagoView",
        "normalize_taxonomy_curie",
        "normalize_lexicon_name",
        "iri_to_curie",
    ] {
        assert!(
            VISION.contains(name) || ARCHITECTURE.contains(name),
            "doc stub must keep naming `{name}`"
        );
    }
    let _ = CuriePrefix::Yago.as_str();
}
