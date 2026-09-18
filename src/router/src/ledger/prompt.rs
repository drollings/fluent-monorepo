//! M10.2 migration shim: the pure prompt-assembler mechanics live in
//! `fluent-knowledge` (`lod_policy`, `selector`, `assembler` over the content
//! seam) — this module re-exports them so existing
//! `crate::ledger::prompt::…` paths (including the binary's) keep working.
//! New code imports from `fluent-knowledge` directly. The `ChatBackend`
//! tier-derivation worker stays in `tiering.rs`.

pub use fluent_knowledge::assembler::{AssembledPrompt, LedgerPromptAssembler};
pub use fluent_knowledge::lod_policy::{
    parse_tiers, LodSpec, PromptBudget, RelevanceSignal, WorkerContext,
};
pub use fluent_knowledge::selector::{CosineRankSelector, FidelitySelector, UniformSelector};

#[cfg(test)]
#[path = "../../tests/ledger_prompt.rs"]
mod tests;
