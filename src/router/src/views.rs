//! M10.3 migration shim: the view mechanics (`Lod`, `LedgerView`,
//! `ParallelLedger`, `FilteredLedger`) live in `fluent_knowledge::views` —
//! this module re-exports them so existing `crate::views::…` paths keep
//! working. The PII frontier view stays here: its transform is the router's
//! write-path guard (`scrub_for_ledger`), which must not move (M0).

pub use fluent_knowledge::views::{FilteredLedger, LedgerView, Lod, ParallelLedger};

use std::collections::HashSet;

use fluent_types::NodeId;

use crate::ledger_guard;

/// The PII frontier view: a `FilteredLedger` whose transform scrubs every
/// rendered line through the write-path guard. One implementation
/// (`scrub_for_ledger`), two callers (DRY).
pub fn pii_redacted<V: LedgerView, S: std::hash::BuildHasher>(
    inner: V,
    excluded: HashSet<NodeId, S>,
) -> FilteredLedger<V> {
    FilteredLedger::new(inner, excluded)
        .with_transform(|_id, text| ledger_guard::scrub_for_ledger(&text).text)
}

#[cfg(test)]
#[path = "../tests/views.rs"]
mod tests;
