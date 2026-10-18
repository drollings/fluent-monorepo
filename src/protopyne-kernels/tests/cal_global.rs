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

use crate::calibration::{run_control, Control, ParityDashboard};
use crate::compat::{
    assert_matrix_covers, compat_for, CompatStatus, COMPATIBILITY, NATIVE_MATRIX_ROWS,
};
use crate::indicator::{
    builtin_registry, BarTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv,
    ParamSet,
};
use crate::KernelError;
use std::sync::Arc;

#[test]
fn controls_hold_on_flat_null_short_and_single_tick() {
    for control in [
        Control::Flat,
        Control::NullInjected,
        Control::Short,
        Control::SingleTick,
    ] {
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
    // Recurrence kernels (state feedback, not staged windows) admit no
    // staged-expression reference yet: they ride as an explicit pending set
    // the calibration milestone must empty, never as silent accepts.
    let registry = builtin_registry().expect("builtins");
    assert_matrix_covers(&registry).expect("bijective");
    // Kernels plus native-library rows, no orphans either way: the two
    // `native-*` rows joined the matrix at the cutover (parity freeze
    // green), so the length lock counts both domains explicitly.
    assert_eq!(
        COMPATIBILITY.len(),
        registry.ids().len() + NATIVE_MATRIX_ROWS.len(),
        "no orphans either way"
    );
    let mut pending = Vec::new();
    for row in COMPATIBILITY {
        assert!(
            (row.eps - 1e-9).abs() < f64::EPSILON,
            "{:?} eps widened to {}",
            row.id,
            row.eps
        );
        match row.status {
            CompatStatus::ParityVerified => {}
            CompatStatus::FormulaVerified(_) => pending.push(row.id),
        }
    }
    assert_eq!(
        pending,
        [
            "eit",
            "kalman",
            "voss",
            "adx",
            "cmf",
            "donchian",
            "ichimoku",
            "sar",
            "supertrend"
        ],
        "pending set is exact: additions promote or extend this list deliberately"
    );
    for id in registry.ids() {
        assert!(compat_for(&id).is_some(), "registry id {id:?} has no row");
    }
}

#[test]
fn dashboard_is_green_with_full_accept_and_zero_false_fire() {
    // Pending recurrence rows ride along (never accepted, never trusted for
    // money) while their staged references do not exist; everything else
    // must accept with zero false fires. The calibration milestone empties
    // the pending set — `is_green` stays the full-trust signal and is not
    // asserted while anything pends.
    let dashboard = ParityDashboard::build().expect("dashboard builds");
    let pending = [
        "eit",
        "kalman",
        "voss",
        "adx",
        "cmf",
        "donchian",
        "ichimoku",
        "sar",
        "supertrend",
    ];
    for row in &dashboard.rows {
        if pending.contains(&row.id.as_str()) {
            assert!(
                !row.accepted,
                "{} pending row must not read accepted",
                row.id
            );
            continue;
        }
        assert!(
            row.accepted,
            "{} must-fire accept < 1.0: {:?}",
            row.id, row.note
        );
    }
    assert!(
        dashboard.rows.iter().all(|row| row.false_fires == 0),
        "must-NOT-fire controls tripped: {:#?}",
        dashboard
            .rows
            .iter()
            .filter(|row| row.false_fires > 0)
            .collect::<Vec<_>>()
    );
    assert!(
        !dashboard.is_green(),
        "pending rows must hold full trust back"
    );
}

/// Fixture kernel with no compatibility-matrix row: the dashboard must report
/// the gap, never panic, so a forgotten matrix row surfaces as a red board.
struct Uncalibrated {
    spec: IndicatorSpec,
}

impl Uncalibrated {
    fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id("fixture-uncalibrated".to_owned())
                .display_name("fixture-uncalibrated".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }
}

impl Indicator for Uncalibrated {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        Ok(IndicatorResult::new(
            self.spec.id.clone(),
            data.close.iter().map(|&c| Some(c)).collect(),
        ))
    }

    fn update(
        &self,
        _state: &mut IndicatorState,
        bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(Some(bar.close))
    }
}

#[test]
fn a_kernel_without_a_matrix_row_is_reported_not_panicked() {
    // The gate must report a regression, not panic on it: a registered kernel
    // that no matrix row covers lands as a refusal row (the dashboard's
    // existing shape) and holds full trust back.
    let mut registry = builtin_registry().expect("builtins");
    registry
        .register(Arc::new(Uncalibrated::new()))
        .expect("fixture registers");
    let dashboard =
        ParityDashboard::build_with(&registry).expect("dashboard builds without panicking");
    let row = dashboard
        .rows
        .iter()
        .find(|row| row.id == "fixture-uncalibrated")
        .expect("the uncalibrated kernel is reported");
    assert!(
        !row.accepted,
        "a kernel without a matrix row must not read accepted"
    );
    assert!(
        row.note
            .as_deref()
            .is_some_and(|note| note.contains("matrix row")),
        "the row names the missing matrix row: {:?}",
        row.note
    );
    assert!(!dashboard.is_green(), "a missing row holds full trust back");
}
