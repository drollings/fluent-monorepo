//! Lazy lemma-pipeline boundary tests (included via `#[path]` forwarder
//! from `src/query/ingest.rs`, per repo convention). The counting
//! factory stands in for the stock constructor: production call sites
//! pass need, tests count builds.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn counting_factory(
    builds: &AtomicUsize,
) -> impl FnOnce() -> Option<spacy_rs::pipeline::NlpPipeline> + '_ {
    || {
        builds.fetch_add(1, Ordering::SeqCst);
        None
    }
}

#[test]
fn lemma_need_constructs_exactly_once() {
    // The recall side asks on first lemma need and reuses after:
    // repeated need still builds once.
    let lazy = LazyNlp::new();
    let builds = AtomicUsize::new(0);
    let first = lazy.get_with(counting_factory(&builds));
    let second = lazy.get_with(counting_factory(&builds));
    assert_eq!(builds.load(Ordering::SeqCst), 1, "construct-once");
    assert_eq!(first.is_some(), second.is_some());
}

#[test]
fn failed_construction_is_cached_not_rebuilt() {
    // Absence is an answer too: a `None` build still counts once, so
    // the failure path can never turn into a rebuild storm.
    let lazy = LazyNlp::new();
    let builds = AtomicUsize::new(0);
    assert!(lazy.get_with(counting_factory(&builds)).is_none());
    assert!(lazy.get_with(counting_factory(&builds)).is_none());
    assert_eq!(builds.load(Ordering::SeqCst), 1);
}

#[test]
fn untouched_holder_builds_nothing() {
    // The holder is inert until asked: paths that never ask (rg,
    // fully-skipped syncs) can never pay construction through it.
    let lazy = LazyNlp::new();
    let builds = AtomicUsize::new(0);
    let factory = counting_factory(&builds);
    drop(factory);
    drop(lazy);
    assert_eq!(builds.load(Ordering::SeqCst), 0);
}
