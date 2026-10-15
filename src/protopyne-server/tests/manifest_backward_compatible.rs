//! The backward-compatibility gate: a manifest with no tape data serializes
//! byte-identically to the frozen pre-tape form.
//!
//! Every tape field is `Option` plus `skip_serializing_if`, so a run with
//! no tape emits exactly the keys the old struct emitted, in the same
//! order. The frozen file is the record; this test is the gate. A run that
//! names tape data serializes its fields instead — pinned by the rerun
//! suite, not here.

use super::*;

fn untaped_manifest() -> RunManifest {
    RunManifest {
        data_version: "spy-15m-v1".to_owned(),
        code_version: "fixture".to_owned(),
        config_json: "{}".to_owned(),
        bar_count: 384,
        first_ts: "2026-01-02T09:00:00+00:00".to_owned(),
        last_ts: "2026-01-10T00:45:00+00:00".to_owned(),
        seed: 7,
        data_digest: None,
        data_first_ts: None,
        data_last_ts: None,
        data_bar_count: None,
        data_provenance: None,
    }
}

#[test]
fn manifest_without_tape_matches_the_frozen_form() {
    let frozen = include_str!("data/manifest_no_tape.json");
    assert_eq!(
        untaped_manifest().to_canonical_json(),
        frozen.trim_end(),
        "a run with no tape must serialize exactly as before: no new keys, same order"
    );
}

#[test]
fn naming_tape_data_changes_the_bytes() {
    let plain = untaped_manifest().to_canonical_json();
    let taped = untaped_manifest()
        .with_tape_data(&[], &[])
        .to_canonical_json();
    assert_ne!(
        plain, taped,
        "pinning tape data must be visible in the bytes, never silent"
    );
    assert!(
        taped.contains("\"data_digest\""),
        "the digest rides as a manifest key: {taped}"
    );
}
