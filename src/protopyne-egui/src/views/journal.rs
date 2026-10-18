//! Proposal-provenance timeline: journal rows in causal order.
//!
//! Each row follows the previous row for the same client, so the journal's
//! own causal chains (signal → risk → grant → submission → fill per client)
//! need no second event model: rows sort by sequence number at build, the
//! timeline is that sorted order, and follow-ups are same-client suffixes.
//! This deliberately composes no graph: every causal edge points forward in
//! `seq`, so a topological sort over these edges reproduces the sorted
//! order exactly — paying graph construction to re-derive what the sort
//! already established. (Not a DAG-skill violation: the directive forbids
//! *re-implementing graph algorithms*; sorting a key-ordered list with the
//! standard sort and reading chain suffixes hand-rolls no topological
//! sort.) Rows sort by sequence number at build, so the timeline is
//! seq-causal, never arrival-causal.

use std::collections::HashMap;

use fluent_egui::EguiError;
use protopyne_view::JournalRowView;

/// Read-only provenance over journal rows, keyed by sequence number (this
/// prints as its rows and timeline, never its internals: no `Debug`).
pub struct Provenance {
    /// Journal rows in sequence order.
    rows: Vec<JournalRowView>,
    /// Sequence numbers in causal order.
    order: Vec<i64>,
    /// Sequence number to its position in `rows`.
    index: HashMap<i64, usize>,
    /// Client id to its sequence numbers in causal order.
    chains: HashMap<String, Vec<i64>>,
}

impl Provenance {
    /// Sort rows into sequence order and index them; refuses a journal with
    /// duplicate sequence numbers instead of substituting an order. Takes
    /// the rows by value so the sort runs in place — the sorted copy is
    /// the stored journal, never a discarded intermediate.
    pub fn of(mut rows: Vec<JournalRowView>) -> Result<Self, EguiError> {
        rows.sort_by_key(|row| row.seq);
        let mut index = HashMap::with_capacity(rows.len());
        let mut chains: HashMap<String, Vec<i64>> = HashMap::new();
        let mut order = Vec::with_capacity(rows.len());
        for (position, row) in rows.iter().enumerate() {
            if index.insert(row.seq, position).is_some() {
                return Err(EguiError::InvalidParam {
                    what: "journal".to_owned(),
                    reason: format!("duplicate sequence number {} has no causal order", row.seq),
                });
            }
            chains
                .entry(row.client_id.clone())
                .or_default()
                .push(row.seq);
            order.push(row.seq);
        }
        Ok(Self {
            rows,
            order,
            index,
            chains,
        })
    }

    /// Sequence numbers in causal order (matches journal order for a
    /// well-formed journal).
    #[must_use]
    pub fn timeline(&self) -> Vec<i64> {
        self.order.clone()
    }

    /// Sequence numbers that follow `seq` on its own causal chain; empty
    /// for unknown rows and for chain tails. The traceability primitive
    /// behind VISION §14 ("what followed this event"): every order traces
    /// to a proposal, a risk decision, and an approval through these
    /// chains. A chain is linear, so the transitive follow-ups are exactly
    /// the same-client suffix — no graph walk needed.
    #[must_use]
    pub fn followed_by(&self, seq: i64) -> Vec<i64> {
        let Some(position) = self.index.get(&seq) else {
            return Vec::new();
        };
        let Some(row) = self.rows.get(*position) else {
            return Vec::new();
        };
        let Some(chain) = self.chains.get(row.client_id.as_str()) else {
            return Vec::new();
        };
        match chain.iter().position(|member| *member == seq) {
            Some(at) => chain[at + 1..].to_vec(),
            // Unreachable: every indexed row was chained at build. A
            // missing entry fails closed instead of panicking.
            None => Vec::new(),
        }
    }

    /// The row behind a sequence number, if present.
    #[must_use]
    pub fn row(&self, seq: i64) -> Option<&JournalRowView> {
        self.index
            .get(&seq)
            .and_then(|position| self.rows.get(*position))
    }

    /// The journal in sequence order: the sorted rows this was built from,
    /// moved out so the sort result is stored, never thrown away.
    #[must_use]
    pub fn into_rows(self) -> Vec<JournalRowView> {
        self.rows
    }
}
