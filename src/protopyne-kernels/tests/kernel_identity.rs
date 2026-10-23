//! All-kernel identity fold: `batch` and the incremental `update_full` path
//! agree position by position — primary and every declared secondary — for
//! every registered kernel, under its own schema's default parameters.
//!
//! The case list is **derived**, never hand-copied: registry ids × each
//! kernel's own declared outputs × default params × a fixture window, so a new
//! kernel joins the fold the moment it registers. Two controls guard the fold
//! itself rather than the kernels: the coverage assertion in the schema suite
//! (every id has a case; every declared secondary appears exactly once) and the
//! mutation control below (swapping `high`/`low` must change at least one
//! kernel, or the fixture is degenerate and the fold is lying).

use crate::indicator::{
    bits_of, builtin_registry, BarTick, Indicator, IndicatorParam, IndicatorResult, IndicatorState,
    OutputRole, OwnedOhlcv, ParamKind, ParamSet,
};
use crate::KernelError;

/// Bars per fixture window: comfortably past every default warmup (the longest
/// shipped default lookback is 200) so the fold also covers the warm kernels.
const BARS: usize = 240;

/// One row of the fold: the registry id, its default parameters, the window it
/// is driven over, and the secondary outputs its own schema declares.
pub(crate) struct KernelCase {
    /// Registry id.
    pub(crate) id: String,
    /// The kernel's own default parameters.
    pub(crate) params: ParamSet,
    /// The distinguishable-leg window the case is driven over.
    pub(crate) data: OwnedOhlcv,
    /// Declared secondary outputs, in schema order.
    pub(crate) secondary: Vec<&'static str>,
}

/// Every registered kernel as one fold case: no corpus to drift from the
/// registry, because there is no corpus — the registry and each kernel's own
/// schema are the source.
pub(crate) fn all_cases() -> Vec<KernelCase> {
    let registry = builtin_registry().expect("registry builds");
    registry
        .ids()
        .into_iter()
        .map(|id| {
            let indicator = registry.get(&id).expect("registered");
            let spec = indicator.spec();
            KernelCase {
                params: spec.default_params(),
                data: fixture_data(BARS),
                secondary: spec
                    .outputs()
                    .iter()
                    .filter(|output| output.role == OutputRole::Secondary)
                    .map(|output| output.name)
                    .collect(),
                id,
            }
        })
        .collect()
}

/// A window whose five legs are distinguishable and ordered: open and close
/// walk different paths, high clears both and low undercuts both, and volume is
/// its own series. A kernel that confuses two legs cannot agree with itself by
/// symmetry over this window.
fn fixture_data(bars: usize) -> OwnedOhlcv {
    let mut data = OwnedOhlcv::default();
    for index in 0..bars {
        let step = index as f64;
        let open = 100.0 + (step * 0.37).sin() * 2.0 + step * 0.01;
        let close = 100.0 + (step * 0.23).cos() * 1.5 + step * 0.02;
        let high = open.max(close) + 1.0 + (step * 0.11).sin().abs();
        let low = open.min(close) - 1.0 - (step * 0.17).cos().abs();
        data.open.push(open);
        data.high.push(high);
        data.low.push(low);
        data.close.push(close);
        data.volume
            .push(1_000.0 + step * 3.0 + (step * 0.7).sin() * 15.0);
    }
    data
}

/// The window shapes every case is driven over: the distinguishable-leg window,
/// a flat line (the seed recurrences must settle exactly), and subnormal
/// magnitudes (underflow behaviour must agree exactly). The two degenerate
/// shapes are what the per-family scratch pins used to carry one kernel at a
/// time; here every kernel gets all three.
fn shapes(data: &OwnedOhlcv) -> Vec<(&'static str, OwnedOhlcv)> {
    let constant = OwnedOhlcv {
        open: vec![42.0; data.len()],
        high: vec![42.0; data.len()],
        low: vec![42.0; data.len()],
        close: vec![42.0; data.len()],
        volume: vec![7.0; data.len()],
    };
    let subnormal_leg = |scale: f64| -> Vec<f64> {
        (1..=data.len())
            .map(|index| index as f64 * 1e-310 * scale)
            .collect()
    };
    let subnormal = OwnedOhlcv {
        open: subnormal_leg(1.0),
        high: subnormal_leg(1.1),
        low: subnormal_leg(0.9),
        close: subnormal_leg(1.0),
        volume: vec![1.0; data.len()],
    };
    vec![
        ("distinguishable", data.clone()),
        ("constant", constant),
        ("subnormal", subnormal),
    ]
}

