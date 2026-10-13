//! Research-only expression fixtures plus tolerance gating for Expr-parity work.
//!
//! [`expr_fixture`] evaluates one caller-supplied `polars::Expr` over the
//! research frame of a bar slice ([`frame_of_bars`](crate::research::frame_of_bars))
//! and returns its single output column as a null-aware series.
//! [`assert_expr_parity`] funnels the comparison through
//! [`compare_series`](common_core::golden::compare_series) so every mismatch
//! names its index, both sides, and the maximum drift.
//!
//! This module never runs in the per-bar hot loop: it allocates columnar
//! frames, exactly like the research helpers it composes. Hand-rolled `f64`
//! recurrences stay the numeric path for `batch`/`update`; expressions here
//! are the independent `Expr` reference the kernels are Expr-parity-checked
//! against.

use polars::prelude::*;

use common_core::golden::compare_series;

use crate::error::KernelError;
use crate::indicator::fin_or_none;
use crate::research::frame_of_bars;

/// Evaluate one `polars::Expr` over the research frame of `bars`.
///
/// The frame carries the `ts`, `open`, `high`, `low`, `close`, and `volume`
/// columns; the expression selects exactly one output column. The result is
/// null-aware: polars nulls become `None`, and `NaN` (Pine `na`) maps to
/// `None` as well, so a null close never leaks as `Some(NaN)` downstream.
/// Output length always equals the input length.
pub fn expr_fixture(bars: &[protopyne_types::Bar], expr: Expr) -> Result<Vec<Option<f64>>, KernelError> {
    let frame = frame_of_bars(bars)?;
    let selected = frame
        .lazy()
        .select([expr])
        .collect()
        .map_err(|e| KernelError::Research(format!("expr_fixture select: {e}")))?;
    if selected.height() != bars.len() {
        return Err(KernelError::Research(format!(
            "expr_fixture: selected height {} != bars {}",
            selected.height(),
            bars.len()
        )));
    }
    let columns = selected.get_columns();
    let first = columns.first().ok_or_else(|| {
        KernelError::Research("expr_fixture: select produced no columns".to_owned())
    })?;
    let cast = first.cast(&DataType::Float64).map_err(|e| {
        KernelError::Research(format!("expr_fixture cast to f64: {e}"))
    })?;
    let chunked = cast.f64().map_err(|e| {
        KernelError::Research(format!("expr_fixture f64 view: {e}"))
    })?;
    Ok((0..chunked.len())
        .map(|i| chunked.get(i).and_then(fin_or_none))
        .collect())
}

/// Compare `actual` against an expression-fixture `expected` series at
/// absolute tolerance `eps`.
///
/// `Ok` on full agreement (including `None`-vs-`None` warmup/`na`
/// agreement); `Err` with one message naming every mismatch (index, both
/// sides) plus the maximum drift, so a planted drift reports its true index
/// instead of failing on the first element.
pub fn assert_expr_parity(
    actual: &[Option<f64>],
    expected: &[Option<f64>],
    eps: f64,
    context: &str,
) -> Result<(), String> {
    let report = compare_series(actual, expected, eps);
    if report.matches {
        return Ok(());
    }
    let mut message = format!(
        "{context}: {} mismatch(es) over {} compared, max_abs_diff {:.6}",
        report.mismatches.len(),
        report.compared,
        report.max_abs_diff
    );
    for mismatch in &report.mismatches {
        use std::fmt::Write as _;
        write!(
            message,
            "; position {}: actual {:?} vs expected {:?} ({:?})",
            mismatch.index, mismatch.actual, mismatch.expected, mismatch.kind
        )
        .expect("message append never fails");
    }
    Err(message)
}

#[cfg(test)]
#[path = "../tests/parity.rs"]
mod tests;
