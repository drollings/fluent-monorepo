//! Load-or-default goldens: missing files, bad JSON, hierarchy order.

use std::path::PathBuf;

use orator_core::OratorConfig;

#[test]
fn missing_file_falls_back_to_default() {
    let missing = PathBuf::from("/nonexistent-orator-dir/config.json");
    assert!(!missing.exists());
    let loaded = OratorConfig::load_or_default(&missing);
    assert_eq!(loaded, OratorConfig::default());
}

#[test]
fn invalid_json_falls_back_to_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.json");
    std::fs::write(&path, "{ not json").expect("write fixture");
    let loaded = OratorConfig::load_or_default(&path);
    assert_eq!(loaded, OratorConfig::default());
}

#[test]
fn unknown_keys_are_ignored_for_forward_compat() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        r#"{"tts": {"backend": "kokoro", "voice": "brass", "future_key": 1}, "future_section": {"x": true}}"#,
    )
    .expect("write fixture");
    let loaded = OratorConfig::load_or_default(&path);
    assert_eq!(loaded.tts.backend, "kokoro");
    assert_eq!(loaded.tts.voice, "brass");
}

/// Leaf sections are strict: a section missing a field falls back to
/// defaults rather than half-loading. Documented here so a future
/// schema change sees the contract it amends.
#[test]
fn partial_leaf_falls_back_to_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"tts": {"backend": "kokoro"}}"#).expect("write fixture");
    let loaded = OratorConfig::load_or_default(&path);
    assert_eq!(loaded, OratorConfig::default());
}

#[test]
fn partial_file_keeps_written_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.json");
    std::fs::write(&path, r#"{"tts": {"backend": "kokoro", "voice": "brass"}}"#)
        .expect("write fixture");
    let loaded: OratorConfig = common_core::config::load_json(&path).unwrap_or_default();
    assert_eq!(loaded.tts.voice, "brass");
    assert_eq!(loaded.audio.backend, "cpal");
}

#[test]
fn hierarchy_prefers_the_project_file() {
    let project = tempfile::tempdir().expect("tempdir");
    let user = tempfile::tempdir().expect("tempdir");
    let project_file = project.path().join("config.json");
    let user_file = user.path().join("config.json");
    std::fs::write(&project_file, r#"{"tts": {"backend": "x", "voice": "p"}}"#)
        .expect("write fixture");
    std::fs::write(&user_file, r#"{"tts": {"backend": "x", "voice": "u"}}"#)
        .expect("write fixture");
    let found = common_core::config::find_hierarchical(&[project_file, user_file])
        .expect("one candidate exists");
    let loaded = OratorConfig::load_or_default(&found);
    assert_eq!(loaded.tts.voice, "p");
}
