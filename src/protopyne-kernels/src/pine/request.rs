//! Headless request datasets: the run-scoped view of every injected tape.
//!
//! `request.security` and `request.security_lower_tf` never fetch: they
//! serve exactly the bars an operator injected for one `(symbol, timeframe)`
//! pair, and refuse naming the live fetch when no pair matches. The engine
//! owns the registrations across runs (they are harness configuration, not
//! run state); this owner is the per-run snapshot both engines read, so a
//! run sees one set of tapes and two runs can never observe each other's.
//!
//! The window rule, the tail walk, and the refusal wording stay with the
//! request evaluation in [`super::eval`]; this module owns only the
//! registrations and their lookup.

use super::eval::InjectedDataset;
use crate::LtfDataset;
use std::sync::Arc;

/// Every injected request tape a run may serve.
#[derive(Debug, Default)]
/// Every injected request tape a run may serve.
pub(crate) struct RequestState {
    /// `request.security` tapes.
    security: Vec<InjectedDataset>,
    /// `request.security_lower_tf` tapes.
    lower_tf: Vec<InjectedDataset>,
}

impl RequestState {
    /// Snapshot the engine's registrations for one run. The datasets are
    /// shared by handle, so this copies two small vectors, never a tape.
    #[must_use]
    pub(crate) fn new(security: &[InjectedDataset], lower_tf: &[InjectedDataset]) -> Self {
        Self {
            security: security.to_vec(),
            lower_tf: lower_tf.to_vec(),
        }
    }

    /// The `request.security` tape registered for one pair, if any.
    #[must_use]
    pub(crate) fn security(&self, symbol: &str, timeframe: &str) -> Option<Arc<LtfDataset>> {
        lookup(&self.security, symbol, timeframe)
    }

    /// The `request.security_lower_tf` tape registered for one pair, if any.
    #[must_use]
    pub(crate) fn lower_tf(&self, symbol: &str, timeframe: &str) -> Option<Arc<LtfDataset>> {
        lookup(&self.lower_tf, symbol, timeframe)
    }

    #[cfg(feature = "bytecode")]
    /// True when no tape of either kind is registered: such a run pays no
    /// request store at all and every `request.*` call refuses the fetch.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.security.is_empty() && self.lower_tf.is_empty()
    }

    #[cfg(feature = "bytecode")]
    /// The registered `request.security` tapes.
    #[must_use]
    pub(crate) fn security_tapes(&self) -> &[InjectedDataset] {
        &self.security
    }

    #[cfg(feature = "bytecode")]
    /// The registered `request.security_lower_tf` tapes.
    #[must_use]
    pub(crate) fn lower_tf_tapes(&self) -> &[InjectedDataset] {
        &self.lower_tf
    }

    /// Whether any lower-timeframe tape is registered. Native serving
    /// declines when one is: an injected pull changes the semantics a
    /// resident native call would otherwise assume.
    #[must_use]
    pub(crate) fn has_lower_tf(&self) -> bool {
        !self.lower_tf.is_empty()
    }
}

/// One registered pair's tape, shared by handle.
fn lookup(slots: &[InjectedDataset], symbol: &str, timeframe: &str) -> Option<Arc<LtfDataset>> {
    slots
        .iter()
        .find(|slot| slot.symbol == symbol && slot.timeframe == timeframe)
        .map(|slot| Arc::clone(&slot.dataset))
}