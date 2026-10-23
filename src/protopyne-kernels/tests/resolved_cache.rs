//! The per-state resolved-scalar cache.
//!
//! The cache exists to take the boundary `ParamSet` (a `HashMap` of
//! `PineValue`s) out of the per-bar loop. It must therefore be a **memo of a
//! pure value**, never a place a parameter change could be forgotten: one
//! resolution per state, a copy per read, nothing stored on refusal, a clone
//! that re-resolves, and a refusal when two kernels try to share one state.

use super::*;
use std::cell::Cell;

#[test]
fn resolves_once_then_reads_copies() {
    let mut state = IndicatorState::new();
    let builds = Cell::new(0);
    let value = state
        .resolved(|| {
            builds.set(builds.get() + 1);
            Ok(14_usize)
        })
        .expect("first resolution");
    assert_eq!(value, 14);
    assert_eq!(builds.get(), 1);
    // A second read never runs the builder, even a builder that would disagree.
    let value = state
        .resolved(|| {
            builds.set(builds.get() + 1);
            Ok(99_usize)
        })
        .expect("cached read");
    assert_eq!(value, 14, "the first resolution is the run's");
    assert_eq!(builds.get(), 1, "the builder ran exactly once");
}

#[test]
fn a_second_scalar_type_refuses() {
    let mut state = IndicatorState::new();
    let _: usize = state.resolved(|| Ok(7_usize)).expect("usize scalars");
    let refused = state
        .resolved(|| Ok((7_usize, 1.5_f64)))
        .expect_err("a different type in one state is a caller bug");
    assert!(
        refused.to_string().contains("reused across kernels"),
        "the refusal names the cause: {refused}"
    );
}

#[test]
fn a_failed_resolution_stores_nothing_and_retries() {
    let mut state = IndicatorState::new();
    let refused = state.resolved::<usize>(|| {
        Err(KernelError::InvalidParam(
            "length must be positive".to_owned(),
        ))
    });
    assert!(refused.is_err(), "the parameter refusal surfaces");
    let value = state
        .resolved(|| Ok(5_usize))
        .expect("a refused resolution caches nothing");
    assert_eq!(value, 5);
}

#[test]
fn a_cloned_state_re_resolves_on_its_own_first_bar() {
    let mut state = IndicatorState::new();
    let _: usize = state.resolved(|| Ok(14_usize)).expect("resolves");
    let mut cloned = state.clone();
    let value = cloned
        .resolved(|| Ok(3_usize))
        .expect("the clone resolves its own");
    assert_eq!(value, 3, "a clone is a fresh window, not a shared memo");
    let value = state
        .resolved(|| Ok(99_usize))
        .expect("the source keeps its own");
    assert_eq!(value, 14);
}
