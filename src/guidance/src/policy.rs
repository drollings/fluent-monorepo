//! Versioned maintenance policy: scope, mechanical classes, human-owned
//! documents, attempt limits, and approvals.
//!
//! The policy version is a staleness input alongside content, prompt
//! version, and model identity: changing the policy re-derives exactly
//! the affected work, never the whole workspace.

use serde::{Deserialize, Serialize};

/// Declarative, versioned statement of what the loop may do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Declarative, versioned statement of what the loop may do.
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
    /// Autonomy cap over the fix tiers (`L0` index only, `L1` and up run
    /// the deterministic rungs; model drafts need `L3`, which behaves
    /// as `L2` until a generative rung lands).
    #[serde(default = "default_autonomy")]
    pub autonomy: String,
}

fn default_policy_version() -> u64 {
    1
}

fn default_attempt_limit() -> u32 {
    3
}

fn default_autonomy() -> String {
    "L2".to_string()
}

impl Default for MaintenancePolicy {
    fn default() -> Self {
        Self {
            version: default_policy_version(),
            scope: Vec::new(),
            mechanical_classes: vec!["rustfmt".to_string(), "clippy-fix".to_string()],
            human_owned: Vec::new(),
            attempt_limit: default_attempt_limit(),
            requires_approval: Vec::new(),
            autonomy: default_autonomy(),
        }
    }
}

/// Whether the autonomy cap permits the fix rungs: `L0` is index-only
/// (sync and report, never rewrite), `L1` and up run the deterministic
/// fixers the mechanical list allows.
#[must_use]
/// Whether the autonomy cap permits the fix rungs: `L0` is index-only
/// (sync and report, never rewrite), `L1` and up run the deterministic
/// fixers the mechanical list allows.
pub fn autonomy_allows_fix(policy: &MaintenancePolicy) -> bool {
    policy.autonomy.as_str() != "L0"
}

/// Fingerprint of the policy for staleness keys: content, prompt version,
/// model identity, and policy version hash together downstream.
#[must_use]
/// Fingerprint of the policy for staleness keys: content, prompt version,
/// model identity, and policy version hash together downstream.
pub fn policy_fingerprint(policy: &MaintenancePolicy) -> String {
    let canonical = serde_json::to_string(policy).unwrap_or_default();
    common_core::hash::blake3_hex(canonical.as_bytes())
}

/// Fingerprint of one policy section for staleness keys: a policy edit
/// re-derives exactly the receipts that consumed the changed section.
#[must_use]
/// Fingerprint of one policy section for staleness keys: a policy edit
/// re-derives exactly the receipts that consumed the changed section.
pub fn section_fingerprint(policy: &MaintenancePolicy, section: &str) -> String {
    let canonical = match section {
        "mechanical" => serde_json::to_string(&policy.mechanical_classes).unwrap_or_default(),
        "scope" => serde_json::to_string(&policy.scope).unwrap_or_default(),
        "human_owned" => serde_json::to_string(&policy.human_owned).unwrap_or_default(),
        "approval" => serde_json::to_string(&policy.requires_approval).unwrap_or_default(),
        "limits" => policy.attempt_limit.to_string(),
        _ => policy.version.to_string(),
    };
    common_core::hash::blake3_hex(canonical.as_bytes())
}

/// Whether stored work predates the current policy: a version mismatch
/// re-derives exactly the affected items, never the whole workspace.
#[must_use]
/// Whether stored work predates the current policy: a version mismatch
/// re-derives exactly the affected items, never the whole workspace.
pub fn policy_stale(stored_version: Option<u64>, current: &MaintenancePolicy) -> bool {
    stored_version.is_none_or(|version| version != current.version)
}

#[cfg(test)]
#[path = "../tests/policy_staleness.rs"]
mod policy_staleness_tests;