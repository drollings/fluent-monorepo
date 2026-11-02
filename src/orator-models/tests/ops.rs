//! Model-operation tests: list shape, verify/remove round-trip, pull gate.

use crate::model::{ModelEntry, ModelKind};
use crate::ops::{
    find_entry, list_models, path_for, pull_plan, remove_entry, verify_bundle, verify_bundle_files,
    verify_entry,
};
use crate::ModelOpError;

#[test]
fn list_covers_all_families() {
    let rows = list_models();
    assert!(rows.len() >= 6);
    assert!(rows.iter().any(|row| row.kind == "whisper"));
    assert!(rows.iter().any(|row| row.kind == "llm"));
    assert!(rows.iter().any(|row| row.kind == "tts"));
    assert!(rows.iter().any(|row| row.kind == "audio"));
}

#[test]
fn vad_family_is_total_but_empty() {
    use crate::model::vad_entries;
    assert!(vad_entries().is_empty(), "v1 pins no VAD weights");
    assert!(find_entry(ModelKind::Vad, "anything").is_none());
    let kinds: std::collections::HashSet<String> =
        list_models().into_iter().map(|row| row.kind).collect();
    for family in ["whisper", "llm", "tts", "audio"] {
        assert!(kinds.contains(family), "list covers {family}");
    }
}

#[test]
fn cross_kind_same_tag_resolves_deterministically() {
    let whisper = find_entry(ModelKind::Whisper, "tiny").expect("whisper tiny");
    assert_eq!(whisper.kind, ModelKind::Whisper);
    assert!(find_entry(ModelKind::Llm, "tiny").is_none());
    assert!(find_entry(ModelKind::Vad, "tiny").is_none());
}

#[test]
fn audio_bundle_lists_with_component_presence() {
    let entry = find_entry(ModelKind::Audio, "liquidai/lfm2.5-audio-1.5b").expect("audio listed");
    assert!(entry.pinned(), "bundle manifest is measured and pinned");
    let rows = list_models();
    assert!(rows.iter().any(|row| row.name == "liquidai/lfm2.5-audio-1.5b" && row.kind == "audio"));
}

#[test]
fn bundle_manifest_rejects_tampered_files() {
    use common_core::hash::{sha256_hex, HashAlgorithm};
    let dir = tempfile::tempdir().expect("tempdir");
    let mut pins = Vec::new();
    let mut paths = Vec::new();
    for (index, name) in ["a.gguf", "b.gguf", "c.gguf", "d.gguf"].into_iter().enumerate() {
        let path = dir.path().join(name);
        let content = format!("component-{index}");
        std::fs::write(&path, &content).expect("fixture");
        let bytes = content.len() as u64;
        let hash = common_core::hash::hash_file(&path, HashAlgorithm::Sha256).expect("hash");
        // Leak to 'static for the pin table shape.
        let leaked: &'static str = Box::leak(hash.into_boxed_str());
        pins.push((name, bytes, leaked));
        paths.push(path);
    }
    let pins: [(&str, u64, &str); 4] = [pins[0], pins[1], pins[2], pins[3]];
    let manifest = sha256_hex(
        pins.iter()
            .map(|(_, _, hash)| *hash)
            .collect::<String>()
            .as_bytes(),
    );
    let files = [paths[0].as_path(), paths[1].as_path(), paths[2].as_path(), paths[3].as_path()];
    assert!(verify_bundle_files(files, &pins, &manifest).expect("valid manifest"));
    std::fs::write(&paths[2], "tampered").expect("tamper");
    assert!(!verify_bundle_files(files, &pins, &manifest).expect("tamper reports"));
    let _ = sha256_hex;
}

#[ignore = "hashes ~1.2GB off disk; run via the soak gate"]
#[test]
fn bundle_manifest_matches_live_bytes() {
    let entry = find_entry(ModelKind::Audio, "liquidai/lfm2.5-audio-1.5b").expect("audio listed");
    match verify_bundle(&entry) {
        Ok(verified) => assert!(verified, "installation bundle must verify"),
        Err(ModelOpError::Missing(_)) => {
            eprintln!("bundle files absent on this machine; presence is reported, not required");
        }
        Err(error) => panic!("unexpected verify failure: {error}"),
    }
}

#[test]
fn unpinned_entries_refuse_pull_and_verify() {
    let whisper = find_entry(ModelKind::Whisper, "small").expect("whisper listed");
    assert!(pull_plan(&whisper).is_err());
    assert!(verify_entry(&whisper).is_err());
}

#[test]
fn pinned_liquidai_entries_plan_into_the_layout() {
    let entry = find_entry(ModelKind::Llm, "liquidai/lfm2.5-2.6b").expect("llm listed");
    assert!(entry.pinned());
    let (_, _, dest) = pull_plan(&entry).expect("pinned plans");
    assert!(dest.ends_with("liquidai/lfm2.5-2.6b-gguf/latest.gguf"));
}

#[test]
fn verify_and_remove_round_trip() {
    // Managed paths resolve under HOME: hold the redirect lock so a
    // concurrent HOME move cannot swap the layout mid-test.
    let _guard = common_core::sync::lock(&crate::ops::ENV_LOCK);
    let entry = ModelEntry {
        name: "test-weights".to_string(),
        kind: ModelKind::Whisper,
        repo: "local/test".to_string(),
        file: "orator-test-roundtrip.bin".to_string(),
        size: Some(5),
        sha256: Some(common_core::hash::sha256_hex(b"hello")),
    };
    let path = path_for(&entry).expect("managed dir resolves");
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, b"hello").expect("fixture");
    assert!(verify_entry(&entry).expect("verifies"));
    std::fs::write(&path, b"world").expect("tamper");
    assert!(!verify_entry(&entry).expect("mismatch reports"));
    assert!(remove_entry(&entry).expect("removes"));
    assert!(!path.exists());
    assert!(!remove_entry(&entry).expect("second remove is noop"));
}
