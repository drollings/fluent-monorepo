//! Runtime classification-tree regeneration + rebootless reload (M14b).
//!
//! The tree config loads once at boot and is statically assembled (VISION
//! §"The Classification Tree": the self-updating loop is the forward
//! target). This module is the first half of that loop: **descriptions and
//! thresholds** regenerate at runtime and swap in without a restart, while
//! **topology edits are excluded** — child membership, keys, routes, groups,
//! models, patterns, and redirect targets are structural, and any reload
//! that changes them is refused.
//!
//! [`VersionedTree`] holds the live tree behind an `RwLock`: readers take
//! whole-version snapshots (never torn), `reload` swaps a regenerated tree
//! in as a new monotonic version (audit-logged), and `rollback` restores the
//! previous generation exactly once (one-reload depth).

use std::sync::RwLock;

use crate::config::classification::{
    default_coherence_threshold, default_safety_threshold, ClassificationNode,
    ClassificationTree,
};

/// Errors from regeneration and reload.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ReloadError {
    /// The node path resolves to no node.
    #[error("unknown tree path: {path}")]
    NotFound { path: String },
    /// Thresholds only exist on classifier nodes.
    #[error("thresholds need a classifier node, found {found} at {path}")]
    WrongNodeType { path: String, found: &'static str },
    /// Thresholds live in `[0, 1]`.
    #[error("threshold {value} out of range [0, 1] at {path}")]
    ThresholdOutOfRange { path: String, value: f64 },
    /// The reloaded tree is identical to the live one — no version minted.
    #[error("reloaded tree is identical to the live version")]
    NoChange,
    /// The reloaded tree changes topology (structure), not just tuning.
    #[error("reloaded tree changes topology: {detail}")]
    TopologyMismatch { detail: String },
    /// No previous generation to roll back to (fresh holder, or rollback
    /// already consumed the one-reload depth).
    #[error("no previous tree generation to roll back to")]
    NoHistory,
}

/// A live tree generation: monotonic version + content.
#[derive(Debug, Clone)]
struct TreeGeneration {
    version: u64,
    tree: ClassificationTree,
}

/// The versioned live holder. `Send + Sync`; snapshots clone under a read
/// lock so reloads never tear a reader's view.
#[derive(Debug)]
pub struct VersionedTree {
    current: RwLock<TreeGeneration>,
    previous: RwLock<Option<TreeGeneration>>,
}

impl VersionedTree {
    /// Publish the boot-loaded tree as version 1.
    pub fn new(tree: ClassificationTree) -> Self {
        Self {
            current: RwLock::new(TreeGeneration { version: 1, tree }),
            previous: RwLock::new(None),
        }
    }

    /// The live version number.
    pub fn version(&self) -> u64 {
        common_core::sync::lock_read(&self.current).version
    }

    /// A whole-version snapshot: `(version, tree)`. Rebootless readers call
    /// this per request/turn and rebuild derived views (prompts, engines)
    /// from the snapshot — no restart, no torn reads.
    pub fn current(&self) -> (u64, ClassificationTree) {
        let guard = common_core::sync::lock_read(&self.current);
        (guard.version, guard.tree.clone())
    }

    /// Swap in a regenerated tree as the next version. Refuses identical
    /// content ([`ReloadError::NoChange`]) and topology changes
    /// ([`ReloadError::TopologyMismatch`]); every accepted reload is
    /// audit-logged with version, fingerprint, and reason. Returns the new
    /// version.
    pub fn reload(&self, tree: ClassificationTree, reason: &str) -> Result<u64, ReloadError> {
        let (live_version, live_tree) = self.current();
        if tree == live_tree {
            return Err(ReloadError::NoChange);
        }
        if topology_fingerprint(&tree) != topology_fingerprint(&live_tree) {
            return Err(ReloadError::TopologyMismatch {
                detail: "child membership, keys, routes, groups, models, patterns, or redirect targets differ".into(),
            });
        }
        let version = live_version + 1;
        *common_core::sync::lock_write(&self.previous) = Some(TreeGeneration {
            version: live_version,
            tree: live_tree,
        });
        *common_core::sync::lock_write(&self.current) = TreeGeneration { version, tree };
        crate::audit::emit(
            "tree_reload",
            serde_json::json!({
                "version": version,
                "previous_version": live_version,
                "reason": reason,
            }),
        );
        Ok(version)
    }

