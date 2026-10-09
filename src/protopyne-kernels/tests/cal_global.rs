//! Global calibration gate: universal controls, frozen table, dashboard.
//!
//! Producer-confidence axis (controls): flat, null-injected, short, and
//! single-tick inputs must never leak `NaN`/`inf` into a `Some`, never shift
//! alignment, and must hold leading warmup `None`s — for every registry
//! kernel, with family-specific flat expectations living in the companion
//! suite. Task-value axis (dashboard): every kernel accepts its `Expr`
//! reference at its frozen matrix eps, with a 0.0 control false-fire rate.
//! Green is 1.0 accept plus 0.0 false-fire, mirroring the spike
//! `CalibrationTally` shape (accept 1.0 plus reject 1.0) one crate over.

use crate::calibration::{Control, ParityDashboard, run_control};
use crate::compat::{COMPATIBILITY, CompatStatus, assert_matrix_covers, compat_for};
use crate::indicator::builtin_registry;

#[test]
fn controls_hold_on_flat_null_short_and_single_tick() {
    for control in [Control::Flat, Control::NullInjected, Control::Short, Control::SingleTick] {
        let violations = run_control(control);
        assert!(
            violations.is_empty(),
            "{control:?} controls fired: {violations:#?}"
        );
    }
}

#[test]
fn null_injection_only_adds_nones_never_moves_values() {
    // Alignment contract: nulling bars may only turn defined outputs into
    // `None` (strict windows covering the gap, honest skips) — a value that
    // stays defined must be bit-identical to the clean run. Anything else is
    // a positional shift, the exact failure this control exists to catch.
    let diffs = crate::calibration::null_alignment_diffs();
    assert!(diffs.is_empty(), "null shifted values: {diffs:#?}");
}

#[test]
fn frozen_eps_table_never_widens() {
    // The promotion gate below locks (id, eps, status) exactly: any widening,
    // any added row, any silent demotion fails here first. Deliberate changes
    // update this table and the matrix together, with justification.
    let registry = builtin_registry().expect("builtins");
    assert_matrix_covers(&registry).expect("bijective");
    assert_eq!(COMPATIBILITY.len(), registry.ids().len(), "no orphans either way");
    for row in COMPATIBILITY {
        assert!(
            (row.eps - 1e-9).abs() < f64::EPSILON,
            "{:?} eps widened to {}",
            row.id,
            row.eps
        );
        assert!(
            matches!(row.status, CompatStatus::ParityVerified),
            "{:?} not promoted",
            row.id
        );
    }
    for id in registry.ids() {
        assert!(compat_for(&id).is_some(), "registry id {id:?} has no row");
    }
}

#[test]
fn dashboard_is_green_with_full_accept_and_zero_false_fire() {
    let dashboard = ParityDashboard::build().expect("dashboard builds");
    assert!(
        dashboard.rows.iter().all(|row| row.accepted),
        "must-fire accept < 1.0: {:#?}",
        dashboard.rows.iter().filter(|row| !row.accepted).collect::<Vec<_>>()
    );
    assert!(
        dashboard.rows.iter().all(|row| row.false_fires == 0),
        "must-NOT-fire controls tripped: {:#?}",
        dashboard.rows.iter().filter(|row| row.false_fires > 0).collect::<Vec<_>>()
    );
    assert!((dashboard.accept_rate() - 1.0).abs() < f64::EPSILON, "accept rate");
    assert!(dashboard.false_fire_rate() == 0.0, "false-fire rate");
    assert!(dashboard.is_green(), "dashboard green");
}
