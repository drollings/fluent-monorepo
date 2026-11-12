//! The chained kernel shell: one child kernel applied pass after pass.
//!
//! `trima`, `dema`, and `tema` are the same shape — run a child kernel over the
//! input, feed its output series back in as the next pass's input, then combine
//! the pass outputs into one number. The child is shared (`sma` or `ema`), the
//! seeding and `na` shape are the child's own, and the only per-kernel parts are:
//! how many passes, the child parameters of each pass, the warmup, and the
//! combine formula.
//!
//! [`Chained`] keeps exactly those parts. [`batch`] runs **whole child batches**
//! (series, one pass after another) and [`update`] runs **per bar** (child
//! states), so neither path is reimplemented as the other: the batch stays
//! series-shaped and the incremental path stays stateful, and the all-kernel fold
//! pins that they agree bit for bit.
//!
//! Kernels that are not a chain stay where they are: a parallel-leg composition
//! (`hma`'s two WMAs over the same input, squeezed, then a third pass), a
//! multi-output kernel (`macd`'s line/signal/histogram), or a kernel whose child
//! reads high/low rather than closes (`adosc`'s AD line) each need a second data
//! flow, and forcing them through a chain would either change their arithmetic or
//! hide it behind an untyped bag.

use crate::indicator::{
    nan_hole, BarTick, Indicator, IndicatorId, IndicatorResult, IndicatorState, Ohlcv, ParamSet,
};
use crate::KernelError;

/// Most passes a chain may run: the shipped chains are two or three passes, and
/// the combine step reads its inputs from a fixed stack array.
pub const MAX_CHAIN_PASSES: usize = 3;

/// A kernel that is a chain of one child kernel over each pass's output.
pub trait Chained: Send + Sync {
    /// The child kernel. Stateless, so one process-wide instance serves every
    /// composed kernel that uses it.
    type Child: Indicator + 'static;

    /// Parameters resolved once per call (a `usize`, or a small copyable tuple).
    type Shape: Copy + Send + Sync + 'static;

    /// Static description of this indicator.
    fn spec(&self) -> &crate::indicator::IndicatorSpec;

    /// Resolve the kernel's scalars from the boundary parameters.
    fn shape(&self, params: &ParamSet) -> Result<Self::Shape, KernelError>;

    /// Number of passes, at most [`MAX_CHAIN_PASSES`].
    fn passes(shape: Self::Shape) -> usize;

    /// The child's parameters for one pass.
    fn pass_params(shape: Self::Shape, pass: usize) -> ParamSet;

    /// Bars of history before the first output for these parameters.
    fn warmup(shape: Self::Shape) -> usize;

    /// Combine the pass outputs for one bar (`None` is `na`).
    fn combine(passes: &[Option<f64>]) -> Option<f64>;

    /// How many *leading* passes read the frame (and are therefore declared as
    /// shared-series edges). One for a plain cascade; a kernel whose later pass
    /// also reads frame legs when co-requested declares it too.
    fn frame_passes(_shape: Self::Shape) -> usize {
        1
    }

    /// The shared child instance.
    fn child() -> &'static Self::Child;

    /// The child's registry id, for the declared dependency edge.
    fn child_id() -> &'static str;
}

/// `warmup` for a [`Chained`] kernel.
///
/// # Errors
///
/// The kernel's own parameter refusal.
pub fn warmup<T: Chained>(kernel: &T, params: &ParamSet) -> Result<usize, KernelError> {
    Ok(T::warmup(kernel.shape(params)?))
}

/// `batch` for a [`Chained`] kernel: child batches pass after pass, then the
/// combine over each index.
///
/// # Errors
///
/// Any pass's refusal (the child's own validation on its parameters).
pub fn batch<T: Chained>(
    kernel: &T,
    data: &Ohlcv<'_>,
    params: &ParamSet,
) -> Result<IndicatorResult, KernelError> {
    let shape = kernel.shape(params)?;
    let passes = T::passes(shape);
    debug_assert!(
        passes <= MAX_CHAIN_PASSES,
        "a chain runs at most {MAX_CHAIN_PASSES} passes"
    );
    let child = T::child();
    let mut input = data.close.to_vec();
    let mut series: Vec<Vec<Option<f64>>> = Vec::with_capacity(passes);
    for pass in 0..passes {
        let result = child.batch(&Ohlcv::close_only(&input), &T::pass_params(shape, pass))?;
        input = result.values.iter().map(|value| nan_hole(*value)).collect();
        series.push(result.values);
    }
    let mut values = Vec::with_capacity(data.len());
    let mut outputs = [None; MAX_CHAIN_PASSES];
    for index in 0..data.len() {
        for (slot, pass) in series.iter().enumerate() {
            outputs[slot] = pass[index];
        }
        values.push(T::combine(&outputs[..passes]));
    }
    Ok(IndicatorResult::new(kernel.spec().id.clone(), values))
}

/// `update` for a [`Chained`] kernel: child states pass after pass, one bar.
///
/// # Errors
///
/// Any pass's refusal, or a parameter refusal.
pub fn update<T: Chained>(
    kernel: &T,
    state: &mut IndicatorState,
    bar: BarTick,
    params: &ParamSet,
) -> Result<Option<f64>, KernelError> {
    let shape = state.resolved(|| kernel.shape(params))?;
    let passes = T::passes(shape);
    debug_assert!(
        passes <= MAX_CHAIN_PASSES,
        "a chain runs at most {MAX_CHAIN_PASSES} passes"
    );
    let child = T::child();
    let mut outputs = [None; MAX_CHAIN_PASSES];
    for pass in 0..passes {
        let tick = if pass == 0 {
            bar
        } else {
            BarTick::close_only(nan_hole(outputs[pass - 1]))
        };
        let (child_state, child_params) =
            state.child_with_params(pass, || T::pass_params(shape, pass));
        outputs[pass] = child.update(child_state, tick, child_params)?;
    }
    let output = T::combine(&outputs[..passes]);
    state.set_last(nan_hole(output));
    Ok(output)
}

/// `deps_with_params` for a [`Chained`] kernel: the passes that read the frame
/// are declared (see [`Chained::frame_passes`]); passes over intermediate
/// series stay inline.
#[must_use]
/// `deps_with_params` for a [`Chained`] kernel: the passes that read the frame
/// are declared (see [`Chained::frame_passes`]); passes over intermediate
/// series stay inline.
pub fn deps_with_params<T: Chained>(kernel: &T, params: &ParamSet) -> Vec<(IndicatorId, ParamSet)> {
    match kernel.shape(params) {
        Ok(shape) => (0..T::frame_passes(shape))
            .map(|pass| (T::child_id().to_owned(), T::pass_params(shape, pass)))
            .collect(),
        Err(_) => Vec::new(),
    }
}