//! Ordered pipeline driver: the fixed per-file stage order plus the
//! pieces that make waves converge instead of merely repeat.
//!
//! The driver composes existing stages, never reimplements them:
//! fingerprinting lives in `diff`, syntactic parsing in `ast_parser`,
//! hashing in `item_hashes`, formatting in `fix`, lint and compile in
//! `clippy`, the ladder in `waves`, certification in `gates`, merging
//! in `shadow`, and completion in `freshness`. This module owns only
//! three things those stages cannot decide alone: the order files
//! enter waves, the early syntactic index that keeps navigation off
//! the build, and what counts as a witness.
//!
//! Stage order (fixed): fingerprint, index-lite, format, combined
//! lint-plus-check with span attribution, fix ladder to fixpoint,
//! final verify, index-finalize, completion marking. Touch stays last;
//! a crash between index-final and touch leaves the file incomplete,
//! never complete-but-unindexed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One file's early syntactic snapshot: parsed without any build, so
/// navigation stays available while the toolchain works. Marked
/// pending by the caller until the full receipt lands.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One file's early syntactic snapshot: parsed without any build, so
/// navigation stays available while the toolchain works. Marked
/// pending by the caller until the full receipt lands.
pub struct LiteItem {
    /// Source path, as passed in.
    pub path: PathBuf,
    /// Top-level member count from the syntax home.
    pub members: usize,
    /// Content hash of the bytes parsed.
    pub content_hash: String,
}

/// Early index over dirty files: parse plus hash, never compile.
/// Order matches the input; failures count as zero-member rows rather
/// than aborting the batch.
#[must_use]
/// Early index over dirty files: parse plus hash, never compile.
/// Order matches the input; failures count as zero-member rows rather
/// than aborting the batch.
pub fn lite_index(paths: &[PathBuf], parser: &mut crate::ast_parser::AstParser) -> Vec<LiteItem> {
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let members = parser
                .parse_file(path, &text)
                .map_or(0, |doc| doc.members.len());
            LiteItem {
                path: path.clone(),
                members,
                content_hash: common_core::hash::sha256_hex(text.as_bytes()),
            }
        })
        .collect()
}

/// Order files for waves: dependency depth first (leaves before the
/// dependents that observe them), then ascending blast radius (fewer
/// transitive dependents first, so a failure bisects against the
/// smallest scope), then path for determinism. Both axes are task
/// value — smaller rework for the run — never producer confidence
/// about any single patch.
#[must_use]
/// Order files for waves: dependency depth first (leaves before the
/// dependents that observe them), then ascending blast radius (fewer
/// transitive dependents first, so a failure bisects against the
/// smallest scope), then path for determinism. Both axes are task
/// value — smaller rework for the run — never producer confidence
/// about any single patch.
pub fn order_for_waves(
    files: &[PathBuf],
    dependencies_of: &dyn Fn(&Path) -> Vec<PathBuf>,
    dependents_of: &dyn Fn(&Path) -> Vec<PathBuf>,
) -> Vec<PathBuf> {
    fn depth(
        path: &Path,
        dependencies_of: &dyn Fn(&Path) -> Vec<PathBuf>,
        memo: &mut HashMap<PathBuf, usize>,
        visiting: &mut HashSet<PathBuf>,
    ) -> usize {
        if let Some(known) = memo.get(path) {
            return *known;
        }
        if !visiting.insert(path.to_path_buf()) {
            return 0;
        }
        let mut deep = 0;
        for dep in dependencies_of(path) {
            deep = deep.max(depth(&dep, dependencies_of, memo, visiting) + 1);
        }
        visiting.remove(path);
        memo.insert(path.to_path_buf(), deep);
        deep
    }

    fn blast_radius(path: &Path, dependents_of: &dyn Fn(&Path) -> Vec<PathBuf>) -> usize {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut stack: Vec<PathBuf> = dependents_of(path);
        while let Some(next) = stack.pop() {
            if seen.insert(next.clone()) {
                stack.extend(dependents_of(&next));
            }
        }
        seen.len()
    }

    let mut memo = HashMap::new();
    let mut visiting = HashSet::new();
    let mut ranked: Vec<(usize, usize, &PathBuf)> = files
        .iter()
        .map(|path| {
            (
                depth(path, dependencies_of, &mut memo, &mut visiting),
                blast_radius(path, dependents_of),
                path,
            )
        })
        .collect();
    ranked.sort_by(|left, right| {
        (left.0, left.1, left.2)
            .partial_cmp(&(right.0, right.1, right.2))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked
        .into_iter()
        .map(|(_, _, path)| path.clone())
        .collect()
}

/// Whether the final wave counts as a witness: nothing was kept and
/// nothing remains that lacks an owner. A kept edit means the tree
/// moved under the verifier; an unowned remainder means the loop must
/// say why and stop instead of declaring done.
#[must_use]
/// Whether the final wave counts as a witness: nothing was kept and
/// nothing remains that lacks an owner. A kept edit means the tree
/// moved under the verifier; an unowned remainder means the loop must
/// say why and stop instead of declaring done.
pub fn witness_clean(kept_in_final_wave: bool, unowned_remaining: usize, stopped: bool) -> bool {
    !kept_in_final_wave && unowned_remaining == 0 && !stopped
}

#[cfg(test)]
#[path = "../tests/pipeline_order.rs"]
mod pipeline_order_tests;
