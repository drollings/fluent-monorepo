//! Weight-layout manifest for LFM3-Audio: which files a runtime
//! needs, where they live, and what is actually on disk.
//!
//! Two layouts exist upstream: the HuggingFace bundle (safetensors,
//! the only complete weights — text backbone + audio tower + codec)
//! and the GGUF conversion (text backbone only; the `mmproj` entry
//! carries a `clip` arch tag, not the audio tower, so no llama-server
//! build can serve audio from it). Verification reports presence and
//! size honestly; unpinned files are never claimed verified.

/// One expected weight file.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Weight-layout manifest for LFM3-Audio: which files a runtime
/// needs, where they live, and what is actually on disk.
/// 
/// Two layouts exist upstream: the HuggingFace bundle (safetensors,
/// the only complete weights — text backbone + audio tower + codec)
/// and the GGUF conversion (text backbone only; the `mmproj` entry
/// carries a `clip` arch tag, not the audio tower, so no llama-server
/// build can serve audio from it). Verification reports presence and
/// size honestly; unpinned files are never claimed verified.
/// One expected weight file.
pub struct LfmWeights {
    /// File name within its home directory.
    pub file: String,
    /// Expected architecture tag (`None` when the file carries none).
    pub arch: Option<String>,
}

/// The two upstream weight homes plus the GGUF conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
/// The two upstream weight homes plus the GGUF conversion.
pub struct LfmLayout {
    /// HuggingFace text+audio bundle files (`model.safetensors`,
    /// tokenizer, configs, audio tokenizer).
    pub hf_bundle: Vec<LfmWeights>,
    /// GGUF text backbone (`latest.gguf`, arch `lfm3`).
    pub gguf_text: LfmWeights,
    /// GGUF projector entry (arch `clip` — not the audio tower).
    pub gguf_projector: LfmWeights,
}

/// The expected layout, transcribed from the reference repo and the
/// `/app` model tree.
#[must_use]
/// The expected layout, transcribed from the reference repo and the
/// `/app` model tree.
pub fn expected_layout() -> LfmLayout {
    LfmLayout {
        hf_bundle: vec![
            file("model.safetensors", None),
            file("tokenizer.json", None),
            file("config.json", None),
            file("audio_tokenizer/config.json", None),
            file("audio_tokenizer/model.safetensors", None),
        ],
        gguf_text: file("latest.gguf", Some("lfm3".to_string())),
        gguf_projector: file("mmproj.gguf", Some("clip".to_string())),
    }
}

fn file(name: &str, arch: Option<String>) -> LfmWeights {
    LfmWeights {
        file: name.to_string(),
        arch,
    }
}

/// One missing or unreadable weight file.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One missing or unreadable weight file.
pub struct MissingWeights {
    /// Home directory checked.
    pub dir: std::path::PathBuf,
    /// Missing file names.
    pub missing: Vec<String>,
    /// Present files with zero size.
    pub empty: Vec<String>,
}

/// Check one weight home: every expected file present and non-empty.
/// Returns `Ok` with the checked file count, or the missing/empty
/// lists — never a silent pass.
pub fn verify_weights(
    dir: &std::path::Path,
    expected: &[LfmWeights],
) -> Result<usize, MissingWeights> {
    let mut missing = Vec::new();
    let mut empty = Vec::new();
    for want in expected {
        let path = dir.join(&want.file);
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > 0 => {}
            Ok(_) => empty.push(want.file.clone()),
            Err(_) => missing.push(want.file.clone()),
        }
    }
    if missing.is_empty() && empty.is_empty() {
        Ok(expected.len())
    } else {
        Err(MissingWeights {
            dir: dir.to_path_buf(),
            missing,
            empty,
        })
    }
}

#[cfg(test)]
#[path = "../../tests/lfm_manifest.rs"]
mod tests;