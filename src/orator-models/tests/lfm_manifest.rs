//! `manifest` checks: layout shape plus presence reporting —
//! hermetic temp dirs, never the real model tree.

use crate::lfm::manifest::{expected_layout, verify_weights};

#[test]
fn layout_names_both_weight_homes() {
    let layout = expected_layout();
    assert_eq!(layout.hf_bundle.len(), 5, "bundle files listed");
    assert!(
        layout
            .hf_bundle
            .iter()
            .any(|entry| entry.file == "model.safetensors"),
        "text+audio backbone listed"
    );
    assert_eq!(layout.gguf_text.arch.as_deref(), Some("lfm3"));
    assert_eq!(
        layout.gguf_projector.arch.as_deref(),
        Some("clip"),
        "the projector entry is clip-arch, not the audio tower"
    );
}

#[test]
fn verify_reports_missing_instead_of_passing() {
    let home = tempfile::tempdir().expect("tempdir");
    let layout = expected_layout();
    let error = verify_weights(home.path(), &layout.hf_bundle).expect_err("empty dir fails");
    assert_eq!(error.missing.len(), 5, "every bundle file missing");
    assert!(error.empty.is_empty());
    assert_eq!(error.dir, home.path());
}

#[test]
fn verify_accepts_present_nonempty_files() {
    let home = tempfile::tempdir().expect("tempdir");
    let layout = expected_layout();
    for entry in &layout.hf_bundle {
        let path = home.path().join(&entry.file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, b"weights").expect("seed writes");
    }
    assert_eq!(
        verify_weights(home.path(), &layout.hf_bundle),
        Ok(5),
        "present files pass with their count"
    );
    std::fs::write(home.path().join("model.safetensors"), b"").expect("truncate");
    let error = verify_weights(home.path(), &layout.hf_bundle).expect_err("empty fails");
    assert_eq!(error.empty, vec!["model.safetensors".to_string()]);
}
