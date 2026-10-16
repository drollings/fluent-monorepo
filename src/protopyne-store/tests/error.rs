//! Store error taxonomy: the three-way distinction callers act on must not
//! collapse into one string.

use fluent_wvr::WorkError;

use super::*;

#[test]
fn only_contention_is_retryable() {
    let busy = StoreError::Busy("store is busy".to_owned());
    assert!(busy.is_retryable(), "contention is worth trying again");

    for permanent in [
        StoreError::InvalidBar("high below open".to_owned()),
        StoreError::Capacity("tape SPY M1 holds 4 bars: projected 5 exceeds max 4".to_owned()),
        StoreError::Plan(PlanError::BlankSymbol),
        StoreError::Snapshot(SnapshotError::Inconsistent {
            reason: "high below open".to_owned(),
        }),
        StoreError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "no such tape").into()),
    ] {
        assert!(
            !permanent.is_retryable(),
            "{permanent} is a fact about the world: a retry reproduces it"
        );
    }
}

#[test]
fn a_store_failure_bridges_into_supervision_as_a_permanent_execution_error() {
    let err = StoreError::Plan(PlanError::UnboundedUnlimited);
    let work: WorkError = err.into();
    assert_eq!(
        work,
        WorkError::Execution(
            "read plan refused: unbounded read with no bar limit: refusing to scan the whole tape"
                .to_owned()
        ),
        "supervision sees the full reason, not a category name"
    );
    assert!(
        !work.is_retryable(),
        "an execution failure is permanent to the supervisor"
    );
}

#[test]
fn a_plan_refusal_states_what_it_refused() {
    assert_eq!(
        PlanError::BlankSymbol.to_string(),
        "symbol must be nonempty"
    );
    assert_eq!(
        PlanError::UnknownSymbol("ZZZZ".to_owned()).to_string(),
        "unknown symbol: ZZZZ"
    );
    assert_eq!(
        PlanError::InvertedRange { start: 9, end: 4 }.to_string(),
        "inverted range: start 9 is not before end 4"
    );
    assert_eq!(
        PlanError::UnsupportedTimeframe {
            symbol: "SPY".to_owned(),
            timeframe: Timeframe::M3,
        }
        .to_string(),
        "unsupported timeframe M3 for SPY"
    );
}

#[test]
fn a_snapshot_refusal_names_both_sides_of_the_conflict() {
    assert_eq!(
        SnapshotError::KeyConflict {
            held: "2026-01-05T20:15:00+00:00".to_owned(),
            newer: "2026-01-05T20:20:00+00:00".to_owned(),
        }
        .to_string(),
        "key conflict: 2026-01-05T20:15:00+00:00 then 2026-01-05T20:20:00+00:00"
    );
    assert_eq!(
        SnapshotError::Inconsistent {
            reason: "invalid bar: high (99) is less than open (100)".to_owned(),
        }
        .to_string(),
        "inconsistent snapshot: invalid bar: high (99) is less than open (100)"
    );
}

#[test]
fn an_io_error_lands_in_the_io_variant_by_the_shared_macro() {
    let err: StoreError = std::io::Error::new(std::io::ErrorKind::NotFound, "no such tape").into();
    assert!(
        matches!(err, StoreError::Io(_)),
        "standard error wiring, not a hand-rolled conversion"
    );
}

#[test]
fn plan_errors_compare_by_value_so_a_resolution_table_can_be_asserted() {
    // `PartialEq` is what lets a table test assert the refusal directly
    // instead of matching on its rendered string.
    assert_eq!(PlanError::BlankSymbol, PlanError::BlankSymbol);
    assert_ne!(PlanError::BlankSymbol, PlanError::UnboundedUnlimited);
}
