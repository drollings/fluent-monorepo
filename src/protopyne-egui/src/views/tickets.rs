//! Manual-ticket projection: which tickets wait, what drifted, and the one
//! gateway write each operator action names.
//!
//! String tags and counts only, on the neutral contract — the same tags the
//! core publishes, compared as text so this crate never links the venue
//! types. Only waiting tickets carry a confirm affordance; settled tickets
//! leave the wait and read only as drift rows, if at all.

use protopyne_view::{GatewayError, TerminalGateway, TicketDraft, TicketDriftView, TicketView};

/// Read-only ticket split: rows awaiting the human's fill report versus
/// flagged disagreements.
#[derive(Debug, Clone, PartialEq)]
/// Read-only ticket split: rows awaiting the human's fill report versus
/// flagged disagreements.
pub struct TicketModel {
    /// Tickets still waiting, in ticket order.
    waiting: Vec<TicketView>,
    /// Flagged disagreements, oldest first.
    drifts: Vec<TicketDriftView>,
}

impl TicketModel {
    /// Pull both ticket reads and shape them. Infallible in shape — a
    /// refusing gateway refuses the whole pull, never an empty model.
    pub fn pull(gateway: &dyn TerminalGateway) -> Result<Self, GatewayError> {
        Ok(Self {
            waiting: gateway.tickets_waiting()?,
            drifts: gateway.ticket_drifts()?,
        })
    }

    /// Tickets still waiting for the human's fill report, in ticket order.
    #[must_use]
    pub fn waiting(&self) -> &[TicketView] {
        &self.waiting
    }

    /// Flagged disagreements, oldest first.
    #[must_use]
    pub fn drifts(&self) -> &[TicketDriftView] {
        &self.drifts
    }

    /// Whether no ticket waits and nothing drifted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty() && self.drifts.is_empty()
    }
}

/// One operator decision on a manual ticket.
///
/// The action carries only the opaque document the gateway forwards (the
/// core alone parses it); applying it invokes exactly one gateway verb, so
/// a rendered click can never fan out into a second write.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One operator decision on a manual ticket.
/// 
/// The action carries only the opaque document the gateway forwards (the
/// core alone parses it); applying it invokes exactly one gateway verb, so
/// a rendered click can never fan out into a second write.
pub enum TicketAction {
    /// Issue a ticket the operator places by hand.
    Issue {
        /// Draft the core turns into the venue intent.
        ticket: TicketDraft,
    },
    /// Confirm a waiting ticket against the human's fill report.
    Confirm {
        /// Ticket to confirm.
        ticket_id: String,
        /// Opaque fill document (quantity plus average price).
        fill: serde_json::Value,
    },
    /// Apply a read-only import snapshot over the human-executed account.
    Import {
        /// Opaque position-list snapshot.
        snapshot: serde_json::Value,
    },
}

impl TicketAction {
    /// An issue click carrying the filled draft.
    #[must_use]
    pub fn issue(ticket: TicketDraft) -> Self {
        Self::Issue { ticket }
    }

    /// A confirm click for `ticket_id` carrying the fill report.
    #[must_use]
    pub fn confirm(ticket_id: &str, fill: serde_json::Value) -> Self {
        Self::Confirm {
            ticket_id: ticket_id.to_owned(),
            fill,
        }
    }

    /// An import click carrying the position snapshot.
    #[must_use]
    pub fn import(snapshot: serde_json::Value) -> Self {
        Self::Import { snapshot }
    }

    /// Invoke the one gateway verb this action names.
    pub fn apply(&self, gateway: &dyn TerminalGateway) -> Result<(), GatewayError> {
        match self {
            Self::Issue { ticket } => gateway.issue_ticket(ticket.clone()),
            Self::Confirm { ticket_id, fill } => gateway.confirm_ticket(ticket_id, fill.clone()),
            Self::Import { snapshot } => gateway.import_tickets(snapshot.clone()),
        }
    }
}