//! The windowed kernel shell: one leg fill, one window evaluation.
//!
//! Most kernels are the same shape — "read the trailing `period` bars of one leg
//! and reduce them to one number". That shell (`batch`'s window loop, `update`'s
//! push-and-fill, the warmup arithmetic, the `None`-before-warmup contract) was
//! repeated per kernel; only the window evaluation is the kernel's own.
//!
//! [`Windowed`] keeps exactly that part: **which legs** the kernel reads, **how
//! wide** the window is, and **what the window evaluates to**. The shell
//! functions here provide everything else, so batch and update run the identical
//! window evaluation in the identical order — the property the all-kernel fold
//! checks bit for bit. Each kernel's `impl Indicator` is then a four-line
//! delegation, which is deliberate: `Indicator` is the registry's one contract,
//! and two blanket impls (one over `Windowed`, one over [`Chained`](crate::composed))
//! would overlap in the type system even though no kernel is both shapes.
//!
//! The `previous` argument of [`Windowed::eval`] is what lets a seed-then-recur
//! kernel (`ema`, `rma`) live here without a second shell: the generic loop
//! carries the last output, and the kernel decides whether to seed from the
//! window mean or step from it.
//!
//! Bypass table (M2.1): every one-leg indicator delegates `refine_warmup`
//! to [`warmup`] below unless listed here. Each bypass carries a
//! `// BYPASS(windowed::warmup)` comment at its `warmup` with its reason.
//!
//! | Bypass | Reason |
//! |---|---|
//! | `filters` Eit / Kalman / Voss | Recursive with seeded estimate or
//! defined-zero gate — opening print defined, honest warmup is 1 |
//! | `ta_momentum` RSV / Williams %R | Single-bar-relative fast
//! oscillators — defined once one bar exists |
//! | `ta_momentum` fast Stoch | fastk leg carries the reference's defined
//! zero from bar zero |

use crate::indicator::{
    nan_hole, BarTick, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv, ParamSet,
};
use crate::math::warmup_none;
use crate::KernelError;

/// Which chart legs a window evaluation reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowLegs {
    /// Closes only: the common case.
    Close,
    /// Closes and volumes (volume-weighted averages).
    CloseVolume,
    /// Closes, highs, and lows (range midpoints).
    HighLow,
}

/// One window of chart legs, oldest first.
///
/// A leg the kernel did not ask for is an **empty** slice, never an alias of a
/// leg it did: a kernel that reads the wrong leg gets an empty window and
/// refuses through its own arithmetic rather than silently reading closes as
/// highs.
#[derive(Debug, Clone, Copy)]
pub struct Window<'a> {
    /// The window's closes (always present, `period` values).
    pub closes: &'a [f64],
    /// The window's highs, or empty unless [`WindowLegs::HighLow`].
    pub highs: &'a [f64],
    /// The window's lows, or empty unless [`WindowLegs::HighLow`].
    pub lows: &'a [f64],
    /// The window's volumes, or empty unless [`WindowLegs::CloseVolume`].
    pub volumes: &'a [f64],
}

impl Window<'_> {
    /// The current bar's close: the newest value in the window.
    #[must_use]
    pub fn newest(&self) -> f64 {
        *self.closes.last().expect("a window is never empty")
    }
}

/// A kernel that is one window over one leg.
///
/// Implementors provide the window evaluation and their own parameter
/// resolution; [`warmup`], [`batch`], and [`update`] below provide the shell. A
/// kernel whose warmup is longer than its window (a composition lag) declares it
/// through [`lag`](Self::lag).
pub trait Windowed: Send + Sync {
    /// Parameters resolved once per call: a `usize`, or a small copyable tuple
    /// the kernel already uses.
    type Shape: Copy + Send + Sync + 'static;

    /// Static description of this indicator.
    fn spec(&self) -> &IndicatorSpec;

    /// Resolve the kernel's scalars from the boundary parameters, with the same
    /// validation the kernel applied before.
    fn shape(&self, params: &ParamSet) -> Result<Self::Shape, KernelError>;

    /// Window width in bars implied by `shape`.
    fn period(shape: Self::Shape) -> usize;

    /// Which legs [`eval`](Self::eval) reads.
    fn legs(&self) -> WindowLegs;