/// Run `batch` and the `update_full` fold over `data` and describe every
/// disagreement — primary and each declared secondary, bit for bit.
///
/// A refusal counts as agreement only when both paths refuse with the same
/// reason: a parameter the batch path rejects must not slip through the tick
/// path (or the reverse) just because it sits at a declared bound.
fn fold_agrees(
    indicator: &dyn Indicator,
    data: &OwnedOhlcv,
    params: &ParamSet,
    secondary: &[&'static str],
) -> Result<(), String> {
    let batch: IndicatorResult = match indicator.batch(&data.views(), params) {
        Ok(result) => result,
        Err(batch_error) => {
            let mut state = IndicatorState::new();
            return match indicator.update_full(&mut state, bar_of(data, 0), params) {
                Err(update_error) if update_error.to_string() == batch_error.to_string() => Ok(()),
                Err(update_error) => Err(format!(
                    "batch refused `{batch_error}` but update refused `{update_error}`"
                )),
                Ok(_) => Err(format!("batch refused `{batch_error}` but update accepted")),
            };
        }
    };
    if batch.values.len() != data.len() {
        return Err(format!(
            "primary length {} != {} bars",
            batch.values.len(),
            data.len()
        ));
    }
    let mut state = IndicatorState::new();
    let mut primary = Vec::with_capacity(data.len());
    let mut folded: Vec<Vec<Option<f64>>> = secondary
        .iter()
        .map(|_| Vec::with_capacity(data.len()))
        .collect();
    for index in 0..data.len() {
        let full = indicator
            .update_full(&mut state, bar_of(data, index), params)
            .map_err(|error| format!("update refused at bar {index}: {error}"))?;
        primary.push(full.primary);
        for (slot, name) in folded.iter_mut().zip(secondary) {
            slot.push(
                full.secondary
                    .iter()
                    .find(|value| value.name == *name)
                    .and_then(|value| value.value),
            );
        }
    }
    if bits_of(&primary) != bits_of(&batch.values) {
        return Err("primary batch != update".to_owned());
    }
    for (slot, name) in folded.iter().zip(secondary) {
        let declared = batch
            .secondary(name)
            .ok_or_else(|| format!("declared output {name} missing from the batch result"))?;
        if bits_of(slot) != bits_of(declared) {
            return Err(format!("secondary {name} batch != update"));
        }
    }
    Ok(())
}

/// One bar of the fixture window.
fn bar_of(data: &OwnedOhlcv, index: usize) -> BarTick {
    BarTick {
        open: data.open[index],
        high: data.high[index],
        low: data.low[index],
        close: data.close[index],
        volume: data.volume[index],
    }
}

/// The fold: for every case and every shape, the batch path and the tick path
/// agree bit for bit, primary and every declared secondary.
#[test]
fn batch_equals_update_full_for_every_declared_output() {
    let registry = builtin_registry().expect("registry builds");
    for case in all_cases() {
        let indicator = registry.get(&case.id).expect("registered");
        for (shape, data) in shapes(&case.data) {
            fold_agrees(&**indicator, &data, &case.params, &case.secondary)
                .unwrap_or_else(|drift| panic!("{} / {shape}: {drift}", case.id));
        }
    }
}

/// The property the per-state scalar cache rests on: a state resolves its
/// scalars from the first bar's parameters and keeps them for the run, so a
/// parameter at any **declared bound** must behave identically through both
/// paths — the batch call (which resolves once per call) and the fold (which
/// resolves once per state). Every default was covered above; this covers the
/// edges of every declared parameter.
#[test]
fn boundary_parameters_keep_batch_and_update_identical() {
    let registry = builtin_registry().expect("registry builds");
    let mut checked = 0_usize;
    for case in all_cases() {
        let indicator = registry.get(&case.id).expect("registered");
        for param in indicator.spec().params() {
            for bound in [param.min, param.max].into_iter().flatten() {
                let params = at_bound(&case.params, param, bound);
                fold_agrees(&**indicator, &case.data, &params, &case.secondary).unwrap_or_else(
                    |drift| panic!("{} @ {}={bound}: {drift}", case.id, param.name),
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked > 0,
        "no declared parameter carries a bound: the boundary fold checked nothing"
    );
}

/// `params` with one declared parameter pinned to `bound`, keeping its kind.
fn at_bound(params: &ParamSet, param: &IndicatorParam, bound: f64) -> ParamSet {
    match param.kind {
        ParamKind::Int => params.clone().with_int(param.name, bound as i64),
        ParamKind::Float => params.clone().with_float(param.name, bound),
        ParamKind::Bool => params.clone().with_bool(param.name, bound != 0.0),
    }
}

/// The mutation control: the fixture must be able to fail. Swapping the high
/// and low legs changes at least one kernel's output; a fixture that cannot
/// move is degenerate, and every agreement above would be vacuous.
#[test]
fn swapping_high_and_low_changes_at_least_one_kernel() {
    let registry = builtin_registry().expect("registry builds");
    let mut moved = Vec::new();
    for case in all_cases() {
        let indicator = registry.get(&case.id).expect("registered");
        let mut swapped = case.data.clone();
        std::mem::swap(&mut swapped.high, &mut swapped.low);
        let clean = indicator
            .batch(&case.data.views(), &case.params)
            .unwrap_or_else(|error| panic!("{}: batch refused: {error}", case.id));
        let flipped = indicator
            .batch(&swapped.views(), &case.params)
            .unwrap_or_else(|error| panic!("{}: swapped batch refused: {error}", case.id));
        if bits_of(&clean.values) != bits_of(&flipped.values) {
            moved.push(case.id.clone());
        }
    }
    assert!(
        !moved.is_empty(),
        "the fixture is degenerate: swapping high/low changed no kernel, so \
         every batch/update agreement above proves nothing"
    );
}

/// The refusal path is not part of the fold, but a case must at least be
/// buildable: a registry id with no schema is a registration bug, and the fold
/// would silently skip it.
#[test]
fn every_case_has_a_schema_and_default_parameters() {
    let registry = builtin_registry().expect("registry builds");
    for case in all_cases() {
        let indicator = registry.get(&case.id).expect("registered");
        let spec = indicator.spec();
        assert!(!spec.outputs().is_empty(), "{}: no outputs", case.id);
        assert!(
            !matches!(
                indicator.batch(&case.data.views(), &case.params),
                Err(KernelError::InvalidParam(_))
            ),
            "{}: defaults refused as invalid",
            case.id
        );
    }
}