    /// Restore the previous generation as a new monotonic version
    /// (audit-logged). One-reload depth: consumes the saved generation, so a
    /// second rollback without an intervening reload fails with `NoHistory`.
    pub fn rollback(&self) -> Result<u64, ReloadError> {
        let prev = common_core::sync::lock_write(&self.previous).take();
        let Some(prev) = prev else {
            return Err(ReloadError::NoHistory);
        };
        let (live_version, live_tree) = self.current();
        let version = live_version + 1;
        *common_core::sync::lock_write(&self.current) = TreeGeneration {
            version,
            tree: prev.tree,
        };
        let _ = live_tree;
        crate::audit::emit(
            "tree_rollback",
            serde_json::json!({
                "version": version,
                "restored_version": prev.version,
            }),
        );
        Ok(version)
    }
}

/// Regenerate one node's description, returning the updated tree. `path` is
/// the child-key chain from the root (`[]` targets the root itself).
/// Descriptions on any node type (classifier, terminal, filter, fallback)
/// are tuning, never topology.
pub fn regenerate_description(
    tree: &ClassificationTree,
    path: &[&str],
    description: String,
) -> Result<ClassificationTree, ReloadError> {
    let mut next = tree.clone();
    let node = resolve_mut(&mut next.root, path)?;
    match node {
        ClassificationNode::Classifier { description: d, .. }
        | ClassificationNode::Terminal { description: d, .. }
        | ClassificationNode::Filter { description: d, .. } => *d = description,
        ClassificationNode::Fallback { description: d, .. } => *d = Some(description),
    }
    Ok(next)
}

/// Regenerate a classifier node's thresholds, returning the updated tree.
/// `None` leaves that threshold untouched (all-`None` is a no-op clone).
/// Thresholds outside `[0, 1]` and non-classifier targets fail; boundary
/// values are legal.
pub fn regenerate_thresholds(
    tree: &ClassificationTree,
    path: &[&str],
    coherence: Option<f64>,
    safety: Option<f64>,
) -> Result<ClassificationTree, ReloadError> {
    for value in coherence.into_iter().chain(safety) {
        if !(0.0..=1.0).contains(&value) {
            return Err(ReloadError::ThresholdOutOfRange {
                path: format_path(path),
                value,
            });
        }
    }
    let mut next = tree.clone();
    let node = resolve_mut(&mut next.root, path)?;
    match node {
        ClassificationNode::Classifier {
            coherence_threshold,
            safety_threshold,
            ..
        } => {
            if let Some(c) = coherence {
                *coherence_threshold = Some(c);
            }
            if let Some(s) = safety {
                *safety_threshold = Some(s);
            }
        }
        other => {
            return Err(ReloadError::WrongNodeType {
                path: format_path(path),
                found: node_kind(other),
            });
        }
    }
    Ok(next)
}

/// Preview the auto-built prompt for the classifier at `path`, with that
/// node's thresholds over the hard defaults (the same node-over-hard rule
/// the root prompt derivation uses, without pipeline overrides). `None` for
/// non-classifier targets — a prompt preview only exists where prompts do.
pub fn prompt_for_path(tree: &ClassificationTree, path: &[&str]) -> Option<String> {
    let node = resolve(&tree.root, path).ok()?;
    match node {
        ClassificationNode::Classifier {
            coherence_threshold,
            safety_threshold,
            ..
        } => node.build_prompt(
            coherence_threshold.unwrap_or_else(default_coherence_threshold),
            safety_threshold.unwrap_or_else(default_safety_threshold),
        ),
        _ => None,
    }
}