    /// Evaluate one window, oldest first. `previous` is the kernel's last
    /// emitted value (seed-then-recur kernels step from it; pure windows
    /// ignore it). `None` is `na`.
    fn eval(&self, shape: Self::Shape, window: &Window<'_>, previous: Option<f64>) -> Option<f64>;

    /// Bars of history beyond the window width before the first output. Zero
    /// for a plain window kernel.
    fn lag(&self, _shape: Self::Shape) -> usize {
        0
    }
}

/// `warmup` for a [`Windowed`] kernel: the window plus any declared lag.
///
/// # Errors
///
/// The kernel's own parameter refusal.
pub fn warmup<T: Windowed>(kernel: &T, params: &ParamSet) -> Result<usize, KernelError> {
    let shape = kernel.shape(params)?;
    Ok(T::period(shape) + kernel.lag(shape))
}

/// `batch` for a [`Windowed`] kernel: one window evaluation per bar from
/// `warmup - 1`, feeding each output forward as `previous`.
///
/// # Errors
///
/// A missing or unaligned leg the kernel asked for, or a parameter refusal.
pub fn batch<T: Windowed>(
    kernel: &T,
    data: &Ohlcv<'_>,
    params: &ParamSet,
) -> Result<IndicatorResult, KernelError> {
    // Legs refuse before parameters, exactly as the per-kernel bodies did: an
    // unaligned leg is a request-shape refusal, not a parameter one.
    let legs = kernel.legs();
    let (highs, lows, volumes) = match legs {
        WindowLegs::Close => (&[][..], &[][..], &[][..]),
        WindowLegs::CloseVolume => (&[][..], &[][..], data.require("volume", data.volume)?),
        WindowLegs::HighLow => (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
            &[][..],
        ),
    };
    let shape = kernel.shape(params)?;
    let period = T::period(shape);
    let warmup = period + kernel.lag(shape);
    let length = data.len();
    let mut values = warmup_none(length);
    if warmup <= length {
        let mut previous: Option<f64> = None;
        for index in warmup - 1..length {
            let start = index + 1 - period;
            let window = Window {
                closes: &data.close[start..=index],
                highs: match legs {
                    WindowLegs::HighLow => &highs[start..=index],
                    _ => &[],
                },
                lows: match legs {
                    WindowLegs::HighLow => &lows[start..=index],
                    _ => &[],
                },
                volumes: match legs {
                    WindowLegs::CloseVolume => &volumes[start..=index],
                    _ => &[],
                },
            };
            let output = kernel.eval(shape, &window, previous);
            previous = output;
            values[index] = output;
        }
    }
    Ok(IndicatorResult::new(kernel.spec().id.clone(), values))
}

/// `update` for a [`Windowed`] kernel: push the bar, fill the scratch legs, and
/// evaluate the window the state now retains.
///
/// # Errors
///
/// The kernel's own parameter refusal.
pub fn update<T: Windowed>(
    kernel: &T,
    state: &mut IndicatorState,
    bar: BarTick,
    params: &ParamSet,
) -> Result<Option<f64>, KernelError> {
    let shape = state.resolved(|| kernel.shape(params))?;
    let period = T::period(shape);
    let warmup = period + kernel.lag(shape);
    state.push(bar, warmup);
    if state.len() < warmup {
        return Ok(None);
    }
    // The previous output copies out before the scratch fill: the window slice
    // borrows the state, so every scalar read leads it.
    let previous = state.last();
    let output = match kernel.legs() {
        WindowLegs::Close => {
            let closes = state.scratch_closes();
            let closes = &closes[closes.len() - period..];
            kernel.eval(
                shape,
                &Window {
                    closes,
                    highs: &[],
                    lows: &[],
                    volumes: &[],
                },
                previous,
            )
        }
        WindowLegs::CloseVolume => {
            let (closes, volumes) = state.scratch_closes_volumes();
            let (closes, volumes) = (
                &closes[closes.len() - period..],
                &volumes[volumes.len() - period..],
            );
            kernel.eval(
                shape,
                &Window {
                    closes,
                    highs: &[],
                    lows: &[],
                    volumes,
                },
                previous,
            )
        }
        WindowLegs::HighLow => {
            let (highs, lows, closes) = state.scratch_highs_lows_closes();
            let (highs, lows, closes) = (
                &highs[highs.len() - period..],
                &lows[lows.len() - period..],
                &closes[closes.len() - period..],
            );
            kernel.eval(
                shape,
                &Window {
                    closes,
                    highs,
                    lows,
                    volumes: &[],
                },
                previous,
            )
        }
    };
    state.set_last(nan_hole(output));
    Ok(output)
}
