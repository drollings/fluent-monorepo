//! Shadow verification for model-drafted patches: each patch verifies
//! alone, accepted patches merge per package and verify once, and a
//! merged failure bisects to the culprit while the rest requeue.
//!
//! The shadow tree never touches the working copy: all checks run
//! against a scratch dir through a caller-supplied predicate, so unit
//! tests pin the logic without a toolchain and the wave scheduler
//! supplies the cargo-backed predicate in production.

use std::path::Path;

/// One drafted patch as full texts (never a diff: replay and revert
/// stay total without a patch language).
#[derive(Debug, Clone)]
/// One drafted patch as full texts (never a diff: replay and revert
/// stay total without a patch language).
pub struct ShadowPatch {
    /// Package-relative file path.
    pub file: String,
    /// Expected live bytes before the patch.
    pub before: String,
    /// Bytes after the patch.
    pub after: String,
}

/// Lay patches over a shadow tree rooted at `dir`, creating parents.
fn stage_patches(dir: &Path, patches: &[ShadowPatch]) {
    for patch in patches {
        let target = dir.join(&patch.file);
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&target, &patch.after);
    }
}

/// Verify one merged patch set: reset the shadow tree to exactly these
/// patches, run the check once, and report the single verdict. The
/// reset keeps successive bisection probes independent.
#[must_use]
/// Verify one merged patch set: reset the shadow tree to exactly these
/// patches, run the check once, and report the single verdict. The
/// reset keeps successive bisection probes independent.
pub fn verify_merged(dir: &Path, patches: &[ShadowPatch], check: &dyn Fn(&Path) -> bool) -> bool {
    if patches.is_empty() {
        return true;
    }
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::create_dir_all(dir);
    stage_patches(dir, patches);
    check(dir)
}

/// Bisection outcome for a merged patch set.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Bisection outcome for a merged patch set.
pub enum Bisect {
    /// The merged set verifies clean; nothing to do.
    Clean,
    /// One patch fails alone; the rest requeue.
    Culprit(String),
    /// Halves pass alone but the merge fails: the failure is an
    /// interaction, so the whole set escalates instead of blaming one.
    Interaction,
}

/// Bisect a failing merged set to its culprit: halve until one patch
/// remains that fails alone. Exactly-one-culprit sets always resolve.
#[must_use]
/// Bisect a failing merged set to its culprit: halve until one patch
/// remains that fails alone. Exactly-one-culprit sets always resolve.
pub fn bisect_culprit(
    dir: &Path,
    patches: &[ShadowPatch],
    check: &dyn Fn(&Path) -> bool,
) -> Bisect {
    if patches.is_empty() || verify_merged(dir, patches, check) {
        return Bisect::Clean;
    }
    let mut suspects: Vec<ShadowPatch> = patches.to_vec();
    while suspects.len() > 1 {
        let mid = suspects.len() / 2;
        let (left, right) = suspects.split_at(mid);
        if !verify_merged(dir, left, check) {
            suspects = left.to_vec();
        } else if !verify_merged(dir, right, check) {
            suspects = right.to_vec();
        } else {
            return Bisect::Interaction;
        }
    }
    suspects
        .first()
        .map_or(Bisect::Clean, |patch| Bisect::Culprit(patch.file.clone()))
}

#[cfg(test)]
#[path = "../tests/shadow_verify.rs"]
mod shadow_verify_tests;
