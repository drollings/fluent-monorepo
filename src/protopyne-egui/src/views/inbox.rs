//! Approval-inbox projection: which rows await a human, which are settled.
//!
//! String tags only, on the neutral contract — the same tags the core
//! publishes, compared as text so this crate never links the proposal types.
//! Only `PendingHuman` rows carry an approve/reject affordance; every other
//! tag (including unknown future ones) settles fail-closed with none.

use protopyne_view::{GatewayError, ProposalView, TerminalGateway};

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

/// One operator decision on an actionable inbox row.
///
/// The action carries only the id and (for a reject) the reason; applying it
/// invokes exactly one gateway verb, so a rendered click can never fan out
/// into a second write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxAction {
    /// Approve the pending proposal.
    Approve {
        /// Proposal to approve.
        proposal_id: String,
    },
    /// Reject the pending proposal with a reason.
    Reject {
        /// Proposal to reject.
        proposal_id: String,
        /// Why the human rejected it.
        reason: String,
    },
}

impl InboxAction {
    /// An approve click for `proposal_id`.
    #[must_use]
    pub fn approve(proposal_id: &str) -> Self {
        Self::Approve {
            proposal_id: proposal_id.to_owned(),
        }
    }

    /// A reject click for `proposal_id`.
    #[must_use]
    pub fn reject(proposal_id: &str, reason: &str) -> Self {
        Self::Reject {
            proposal_id: proposal_id.to_owned(),
            reason: reason.to_owned(),
        }
    }

    /// Invoke the one gateway verb this action names.
    pub fn apply(&self, gateway: &dyn TerminalGateway) -> Result<(), GatewayError> {
        match self {
            Self::Approve { proposal_id } => gateway.approve(proposal_id),
            Self::Reject {
                proposal_id,
                reason,
            } => gateway.reject(proposal_id, reason),
        }
    }
}