/// Structural fingerprint of the tree: everything EXCEPT descriptions and
/// thresholds (the tunable surface). Two trees share a fingerprint iff a
/// reload between them is tuning-only. Hashes node kinds, child keys and
/// order, routes, groups, roles, `always_route`, models, model groups,
/// patterns, outcomes, redirect targets, and interlingua matches.
pub fn topology_fingerprint(tree: &ClassificationTree) -> u64 {
    use std::hash::{Hash, Hasher};
    fn feed<H: Hasher>(node: &ClassificationNode, state: &mut H) {
        match node {
            ClassificationNode::Classifier {
                model,
                model_group,
                children,
                ..
            } => {
                0u8.hash(state);
                model.hash(state);
                model_group.hash(state);
                children.len().hash(state);
                for child in children {
                    child.key.hash(state);
                    feed(&child.node, state);
                }
            }
            ClassificationNode::Terminal {
                route,
                group,
                role,
                always_route,
                ..
            } => {
                1u8.hash(state);
                route.hash(state);
                group.hash(state);
                role.hash(state);
                always_route.hash(state);
            }
            ClassificationNode::Filter {
                patterns,
                outcome,
                redirect_to,
                match_interlingua,
                ..
            } => {
                2u8.hash(state);
                patterns.hash(state);
                format!("{outcome:?}").hash(state);
                redirect_to.hash(state);
                format!("{match_interlingua:?}").hash(state);
            }
            ClassificationNode::Fallback { node, .. } => {
                3u8.hash(state);
                feed(node, state);
            }
        }
    }
    // fnv1a64 over a debug-stable feed is overkill; the standard hasher
    // suffices — the fingerprint never leaves the process (reload/compare
    // only), so cross-run stability is not required.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    feed(&tree.root, &mut hasher);
    std::hash::Hasher::finish(&hasher)
}

fn node_kind(node: &ClassificationNode) -> &'static str {
    match node {
        ClassificationNode::Classifier { .. } => "classifier",
        ClassificationNode::Terminal { .. } => "terminal",
        ClassificationNode::Filter { .. } => "filter",
        ClassificationNode::Fallback { .. } => "fallback",
    }
}

fn format_path(path: &[&str]) -> String {
    if path.is_empty() {
        "(root)".to_string()
    } else {
        path.join("/")
    }
}

/// Resolve a child-key path to a shared node. An empty path targets the
/// node itself (the root at top level). Only classifier nodes have keyed
/// children — any other node on the path (including fallback wrappers, which
/// carry no keys) ends resolution with `NotFound`.
fn resolve<'a>(
    node: &'a ClassificationNode,
    path: &[&str],
) -> Result<&'a ClassificationNode, ReloadError> {
    let mut current = node;
    for (i, key) in path.iter().enumerate() {
        let ClassificationNode::Classifier { children, .. } = current else {
            return Err(ReloadError::NotFound {
                path: path[..=i].join("/"),
            });
        };
        current = children
            .iter()
            .find(|c| &c.key == key)
            .map(|c| &c.node)
            .ok_or_else(|| ReloadError::NotFound {
                path: path[..=i].join("/"),
            })?;
    }
    Ok(current)
}

/// Resolve a child-key path to an exclusive node (same rules as `resolve`).
fn resolve_mut<'a>(
    node: &'a mut ClassificationNode,
    path: &[&str],
) -> Result<&'a mut ClassificationNode, ReloadError> {
    let mut current = node;
    for (i, key) in path.iter().enumerate() {
        let next = match current {
            ClassificationNode::Classifier { children, .. } => {
                children.iter_mut().find(|c| &c.key == key).map(|c| &mut c.node)
            }
            _ => None,
        };
        current = next.ok_or_else(|| ReloadError::NotFound {
            path: path[..=i].join("/"),
        })?;
    }
    Ok(current)
}

#[cfg(test)]
#[path = "../../tests/tree_reload.rs"]
mod tests;
