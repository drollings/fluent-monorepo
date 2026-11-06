//! Infallible ordered-`Option` ladder characterization.

use fluent_wvr::ladder::first_some_in_order_sync;
use std::cell::RefCell;

#[test]
fn no_rungs_returns_none() {
    let rungs: Vec<u32> = Vec::new();
    assert_eq!(first_some_in_order_sync(rungs, Some), None);
}

#[test]
fn all_declines_returns_none() {
    assert_eq!(first_some_in_order_sync([1u32, 2, 3], |_| None::<u32>), None);
}

#[test]
fn first_rung_wins() {
    assert_eq!(
        first_some_in_order_sync([1u32, 2, 3], |r| (r == 1).then_some("first")),
        Some("first")
    );
}

#[test]
fn later_rung_wins() {
    assert_eq!(
        first_some_in_order_sync([1u32, 2, 3], |r| (r == 3).then_some("last")),
        Some("last")
    );
}

#[test]
fn duplicates_choose_the_first() {
    let seen = RefCell::new(Vec::new());
    let out = first_some_in_order_sync([7u32, 7, 7], |r| {
        seen.borrow_mut().push(r);
        Some(r)
    });
    assert_eq!(out, Some(7));
    assert_eq!(*seen.borrow(), vec![7]);
}

#[test]
fn no_rung_runs_after_acceptance() {
    let seen = RefCell::new(Vec::new());
    let out = first_some_in_order_sync([1u32, 2, 3, 4], |r| {
        seen.borrow_mut().push(r);
        (r == 2).then_some(r * 10)
    });
    assert_eq!(out, Some(20));
    assert_eq!(*seen.borrow(), vec![1, 2]);
}

#[test]
fn rungs_are_owned_and_passed_by_value() {
    let out = first_some_in_order_sync([vec![1u32], vec![2u32]], |mut r| {
        r.push(9);
        Some(r)
    });
    assert_eq!(out, Some(vec![1, 9]));
}
