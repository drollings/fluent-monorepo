//! The whole-tape read algorithm, rebuilt from public pieces.
//!
//! Included once from `tape.rs`; consumer suites `use` it, never
//! include it again. The oracle is whole-tape coverage plus plan plus
//! per-span fetches — what the read paths did before windowed stamp
//! planning — so the equivalence suites prove the windowed path serves
//! identical bars, spans, and refusals rather than merely plausible ones.

#![allow(dead_code)]

use protopyne_types::{Bar, Timeframe};

use crate::coverage::Coverage;
use crate::error::StoreError;
use crate::fold::{fold_bars, narrow_to_window};
use crate::key::SymbolId;
use crate::plan::{ReadPlan, wanted_window};
use crate::store::order_and_validate;
use crate::store::{CandleSeries, ReadRequest};

/// Serve `req` the old way: whole-tape `coverage`, resolved plan, one bar
/// fetch per planned span, double-validated with a fold and narrow exactly
/// as the former read paths did.
pub fn oracle_from_pieces(
    tape: Option<(SymbolId, Timeframe)>,
    coverage: Option<Coverage>,
    fetch: &dyn Fn(Option<i64>, Option<i64>) -> Result<Vec<Bar>, StoreError>,
    req: &ReadRequest,
) -> Result<CandleSeries, StoreError> {
    let plan = ReadPlan::resolve_for(tape, coverage, req)?;
    let served = wanted_window(req, Some(plan.covered()))?;
    let mut bars = Vec::new();
    for span in plan.spans() {
        bars.extend(fetch(span.span().start_ms, span.span().end_ms)?);
    }
    let bars = order_and_validate(bars)?;
    let bars = if req.timeframe == plan.key().timeframe {
        bars
    } else {
        narrow_to_window(fold_bars(&bars, req.timeframe)?, &served, req.timeframe)
    };
    let bars = order_and_validate(bars)?;
    Ok(CandleSeries {
        key: plan.key(),
        bars,
        served,
        provenance: plan.spans().to_vec(),
    })
}

/// Both outcomes agree: identical series on success, identical refusal
/// text on failure. Errors carry no `PartialEq`, so the debug rendering —
/// which names the refusal variant and span — is the comparison.
pub fn assert_same_outcome(
    actual: &Result<CandleSeries, StoreError>,
    expected: &Result<CandleSeries, StoreError>,
    what: &str,
) {
    match (actual, expected) {
        (Ok(actual), Ok(expected)) => assert_eq!(
            actual, expected,
            "served series, window, and provenance agree for {what}"
        ),
        (Err(actual), Err(expected)) => assert_eq!(
            format!("{actual:?}"),
            format!("{expected:?}"),
            "refusal variant and span agree for {what}"
        ),
        _ => panic!("outcome mismatch for {what}: {actual:?} vs {expected:?}"),
    }
}
