//! Named input components for sidecar receipts, plus the header-only
//! sweep that backdates receipts whose consumed inputs changed.
//!
//! Each receipt records only the components it consumed; a change to an
//! unrecorded component never invalidates it. Comparison is per key, so
//! prompt or model rotation cannot re-lint anything by itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fluent_types::sidecar::SidecarHeader;

use crate::policy::{section_fingerprint, MaintenancePolicy};

/// Component names recorded by the gen receipt today.
pub const COMP_PARSER: &str = "parser";
/// Sidecar layout version the receipt was written against.
pub const COMP_SCHEMA: &str = "schema";
/// Policy mechanical-classes section consumed by fix-class gating.
pub const COMP_POLICY_MECHANICAL: &str = "policy.mechanical";
/// Policy scope section consumed by ownership checks.
pub const COMP_POLICY_SCOPE: &str = "policy.scope";
/// Toolchain identity (compiler plus lint tool versions).
pub const COMP_TOOLCHAIN: &str = "toolchain";
/// Format configuration identity.
pub const COMP_FMT_CFG: &str = "fmt_cfg";
/// Lint configuration identity.
pub const COMP_LINT_CFG: &str = "lint_cfg";
/// Lockfile identity.
pub const COMP_LOCKFILE: &str = "lockfile";
/// Configuration-matrix cell set identity.
pub const COMP_CFG_MATRIX: &str = "cfg_matrix";
/// Model identity prefix per role (`model:<role>`).
pub const COMP_MODEL_PREFIX: &str = "model:";
/// Prompt identity prefix per role (`prompt:<role>`).
pub const COMP_PROMPT_PREFIX: &str = "prompt:";
/// Embedder identity.
pub const COMP_EMBEDDER: &str = "embedder";

/// Optional input fingerprints beyond the policy sections. Each `Some`
/// value is already a hex hash of that input; `None` means the caller
/// did not consume it, so it never appears in the receipt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComponentInputs {
    /// Toolchain fingerprint.
    pub toolchain: Option<String>,
    /// Format configuration fingerprint.
    pub fmt_cfg: Option<String>,
    /// Lint configuration fingerprint.
    pub lint_cfg: Option<String>,
    /// Lockfile fingerprint.
    pub lockfile: Option<String>,
    /// Configuration-matrix fingerprint.
    pub cfg_matrix: Option<String>,
    /// Model fingerprint for the triage role.
    pub model_triage: Option<String>,
    /// Prompt fingerprint for the triage role.
    pub prompt_triag: Option<String>,
    /// Embedder fingerprint.
    pub embedder: Option<String>,
}

/// Named input components and their hex hashes. Absent keys were not
/// consumed and never invalidate on change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComponentVector {
    /// Component name to hex hash.
    pub components: BTreeMap<String, String>,
}

impl ComponentVector {
    /// Current vector for a gen receipt under one policy.
    #[must_use]
    pub fn current(policy: &MaintenancePolicy) -> Self {
        Self::current_with(policy, &ComponentInputs::default())
    }

    /// Current vector plus the explicitly consumed optional inputs.
    /// Absent inputs never appear, so their later change can never
    /// invalidate this receipt.
    #[must_use]
    pub fn current_with(policy: &MaintenancePolicy, inputs: &ComponentInputs) -> Self {
        let mut components = BTreeMap::new();
        components.insert(
            COMP_PARSER.to_string(),
            common_core::hash::blake3_hex(env!("CARGO_PKG_VERSION").as_bytes()),
        );
        components.insert(
            COMP_SCHEMA.to_string(),
            common_core::hash::blake3_hex(
                &crate::sync::json_store::SIDECAR_SCHEMA_VERSION.to_be_bytes(),
            ),
        );
        components.insert(
            COMP_POLICY_MECHANICAL.to_string(),
            section_fingerprint(policy, "mechanical"),
        );
        components.insert(
            COMP_POLICY_SCOPE.to_string(),
            section_fingerprint(policy, "scope"),
        );
        if let Some(value) = &inputs.toolchain {
            components.insert(COMP_TOOLCHAIN.to_string(), value.clone());
        }
        if let Some(value) = &inputs.fmt_cfg {
            components.insert(COMP_FMT_CFG.to_string(), value.clone());
        }
        if let Some(value) = &inputs.lint_cfg {
            components.insert(COMP_LINT_CFG.to_string(), value.clone());
        }
        if let Some(value) = &inputs.lockfile {
            components.insert(COMP_LOCKFILE.to_string(), value.clone());
        }
        if let Some(value) = &inputs.cfg_matrix {
            components.insert(COMP_CFG_MATRIX.to_string(), value.clone());
        }
        if let Some(value) = &inputs.model_triage {
            components.insert(format!("{COMP_MODEL_PREFIX}triage"), value.clone());
        }
        if let Some(value) = &inputs.prompt_triag {
            components.insert(format!("{COMP_PROMPT_PREFIX}triage"), value.clone());
        }
        if let Some(value) = &inputs.embedder {
            components.insert(COMP_EMBEDDER.to_string(), value.clone());
        }
        Self { components }
    }

