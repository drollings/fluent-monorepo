//! First-accept-wins combinator — canonical home moved to `fluent_wvr::ladder`.
//!
//! This module is a delegation shim: the single implementation lives in
//! `fluent-wvr` (so `ComponentCascade` can compose the sync walk without a
//! dependency cycle), and every existing `fluent_concurrency::ladder::…`
//! import keeps resolving here. Do not add a second walker.
pub use fluent_wvr::ladder::{
    first_accept_in_order, first_accept_in_order_sync, first_some_in_order_sync,
};

#[cfg(test)]
#[path = "../tests/ladder.rs"]
mod tests;
