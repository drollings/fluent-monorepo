//! Doc→code jump records: per-chunk crate attribution plus the shared
//! funnel vocabulary, derived from the doc tree with the same chunker
//! and attribution the capability ingest uses. Included via `#[path]`
//! forwarder from `src/jumps.rs`, per repo convention.

use super::*;
use std::collections::BTreeSet;

fn architecture_sync_text() -> &'static str {
    "# Overview\n\n## Selection modes\n\n--file <path>: full single-file sync — fix, then member gen, then the fragment-index half (ingest with sig-gated dependent propagation) scoped to the file.\n"
}

fn calibration_text() -> &'static str {
    "# Calibration\n\nAll decision points ship shadow-only: the default model has no calibration entry, so `authorize` fails closed.\n"
}

fn router_text() -> &'static str {
    "# Vision\n\nCoral Router exposes an OpenAI-compatible HTTP API and routes each request through a deterministic pre-filter, then a classifier.\n"
}

#[test]
fn jumps_attribute_chunks_to_owning_crates() {
    let jumps = derive_jumps_for_doc("doc/guidance/ARCHITECTURE.md", architecture_sync_text());
    assert!(!jumps.is_empty());
    assert!(jumps.iter().all(|jump| jump.crate_name == "guidance-core"));
    let terms: BTreeSet<&str> = jumps
        .iter()
        .flat_map(|jump| jump.terms.iter().map(String::as_str))
        .collect();
    assert!(terms.contains("sync"), "terms: {terms:?}");
}

#[test]
fn jumps_share_the_capability_attribution_table() {
    let jumps = derive_jumps_for_doc("doc/guidance/CALIBRATION.md", calibration_text());
    assert!(jumps.iter().all(|jump| jump.crate_name == "guidance-core"));
    let jumps = derive_jumps_for_doc("doc/router/VISION.md", router_text());
    assert!(jumps.iter().all(|jump| jump.crate_name == "fluent-router"));
}

#[test]
fn root_docs_fall_back_to_meta_never_a_guess() {
    let jumps = derive_jumps_for_doc(
        "README.md",
        "# Root\n\nMember projects: Fluent WVR, Fluent Concurrency.\n",
    );
    assert!(!jumps.is_empty());
    assert!(jumps.iter().all(|jump| jump.crate_name == "unknown"));
    assert!(jumps.iter().all(|jump| jump.domain == "meta"));
}

#[test]
fn manifest_transitions_missing_derived_stale() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().join("ws");
    let guidance = dir.path().join(".guidance");
    std::fs::create_dir_all(workspace.join("doc").join("guidance")).expect("mkdir");
    std::fs::write(
        workspace.join("doc").join("guidance").join("NOTE.md"),
        "# Note\n\nSync decides reprocessing.\n",
    )
    .expect("write");
    assert!(manifest_stale(&workspace, &guidance));
    let first = sync_jumps(&workspace, &guidance).expect("sync");
    assert!(first.rederived);
    assert!(!manifest_stale(&workspace, &guidance));
    let second = sync_jumps(&workspace, &guidance).expect("resync");
    assert!(!second.rederived, "converged tree rederives nothing");
    assert_eq!(first.chunks, second.chunks);
    std::fs::write(
        workspace.join("doc").join("guidance").join("NOTE.md"),
        "# Note\n\nSync decides reprocessing and calibration.\n",
    )
    .expect("rewrite");
    assert!(manifest_stale(&workspace, &guidance));
}

#[test]
fn stale_manifest_loads_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = dir.path().join("ws");
    let guidance = dir.path().join(".guidance");
    std::fs::create_dir_all(workspace.join("doc")).expect("mkdir");
    std::fs::write(workspace.join("doc").join("a.md"), "# A\n\nAlpha beta gamma.\n")
        .expect("write");
    sync_jumps(&workspace, &guidance).expect("sync");
    std::fs::write(
        workspace.join("doc").join("a.md"),
        "# A\n\nCompletely different words here.\n",
    )
    .expect("rewrite");
    assert!(load_jump_index(&workspace, &guidance).is_none());
}
