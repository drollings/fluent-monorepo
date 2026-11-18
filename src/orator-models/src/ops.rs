//! Model file operations: list, verify, remove, and the download plan.
//!
//! Downloads ride the shared artifact plane (pinned, hashed, atomic
//! rename); this module maps one catalog entry to a one-artifact
//! snapshot and inherits the plane's guarantees. Unpinned entries
//! document intent and never download.

use std::path::PathBuf;

use thiserror::Error;

/// File-operation failures. Every variant names the next action.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ModelOpError {
    /// No catalog entry matches.
    #[error("unknown model: {0}")]
    Unknown(String),
    /// The entry is unpinned: nothing to fetch or verify against.
    #[error("model {0} is unpinned (no size/hash); cannot {1}")]
    Unpinned(String, String),
    /// The file is missing.
    #[error("model file missing at {0}")]
    Missing(String),
    /// Size or hash mismatch.
    #[error("model {0} failed verification: {1}")]
    Mismatch(String, String),
    /// Refusing to remove a shared installation file.
    #[error("refusing to remove installation file at {0}")]
    Installation(String),
    /// Filesystem failure.
    #[error("model file error: {0}")]
    Io(String),
}

/// One row of `models list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelStatus {
    /// Catalog tag.
    pub name: String,
    /// Family label.
    pub kind: String,
    /// Whether size + hash are pinned.
    pub pinned: bool,
    /// Whether the file exists locally.
    pub present: bool,
    /// Size agreement for listings (fast stat); `models verify` does
    /// the cryptographic pass. None when absent/unpinned.
    pub verified: Option<bool>,
}

/// Local file for `entry` in the managed directory. Installation
/// entries (LLM, audio) resolve through [`resolve_entry_path`].
/// `None` when no home base resolves.
#[must_use]
pub fn path_for(entry: &crate::model::ModelEntry) -> Option<PathBuf> {
    orator::models_dir().map(|dir| dir.join(&entry.file))
}

/// Resolve where `entry` lives: the installation layout first for
/// LLM weights (router rule) and audio bundles (component files),
/// the managed directory otherwise. `None` when nothing exists.
#[must_use]
pub fn resolve_entry_path(entry: &crate::model::ModelEntry) -> Option<PathBuf> {
    match entry.kind {
        crate::model::ModelKind::Llm => {
            let mut roots = vec![PathBuf::from(orator::DEFAULT_GGUF_DIR)];
            roots.extend(orator::llama_weights_dirs());
            roots.extend(orator::models_dir());
            roots
                .into_iter()
                .map(|root| root.join(&entry.file))
                .find(|path| path.exists())
        }
        crate::model::ModelKind::Audio => bundle_dir(),
        _ => path_for(entry).filter(|path| path.exists()),
    }
}

/// Installation directory holding the audio bundle, when complete.
#[must_use]
pub fn bundle_dir() -> Option<PathBuf> {
    let bundle = orator::default_bundle();
    bundle.present().then(|| {
        bundle
            .mmproj
            .parent()
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
    })
}

/// Verify the bundle manifest: every component matches its pinned
/// size + hash, and the manifest over the digests matches the entry.
pub fn verify_bundle(entry: &crate::model::ModelEntry) -> Result<bool, ModelOpError> {
    let bundle = orator::default_bundle();
    verify_bundle_files(
        &[bundle.backbone.clone(), bundle.mmproj.clone()],
        &crate::model::AUDIO_COMPONENT_PINS,
        entry.sha256.as_deref().unwrap_or_default(),
    )
}

/// Manifest check over explicit files and pins (test seam: temp files
/// stay fast while the live-bytes case covers the installation).
pub fn verify_bundle_files(
    files: &[std::path::PathBuf],
    pins: &[(&str, u64, &str)],
    manifest: &str,
) -> Result<bool, ModelOpError> {
    if files.len() != pins.len() {
        return Err(ModelOpError::Mismatch(
            "bundle".to_string(),
            format!(
                "file/pin count mismatch ({} files, {} pins)",
                files.len(),
                pins.len()
            ),
        ));
    }
    let mut digests = String::new();
    for (file, (_, size, hash)) in files.iter().zip(pins) {
        if !file.exists() {
            return Err(ModelOpError::Missing(file.display().to_string()));
        }
        let bytes = std::fs::metadata(file)
            .map_err(|error| ModelOpError::Io(error.to_string()))?
            .len();
        if bytes != *size {
            return Ok(false);
        }
        let digest = common_core::hash::hash_file(file, common_core::hash::HashAlgorithm::Sha256)
            .map_err(|error| ModelOpError::Io(error.to_string()))?;
        if digest != *hash {
            return Ok(false);
        }
        digests.push_str(&digest);
    }
    Ok(common_core::hash::sha256_hex(digests.as_bytes()) == manifest)
}

/// List every catalog entry with presence and verification state.
#[must_use]
pub fn list_models() -> Vec<ModelStatus> {
    let mut out = Vec::new();
    for entry in crate::model::whisper_entries()
        .into_iter()
        .chain(crate::model::llm_entries())
        .chain(crate::model::tts_entries())
        .chain(crate::model::audio_entries())
        .chain(crate::model::vad_entries())
    {
        // Language-model weights live in the llama.cpp installation
        // layout first; managed artifacts use the model directory.
        // Audio bundles resolve through their component files.
        let present = if entry.kind == crate::model::ModelKind::Llm {
            resolve_entry_path(&entry).is_some()
        } else if entry.kind == crate::model::ModelKind::Audio {
            orator_s2s_bundle_present()
        } else {
            path_for(&entry).is_some_and(|path| path.exists())
        };
        let verified = if present && entry.pinned() {
            // Fast agreement (sizes only): `models verify` does the
            // cryptographic pass. Listing must stay instant even for
            // gigabyte weights.
            size_matches(&entry)
        } else {
            None
        };
        out.push(ModelStatus {
            name: entry.name.clone(),
            kind: format!("{:?}", entry.kind).to_lowercase(),
            pinned: entry.pinned(),
            present,
            verified,
        });
    }
    out
}