    /// Keep only the named consumed inputs: receipts record exactly
    /// what they consumed, so unrelated changes never invalidate them.
    #[must_use]
    pub fn select(&self, names: &[&str]) -> Self {
        Self {
            components: self
                .components
                .iter()
                .filter(|(name, _)| names.contains(&name.as_str()))
                .map(|(name, hash)| (name.clone(), hash.clone()))
                .collect(),
        }
    }
}

/// One save receipt: the components this derivation consumed plus the
/// source bytes it read. Bundled so later inputs extend the struct
/// instead of widening another call signature.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidecarReceipt {
    /// Consumed input components.
    pub components: ComponentVector,
    /// Hex hash of the source bytes read, when known.
    pub source_hash: Option<String>,
}

impl SidecarReceipt {
    /// Receipt over explicit components and an optional source hash.
    #[must_use]
    pub fn new(components: ComponentVector, source_hash: Option<String>) -> Self {
        Self {
            components,
            source_hash,
        }
    }
}

/// Verdict of the sweep for one sidecar file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepDecision {
    /// All recorded components match; nothing to do.
    Current,
    /// A recorded component changed; the sidecar was backdated.
    Backdated,
    /// No header or no source to compare against; left untouched.
    Unknown,
}

/// Outcome of the sweep for one sidecar file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepOutcome {
    /// Sidecar path, as walked.
    pub path: PathBuf,
    /// Sweep verdict.
    pub decision: SweepDecision,
}

/// Backdate every sidecar under `sidecar_dir` whose recorded components
/// disagree with `current`, reading first lines only. Invalidation moves
/// the sidecar timestamp, never the source. Sources resolve against
/// `source_dir` by stripping one `.json` suffix off the sidecar path.
#[must_use]
pub fn sweep_sidecars(
    sidecar_dir: &Path,
    source_dir: &Path,
    current: &ComponentVector,
) -> Vec<SweepOutcome> {
    let mut sidecars = Vec::new();
    crate::walk::walk_files(sidecar_dir, &["json"], |path| {
        sidecars.push(path.to_path_buf());
    });
    sidecars.sort();
    sidecars
        .into_iter()
        .map(|path| {
            let decision = sweep_one(&path, sidecar_dir, source_dir, current);
            SweepOutcome { path, decision }
        })
        .collect()
}

/// Read only the first line of a file: the sweep never pays for bodies.
fn first_line(path: &Path) -> Option<String> {
    use std::io::BufRead as _;
    let file = std::fs::File::open(path).ok()?;
    let mut lines = std::io::BufReader::new(file).lines();
    lines.next()?.ok()
}

fn sweep_one(
    sidecar: &Path,
    sidecar_dir: &Path,
    source_dir: &Path,
    current: &ComponentVector,
) -> SweepDecision {
    let Some(line) = first_line(sidecar) else {
        return SweepDecision::Unknown;
    };
    let Some(header) = SidecarHeader::parse_line(&line) else {
        return SweepDecision::Unknown;
    };
    let stale = header.components.iter().any(|(name, hash)| {
        current.components.get(name).is_none_or(|want| want != hash)
    });
    if !stale {
        return SweepDecision::Current;
    }
    let rel = sidecar
        .strip_prefix(sidecar_dir)
        .unwrap_or(sidecar)
        .to_string_lossy();
    let rel = rel.strip_suffix(".json").unwrap_or(&rel);
    let source = source_dir.join(rel);
    if !crate::freshness::backdate_sidecar(sidecar, &source) {
        return SweepDecision::Unknown;
    }
    SweepDecision::Backdated
}

/// Whether a recorded receipt stays valid under the current vector
/// for exactly the inputs it consumed. Task value: same inputs mean
/// the stored derivation is still right. Unrecorded inputs never
/// invalidate, and unknown names fail closed toward reprocessing.
#[must_use]
pub fn receipt_reusable(
    recorded: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
    consumed: &[&str],
) -> bool {
    consumed.iter().all(|name| match recorded.get(*name) {
        Some(want) => current.get(*name).is_some_and(|have| have == want),
        None => false,
    })
}

#[cfg(test)]
#[path = "../tests/component_sweep.rs"]
mod component_sweep_tests;

#[cfg(test)]
#[path = "../tests/m1_truth.rs"]
mod m1_truth_tests;
