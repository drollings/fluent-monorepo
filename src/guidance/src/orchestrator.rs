//! Narrow inbound orchestrator contract: validated posts, claims,
//! and reads that never touch running scheduler state.
//!
//! The queue handle itself is never exposed: posts validate closed
//! enums and existing dependencies, then enqueue as queued work only.
//! In-progress items are unreachable through this surface, so
//! single-loop ownership and crash-resume invariants hold by
//! construction. Persistence is file-based: snapshots and ledgers
//! serialize to JSON the caller stores; the contract never holds a
//! live handle across calls.

use std::collections::BTreeMap;
use std::path::Path;

use crate::journal::review_journal;
use crate::maintenance::{MaintenanceQueue, QueueError};
use crate::plans::{PlanError, PlanItem, WorkPlan};
use crate::query::context_pack::{build_task_pack, ContextPack, PackInput, PackManifest};

/// Fix classes the contract accepts. Closed: anything else refuses
/// before touching the queue.
pub const KNOWN_KINDS: &[&str] = &["rustfmt", "clippy-fix", "index", "commentary-refresh", "plan-item"];

/// One validated work post with its acceptance gate.
#[derive(Debug, Clone)]
pub struct PostWork {
    /// Stable id (also the graph key).
    pub id: String,
    /// Component scope the narrowest gate covers.
    pub scope: String,
    /// Fix class; must name a known kind.
    pub kind: String,
    /// Dependency ids; every one must already be queued.
    pub deps: Vec<String>,
}

/// Contract refusal: validation failed, nothing was mutated.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    /// Empty stable id.
    #[error("empty work id")]
    EmptyId,
    /// Fix class outside the closed set.
    #[error("unknown fix class: {0}")]
    UnknownKind(String),
    /// Dependency names no queued item.
    #[error("dangling dependency: {0}")]
    DanglingDep(String),
    /// Stable id already queued.
    #[error("duplicate work item: {0}")]
    Duplicate(String),
    /// Escalation id unknown to the ledger.
    #[error("unknown escalation: {0}")]
    UnknownEscalation(String),
    /// Transition the ledger refuses (resolve unclaimed, claim resolved).
    #[error("refused escalation transition: {0}")]
    BadTransition(String),
    /// Plan import failure.
    #[error("plan import failed: {0}")]
    Plan(String),
}

impl From<QueueError> for ContractError {
    fn from(error: QueueError) -> Self {
        match error {
            QueueError::Duplicate(id) => Self::Duplicate(id),
            QueueError::AlreadyRunning => Self::BadTransition("loop already running".to_string()),
        }
    }
}

impl From<PlanError> for ContractError {
    fn from(error: PlanError) -> Self {
        match error {
            PlanError::Duplicate(id) => Self::Plan(format!("duplicate plan item: {id}")),
        }
    }
}

/// Validate and enqueue one unit as queued work. Refusals (empty id,
/// unknown kind, dangling dep, duplicate) leave the queue unchanged.
pub fn post_work(queue: &mut MaintenanceQueue, req: &PostWork) -> Result<(), ContractError> {
    if req.id.is_empty() {
        return Err(ContractError::EmptyId);
    }
    if !KNOWN_KINDS.contains(&req.kind.as_str()) {
        return Err(ContractError::UnknownKind(req.kind.clone()));
    }
    let deps: Vec<&str> = req.deps.iter().map(String::as_str).collect();
    for dep in &deps {
        if queue.state(dep).is_none() {
            return Err(ContractError::DanglingDep((*dep).to_string()));
        }
    }
    queue.enqueue(&req.id, &req.scope, &req.kind, &deps)?;
    Ok(())
}

/// Lifecycle of one escalation in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EscalationLife {
    /// Filed, awaiting an owner.
    Open,
    /// Owned, being worked.
    Claimed,
    /// Handled; terminal.
    Resolved,
}

/// One ledger entry: who owns it and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EscalationEntry {
    /// Escalation id.
    pub id: String,
    /// Owner, once claimed or resolved.
    pub owner: Option<String>,
    /// Lifecycle state.
    pub state: EscalationLife,
}

/// Claim/resolve ledger. Transitions validate; bad ones refuse without
/// mutating the entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EscalationLedger {
    /// Entries by escalation id, sorted.
    pub entries: BTreeMap<String, EscalationEntry>,
}

impl EscalationLedger {
    /// File one escalation as open. Duplicate ids refuse.
    pub fn file(&mut self, id: &str) -> Result<(), ContractError> {
        if self.entries.contains_key(id) {
            return Err(ContractError::Duplicate(id.to_string()));
        }
        self.entries.insert(
            id.to_string(),
            EscalationEntry { id: id.to_string(), owner: None, state: EscalationLife::Open },
        );
        Ok(())
    }

    /// Claim an open escalation for an owner. Resolved entries never
    /// reopen; claiming a claimed entry refuses.
    pub fn claim(&mut self, id: &str, owner: &str) -> Result<(), ContractError> {
        match self.entries.get_mut(id) {
            None => Err(ContractError::UnknownEscalation(id.to_string())),
            Some(entry) if entry.state == EscalationLife::Open => {
                entry.state = EscalationLife::Claimed;
                entry.owner = Some(owner.to_string());
                Ok(())
            }
            Some(_) => Err(ContractError::BadTransition(format!("claim {id}"))),
        }
    }

    /// Resolve a claimed escalation (owner must match) or an open one
    /// with an owner supplied. Resolving twice refuses.
    pub fn resolve(&mut self, id: &str, owner: &str) -> Result<(), ContractError> {
        match self.entries.get_mut(id) {
            None => Err(ContractError::UnknownEscalation(id.to_string())),
            Some(entry) if entry.state == EscalationLife::Resolved => {
                Err(ContractError::BadTransition(format!("resolve {id}")))
            }
            Some(entry)
                if entry.state == EscalationLife::Open
                    || entry.owner.as_deref() == Some(owner) =>
            {
                entry.state = EscalationLife::Resolved;
                entry.owner = Some(owner.to_string());
                Ok(())
            }
            Some(_) => Err(ContractError::BadTransition(format!("resolve {id}"))),
        }
    }
}

/// Import plan items into a reviewable plan. Delegates to the plan
/// store; the loop never executes the content.
pub fn import_plan(id: &str, items: Vec<PlanItem>) -> Result<WorkPlan, ContractError> {
    Ok(WorkPlan::import(id, items)?)
}

/// Tail the ledger: the last `count` journal groups at or after
/// `since` (a group index cursor), rendered as `file:class:entries`.
/// Pure read over the journal dir; never writes.
#[must_use]
pub fn tail_ledger(journal_dir: &Path, since: usize, count: usize) -> Vec<String> {
    review_journal(journal_dir)
        .into_iter()
        .skip(since)
        .take(count.max(1))
        .map(|group| format!("{}:{}:{}", group.file, group.fix_class, group.entries.len()))
        .collect()
}

/// Request a context pack through the single pack builder. No second
/// pack path; the bytes match `build_task_pack` exactly.
#[must_use]
pub fn request_pack(input: &PackInput) -> (ContextPack, PackManifest) {
    build_task_pack(input)
}

#[cfg(test)]
#[path = "../tests/orchestrator_contract.rs"]
mod orchestrator_contract_tests;
