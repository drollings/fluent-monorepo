//! Versioned maintenance policy: scope, mechanical classes, human-owned
//! documents, attempt limits, and approvals.
//!
//! The policy version is a staleness input alongside content, prompt
//! version, and model identity: changing the policy re-derives exactly
//! the affected work, never the whole workspace.

use serde::{Deserialize, Serialize};

/// Declarative, versioned statement of what the loop may do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenancePolicy {
    /// Monotonic version; bumped on every policy edit.
    #[serde(default = "default_policy_version")]
    pub version: u64,
    /// Workspace scopes the loop owns (path prefixes).
    #[serde(default)]
    pub scope: Vec<String>,
    /// Fix classes the loop may auto-execute (`rustfmt`, `clippy-fix`).
    #[serde(default)]
    pub mechanical_classes: Vec<String>,
    /// Documents owned by humans, never machine-written.
    #[serde(default)]
    pub human_owned: Vec<String>,
    /// Bounded attempts per work item.
    #[serde(default = "default_attempt_limit")]
    pub attempt_limit: u32,
    /// Fix classes requiring approval before applying.
    #[serde(default)]
    pub requires_approval: Vec<String>,
}

fn default_policy_version() -> u64 {
    1
}

fn default_attempt_limit() -> u32 {
    3
}

impl Default for MaintenancePolicy {
    fn default() -> Self {
        Self {
            version: default_policy_version(),
            scope: Vec::new(),
            mechanical_classes: vec!["rustfmt".to_string()],
            human_owned: Vec::new(),
            attempt_limit: default_attempt_limit(),
            requires_approval: Vec::new(),
        }
    }
}

/// Fingerprint of the policy for staleness keys: content, prompt version,
/// model identity, and policy version hash together downstream.
#[must_use]
pub fn policy_fingerprint(policy: &MaintenancePolicy) -> String {
    let canonical = serde_json::to_string(policy).unwrap_or_default();
    common_core::hash::blake3_hex(canonical.as_bytes())
}

/// Whether stored work predates the current policy: a version mismatch
/// re-derives exactly the affected items, never the whole workspace.
#[must_use]
pub fn policy_stale(stored_version: Option<u64>, current: &MaintenancePolicy) -> bool {
    stored_version.is_none_or(|version| version != current.version)
}

#[cfg(test)]
#[path = "../tests/policy_staleness.rs"]
mod policy_staleness_tests;
