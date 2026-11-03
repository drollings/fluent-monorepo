//! Honest `models verify` reporting: every requested entry gets a
//! line, and nothing verifies silently.
//!
//! The cryptographic pass still lives in `orator_models`; this
//! module only classifies rows into report lines, so the policy — a
//! named entry that cannot be verified fails, the all-models run
//! fails only on a present-and-pinned mismatch or error — stays
//! unit-pinned without touching disk.

use orator_models::ModelStatus;

/// `models verify` outcome: report lines plus how the command exits.
pub struct VerifyReport {
    /// One line per examined entry, in catalog order.
    pub lines: Vec<String>,
    /// Whether the command fails.
    pub failed: bool,
    /// Whether the requested name matches no catalog entry.
    pub unknown: bool,
}

/// Classify every catalog `row` for `models verify`. `name` selects
/// one model (empty verifies all); `check` runs the cryptographic
/// pass for a pinned-and-present row and names its failure.
#[must_use]
pub fn verify_report(
    rows: &[ModelStatus],
    name: &str,
    mut check: impl FnMut(&str) -> Result<bool, String>,
) -> VerifyReport {
    if !name.is_empty() && !rows.iter().any(|row| row.name == name) {
        return VerifyReport {
            lines: Vec::new(),
            failed: true,
            unknown: true,
        };
    }
    let mut report = VerifyReport {
        lines: Vec::new(),
        failed: false,
        unknown: false,
    };
    for row in rows.iter().filter(|row| name.is_empty() || row.name == name) {
        let named = !name.is_empty();
        let (line, failed) = if !row.present {
            (format!("{}: missing", row.name), named)
        } else if !row.pinned {
            (
                format!("{}: unverified (unpinned)", row.name),
                named,
            )
        } else {
            match check(&row.name) {
                Ok(true) => (format!("{}: ok", row.name), false),
                Ok(false) => (format!("{}: MISMATCH", row.name), true),
                Err(detail) => (format!("{}: {detail}", row.name), true),
            }
        };
        report.failed |= failed;
        report.lines.push(line);
    }
    report
}

/// Fetch one pinned single-file entry through the shared artifact
/// plane (Hugging Face, `main` revision): streamed to a `.part` file,
/// size-plus-hash verified, atomically renamed only on match. The
/// cache directory is the destination's parent, so the verified file
/// lands exactly at `dest`. Unpinned entries refuse before any
/// network; audio bundles refuse with a named remedy (four-component
/// fetch, not a single file). Present-and-verified cache hits skip
/// the network entirely.
pub async fn pull_single(
    entry: &orator_models::ModelEntry,
    dest: &std::path::Path,
    fetcher: &dyn fluent_llm::ArtifactFetcher,
) -> Result<std::path::PathBuf, String> {
    if entry.kind == orator_models::ModelKind::Audio {
        return Err(format!(
            "orator: bundle pull for {} is not a single-file fetch (install the four audio components into the audio layout)",
            entry.name
        ));
    }
    if !entry.pinned() {
        return Err(format!(
            "model {} is unpinned (no size/hash); cannot pull",
            entry.name
        ));
    }
    let parent = dest.parent().ok_or_else(|| {
        format!(
            "orator: pull destination {} has no parent directory",
            dest.display()
        )
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("orator: pull mkdir {}: {error}", parent.display()))?;
    let local_name = dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            format!(
                "orator: pull destination {} has no file name",
                dest.display()
            )
        })?;
    let (Some(size), Some(sha256)) = (entry.size, entry.sha256.clone()) else {
        return Err(format!(
            "model {} is unpinned (no size/hash); cannot pull",
            entry.name
        ));
    };
    let mut local_paths = std::collections::HashMap::new();
    local_paths.insert(entry.file.clone(), local_name);
    let source = fluent_llm::ModelArtifactSource {
        kind: fluent_llm::SourceKind::HuggingFace,
        repo: entry.repo.clone(),
        revision: "main".to_string(),
        cache_directory: parent.to_path_buf(),
        local_paths,
    };
    let artifacts = vec![fluent_llm::ModelArtifact {
        path: entry.file.clone(),
        size,
        sha256,
    }];
    let options = fluent_llm::ResolveArtifactsOptions {
        model: &entry.name,
        sources: vec![source],
        artifacts,
        on_progress: None,
        on_download_plan: None,
        on_fallback: None,
        fetcher,
        timeouts: fluent_llm::ArtifactTimeouts::default(),
        lock: fluent_llm::LockOptions::default(),
    };
    fluent_llm::resolve_model_artifacts(&options)
        .await
        .map_err(|error| format!("orator: pull {} failed: {error}", entry.name))?;
    if !dest.exists() {
        return Err(format!(
            "orator: pull {} finished without landing at {}",
            entry.name,
            dest.display()
        ));
    }
    Ok(dest.to_path_buf())
}

#[cfg(test)]
#[path = "../tests/models.rs"]
mod tests;
