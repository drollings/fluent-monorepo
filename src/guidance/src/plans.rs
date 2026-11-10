//! Durable work plans: human-authored intent the loop tracks and
//! verifies but never executes. Plans reference work items by stable
//! ID; they never enter the scheduler, so a re-plan merges by ID and
//! preserves completed work instead of restarting it.

use std::collections::BTreeMap;

/// Acceptance gate for one plan item: the package check plus the named
/// test that must pass for the item to count as verified. Evaluated
/// through the seal runners, never by the plan itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AcceptanceGate {
    /// Package directory the check covers.
    pub package: String,
    /// Named test that must pass (baseline-compared by the caller).
    pub test_name: Option<String>,
}

/// Lifecycle of one plan item. Scheduler states are never reused here:
/// a plan records author intent, not queue position.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanItemState {
    /// Awaiting its acceptance gate.
    #[default]
    Open,
    /// Gate passed; preserved across revisions.
    Verified,
}

/// One planned unit of non-mechanical work.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanItem {
    /// Stable identity across revisions.
    pub id: String,
    /// Target item or file the work concerns.
    pub target: String,
    /// Gate that proves the item done.
    pub acceptance: AcceptanceGate,
    /// Lifecycle state.
    pub state: PlanItemState,
}

/// A reviewable plan: ordered items plus the changelog that tells the
/// loop what a revision added or changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkPlan {
    /// Plan identity.
    pub id: String,
    /// Items by stable ID, in import order.
    pub items: BTreeMap<String, PlanItem>,
    /// Insertion order of IDs (serde preserves the map, not the order).
    pub order: Vec<String>,
    /// Human-readable revision notes, oldest first.
    pub changelog: Vec<String>,
}

impl WorkPlan {
    /// Import items as a new plan revision zero. Duplicate IDs refuse;
    /// the loop never sees a half-imported plan.
    pub fn import(id: &str, items: Vec<PlanItem>) -> Result<Self, PlanError> {
        let mut plan = Self { id: id.to_string(), ..Self::default() };
        for item in items {
            if plan.items.contains_key(&item.id) {
                return Err(PlanError::Duplicate(item.id));
            }
            plan.order.push(item.id.clone());
            plan.changelog.push(format!("add {}", item.id));
            plan.items.insert(item.id.clone(), item);
        }
        Ok(plan)
    }

    /// Revise by ID: completed items survive untouched, new IDs append,
    /// changed definitions reopen with a changelog line. Removing an ID
    /// drops it with a line; completed work is never re-derived, only
    /// its definition replaced.
    pub fn revise(&mut self, items: Vec<PlanItem>) {
        let mut seen = std::collections::HashSet::new();
        for item in items {
            seen.insert(item.id.clone());
            match self.items.get(&item.id) {
                Some(existing)
                    if existing.target == item.target
                        && existing.acceptance == item.acceptance =>
                {
                    // Same definition: state (including Verified) survives.
                }
                Some(_) => {
                    let state = PlanItemState::Open;
                    self.changelog.push(format!("change {}", item.id));
                    self.items.insert(
                        item.id.clone(),
                        PlanItem { state, ..item.clone() },
                    );
                    if !self.order.contains(&item.id) {
                        self.order.push(item.id.clone());
                    }
                }
                None => {
                    self.changelog.push(format!("add {}", item.id));
                    self.order.push(item.id.clone());
                    self.items.insert(item.id.clone(), item);
                }
            }
        }
        let removed: Vec<String> = self
            .order
            .iter()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        for id in removed {
            self.items.remove(&id);
            self.order.retain(|kept| kept != &id);
            self.changelog.push(format!("remove {id}"));
        }
    }

    /// Verify open and verified items against their gates: open items
    /// whose gate passes become verified; verified items whose gate
    /// later regresses reopen. Returns the reopened IDs.
    pub fn verify(
        &mut self,
        build: &crate::seal::BuildRunner,
        test: &crate::seal::TestRunner,
    ) -> Vec<String> {
        let mut reopened = Vec::new();
        for item in self.items.values_mut() {
            let build_ok = build(std::path::Path::new(&item.acceptance.package))
                .is_ok_and(|verdict| verdict.clean);
            let test_ok = item.acceptance.test_name.as_deref().is_none_or(|_| {
                test(std::path::Path::new(&item.acceptance.package))
                    .is_ok_and(|report| report.new_failures.is_empty())
            });
            if build_ok && test_ok {
                item.state = PlanItemState::Verified;
            } else if item.state == PlanItemState::Verified {
                item.state = PlanItemState::Open;
                reopened.push(item.id.clone());
            }
        }
        reopened.sort();
        reopened
    }

    /// Nothing open: every item verified.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.items.values().all(|item| item.state == PlanItemState::Verified)
    }

    /// Write-ahead intent for a revision: hash the serialized plan
    /// before and after, so a crash between import and persist
    /// reconciles instead of losing the revision.
    pub fn revision_intent(&self, next: &Self) -> crate::journal::IntentRecord {
        let pre = common_core::hash::blake3_hex(
            serde_json::to_string(self).unwrap_or_default().as_bytes(),
        );
        let post = common_core::hash::blake3_hex(
            serde_json::to_string(next).unwrap_or_default().as_bytes(),
        );
        crate::journal::IntentRecord {
            id: format!("plan:{}", self.id),
            file: format!("plan:{}", self.id),
            pre_hash: format!("plan:{pre}"),
            post_hash: format!("plan:{post}"),
        }
    }
}

/// Plan import failure: duplicate stable IDs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// Two items share one stable ID.
    #[error("duplicate plan item: {0}")]
    Duplicate(String),
}

#[cfg(test)]
#[path = "../tests/work_plans.rs"]
mod work_plans_tests;
