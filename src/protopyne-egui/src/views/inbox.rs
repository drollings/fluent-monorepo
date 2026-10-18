//! Approval-inbox projection: which rows await a human, which are settled.
//!
//! String tags only, on the neutral contract — the same tags the core
//! publishes, compared as text so this crate never links the proposal types.
//! Only `PendingHuman` rows carry an approve/reject affordance; every other
//! tag (including unknown future ones) settles fail-closed with none.

use protopyne_view::ProposalView;

/// Read-only inbox split: rows awaiting a human act versus settled rows.
#[derive(Debug, Clone, PartialEq)]
pub struct InboxModel {
    /// Rows in `PendingHuman`, in inbox order.
    actionable: Vec<ProposalView>,
    /// Every other row, in inbox order.
    settled: Vec<ProposalView>,
}

impl InboxModel {
    /// Lifecycle tag that carries the approve/reject affordance.
    pub const ACTIONABLE_TAG: &str = "PendingHuman";

    /// Split inbox rows: `PendingHuman` rows await a human, everything else
    /// is settled. Infallible — unknown tags settle, never act.
    #[must_use]
    pub fn of(proposals: Vec<ProposalView>) -> Self {
        let mut actionable = Vec::new();
        let mut settled = Vec::new();
        for proposal in proposals {
            if Self::is_actionable(&proposal.state) {
                actionable.push(proposal);
            } else {
                settled.push(proposal);
            }
        }
        Self {
            actionable,
            settled,
        }
    }

    /// Whether a lifecycle tag carries approve/reject. Only the waiting
    /// tag does; unknown tags fail closed.
    #[must_use]
    pub fn is_actionable(state: &str) -> bool {
        state == Self::ACTIONABLE_TAG
    }

    /// Rows awaiting a human act, in inbox order.
    #[must_use]
    pub fn actionable(&self) -> &[ProposalView] {
        &self.actionable
    }

    /// Settled rows, in inbox order.
    #[must_use]
    pub fn settled(&self) -> &[ProposalView] {
        &self.settled
    }

    /// Whether no row awaits a human.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actionable.is_empty()
    }
}
