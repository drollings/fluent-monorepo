//! `Target → WorkUnit` bridge.
//!
//! **Status: compatibility surface (scaffold).** Exercised by tests today;
//! kept as the single `Target`-to-`Component` home so resolver execution
//! never grows a second adapter.
//!
//! A [`Target`] (from the target registry / resolver world, with bitset
//! capabilities) adapted to the fluent-wvr [`Component`] world so the
//! resolver's `ExecutionPlan` can run under `SupervisedBatch` / `WorkUnit` semantics.
//!
//! This module replaces the pruned `DagExecutor` Its `execute` mirrors
//! `CommandUnit`'s semantics exactly via the shared `run_shell_command`
//! helper — sequential determinism comes from walking
//! `ExecutionPlan::order`, and per-target result linkage is preserved via
//! `Target::id`.

use crate::target::{CapabilityRegistry, Target};
use crate::work_unit::{impl_name_command_fields, run_shell_command};
use fluent_wvr::prelude::*;
use internment::ArcIntern;

/// A [`Target`] adapted to the `WorkUnit`/`Component` interface.
///
/// `depends` / `provides` are the `Target`'s bitsets resolved to capability
/// names via a [`CapabilityRegistry`] (bit indices are per-registry; indices
/// that map to no name are skipped with a `tracing::warn!`).
#[derive(Debug, Clone)]
pub struct TargetWorkUnit {
    pub name: ArcIntern<str>,
    pub depends: Vec<ArcIntern<str>>,
    pub provides: Vec<ArcIntern<str>>,
    pub command: String,
    pub essential: bool,
}

impl TargetWorkUnit {
    /// Maps the `Target`'s `BitVec` `depends`/`provides` to capability names.
    ///
    /// Bit indices are per-registry; indices with no registered name are
    /// skipped with a `tracing::warn!` rather than failing.
    pub fn from_target(target: &Target, caps: &CapabilityRegistry) -> Self {
        let depends = bitvec_to_names(target.depends.iter_ones(), caps, "depends", &target.name);
        let provides = bitvec_to_names(target.provides.iter_ones(), caps, "provides", &target.name);
        Self {
            name: target.name.clone(),
            depends,
            provides,
            command: target.command.clone(),
            essential: target.essential,
        }
    }
}

fn bitvec_to_names(
    bits: impl Iterator<Item = usize>,
    caps: &CapabilityRegistry,
    kind: &str,
    target: &str,
) -> Vec<ArcIntern<str>> {
    bits.filter_map(|idx| {
        if let Some(name) = caps.get_name(idx) {
            Some(name)
        } else {
            tracing::warn!(
                target,
                kind,
                bit_index = idx,
                "bit index not mapped in CapabilityRegistry; skipping"
            );
            None
        }
    })
    .collect()
}

impl WorkUnit for TargetWorkUnit {
    fn name(&self) -> &str {
        &self.name
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        run_shell_command(&self.name, &self.command, ctx)
    }
}

impl_name_command_fields!(
    TargetWorkUnit,
    |unit: &mut TargetWorkUnit, value: &str| unit.name = ArcIntern::from(value),
    |unit: &TargetWorkUnit| unit.name.to_string(),
    "Target name"
);

impl_component!(TargetWorkUnit);

#[cfg(test)]
#[path = "../tests/target_work_unit.rs"]
mod tests;