/// Fast size agreement for listings (stat only, no hashing). The
/// cryptographic pass lives in [`verify_entry`].
fn size_matches(entry: &crate::model::ModelEntry) -> Option<bool> {
    let want = entry.size.unwrap_or(0);
    if entry.kind == crate::model::ModelKind::Audio {
        let bundle = orator::default_bundle();
        let files = [&bundle.backbone, &bundle.mmproj];
        let mut total = 0u64;
        for (file, (_, size, _)) in files.into_iter().zip(crate::model::AUDIO_COMPONENT_PINS) {
            let bytes = std::fs::metadata(file).ok()?.len();
            if bytes != size {
                return Some(false);
            }
            total = total.saturating_add(bytes);
        }
        return Some(total == want);
    }
    let path = resolve_entry_path(entry)?;
    let bytes = std::fs::metadata(&path).ok()?.len();
    Some(bytes == want)
}

/// Verify the local file for `entry` against size + hash.
///
/// The size-then-digest mechanics compose
/// `common_core::hash::verify_sha256_file`; this wrapper keeps the catalog
/// policy local (missing-file and unpinned variants, audio bundles resolve
/// separately). `lfm::manifest::verify_weights` remains a presence/non-empty
/// check, a different contract.
pub fn verify_entry(entry: &crate::model::ModelEntry) -> Result<bool, ModelOpError> {
    if !entry.pinned() {
        return Err(ModelOpError::Unpinned(
            entry.name.clone(),
            "verify".to_string(),
        ));
    }
    if entry.kind == crate::model::ModelKind::Audio {
        return verify_bundle(entry);
    }
    let Some(path) = resolve_entry_path(entry) else {
        return Err(ModelOpError::Missing(entry.file.clone()));
    };
    let want = entry.size.unwrap_or(0);
    match common_core::hash::verify_sha256_file(
        &path,
        std::num::NonZeroU64::new(want),
        entry.sha256.as_deref().unwrap_or_default(),
    )
    .map_err(|error| ModelOpError::Io(error.to_string()))?
    {
        common_core::hash::FileVerification::Match => Ok(true),
        common_core::hash::FileVerification::SizeMismatch { .. }
        | common_core::hash::FileVerification::HashMismatch { .. } => Ok(false),
    }
}

/// Remove the local file for `entry`. Missing files are a no-op
/// success. Shared installation files are refused: orator removes
/// only what it downloaded into the managed directory.
pub fn remove_entry(entry: &crate::model::ModelEntry) -> Result<bool, ModelOpError> {
    let managed = path_for(entry);
    if managed.as_ref().is_some_and(|path| path.exists()) {
        let path = managed.expect("managed file exists");
        std::fs::remove_file(&path).map_err(|error| ModelOpError::Io(error.to_string()))?;
        return Ok(true);
    }
    let resolved = resolve_entry_path(entry);
    if resolved.is_some_and(|resolved| Some(&resolved) != managed.as_ref()) {
        return Err(ModelOpError::Installation(
            resolve_entry_path(entry)
                .expect("resolved")
                .display()
                .to_string(),
        ));
    }
    Ok(false)
}

/// Whether the reference audio bundle is fully present.
fn orator_s2s_bundle_present() -> bool {
    orator::default_bundle().present()
}

/// Find an entry by family tag and name across the catalog.
#[must_use]
pub fn find_entry(kind: crate::model::ModelKind, name: &str) -> Option<crate::model::ModelEntry> {
    crate::model::ModelManifest {
        entries: crate::model::whisper_entries()
            .into_iter()
            .chain(crate::model::llm_entries())
            .chain(crate::model::tts_entries())
            .chain(crate::model::audio_entries())
            .chain(crate::model::vad_entries())
            .collect(),
    }
    .find(kind, name)
    .cloned()
}

/// Download plan for a pinned entry: remote coordinates plus the local
/// destination (the installation layout for LLM weights and audio
/// bundles, the managed directory otherwise). The caller hands this
/// to the artifact plane; unpinned entries refuse before any network
/// happens.
pub fn pull_plan(
    entry: &crate::model::ModelEntry,
) -> Result<(String, String, PathBuf), ModelOpError> {
    if !entry.pinned() {
        return Err(ModelOpError::Unpinned(
            entry.name.clone(),
            "pull".to_string(),
        ));
    }
    let dest = match entry.kind {
        crate::model::ModelKind::Llm | crate::model::ModelKind::Audio => resolve_entry_path(entry)
            .unwrap_or_else(|| PathBuf::from(orator::DEFAULT_GGUF_DIR).join(&entry.file)),
        _ => path_for(entry).ok_or_else(|| {
            ModelOpError::Io(
                "no managed model directory (HOME and XDG_DATA_HOME are unset)".to_string(),
            )
        })?,
    };
    Ok((entry.repo.clone(), entry.file.clone(), dest))
}

#[cfg(test)]
#[path = "../tests/ops.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/calibration.rs"]
mod calibration_tests;

/// Serializes HOME redirection across hermetic tests. The managed
/// layout resolves under HOME, so tests that move it hold this
/// while the redirect is visible.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
