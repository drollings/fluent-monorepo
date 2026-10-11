//! `ta.*` builtin bridge: script calls dispatch to registered kernels.
//!
//! [`TaBridge`] owns one [`IndicatorRegistry`] plus per-call-site stream
//! state (kernel states, source histories, running sums). Single-source
//! kernels run through [`Indicator::update`](crate::indicator::Indicator::update)
//! with `close` overridden by the evaluated source — the source argument is
//! honored, never dropped — while chart legs (high/low/volume) stay real,
//! fixing the reference's close-always bug. Recurrence comes from the
//! kernels themselves, never per-bar-from-zero recomputation.
//!
//! Window and pair operators without kernels (`highest`, `lowest`,
//! `highestbars`, `lowestbars`, `crossover`, `crossunder`) tail-call the
//! shared [`util`](crate::util) whole-series functions over the same
//! histories, so there is exactly one implementation of each. Pure
//! statistics without kernels (`correlation`, `median`) compute from the
//! shared [`math`](crate::math) primitives. Multi-output kernels (`macd`,
//! `bb`, `stoch`) yield their primary series until tuple unpacking arrives;
//! destructuring a call result refuses fail-closed rather than binding a
//! partial tuple. Anything without a kernel or a certain formula
//! (`dmi`, `adx`, `sar`, `dc`, `mode`, `percentrank`, `linreg`, pivots,
//! unknown names) refuses with its name.

use std::collections::HashMap;

use crate::error::KernelError;
use crate::indicator::{BarTick, IndicatorRegistry, IndicatorState, ParamSet, builtin_registry};
use crate::value::PineValue;

/// One script-facing `ta.*` namespace: kernels plus stream state.
///
/// Histories grow with the run (bounded by the runtime's `max_bars`), so
/// per-bar window operators stay `O(history)` — exact shared-`util`
/// semantics beat micro-optimized lookbacks at this scale.
pub struct TaBridge {
    registry: IndicatorRegistry,
    kernel_states: HashMap<String, IndicatorState>,
    histories: HashMap<String, Vec<f64>>,
    cum_sums: HashMap<String, f64>,
}

impl TaBridge {
    /// Every `ta.*` method the bridge accepts, in dispatch order. The
    /// calibration gate freezes this list: additions and removals fail
    /// there first, and anything absent refuses naming the method.
    #[must_use]
    pub fn supported_methods() -> &'static [&'static str] {
        &[
            "sma", "ema", "wma", "hma", "rma", "vwma", "rsi", "mom", "roc", "cci", "cmo",
            "mfi", "alma", "macd", "bb", "atr", "tr", "obv", "stoch", "vwap", "wpr",
            "stdev", "variance", "highest", "lowest", "highestbars", "lowestbars", "change",
            "crossover", "crossunder", "cum", "correlation", "median", "eit", "kalman", "voss",
        ]
    }

    /// Bind the shared kernel registry. Fails only if builtins collide.
    pub fn new() -> Result<Self, KernelError> {
        Ok(Self {
            registry: builtin_registry()?,
            kernel_states: HashMap::new(),
            histories: HashMap::new(),
            cum_sums: HashMap::new(),
        })
    }

    /// Dispatch one `ta.<method>(args)` call at one bar. `site` identifies
    /// the textual call site (stable across bars); `tick` carries the
    /// current chart legs. Evaluated `None` sources enter as `NaN` and
    /// propagate through kernels and windows exactly like leg `na`.
    pub fn call(
        &mut self,
        site: &str,
        method: &str,
        args: &[Option<PineValue>],
        tick: BarTick,
    ) -> Result<Option<PineValue>, KernelError> {
        let bad_arity = |want: &str| {
            KernelError::Eval(format!(
                "ta.{method} takes {want}, got {} argument(s)",
                args.len()
            ))
        };
        match method {
            "sma" | "ema" | "wma" | "hma" | "rma" | "vwma" | "rsi" | "mom" | "roc" | "cci"
            | "cmo" | "mfi" => {
                let [source, length] = args else {
                    return Err(bad_arity("source and length"));
                };
                let params = ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
                self.update_kernel(site, method, source.as_ref(), &params, tick)
            }
            "alma" => {
                let [source, length, offset, sigma] = args else {
                    return Err(bad_arity("source, length, offset and sigma"));
                };
                let params = ParamSet::new()
                    .with_int("length", pine_length(method, length.as_ref())?)
                    .with_float("offset", pine_number(method, offset.as_ref(), "offset")?)
                    .with_float("sigma", pine_number(method, sigma.as_ref(), "sigma")?);
                self.update_kernel(site, "alma", source.as_ref(), &params, tick)
            }
            "macd" => {
                let [source, fast, slow, signal] = args else {
                    return Err(bad_arity("source, fast, slow and signal lengths"));
                };
                let params = ParamSet::new()
                    .with_int("fast_length", pine_length(method, fast.as_ref())?)
                    .with_int("slow_length", pine_length(method, slow.as_ref())?)
                    .with_int("signal_length", pine_length(method, signal.as_ref())?);
                self.update_kernel(site, "macd", source.as_ref(), &params, tick)
            }
            "bb" => {
                let [source, length, mult] = args else {
                    return Err(bad_arity("source, length and mult"));
                };
                let params = ParamSet::new()
                    .with_int("length", pine_length(method, length.as_ref())?)
                    .with_float("mult", pine_number(method, mult.as_ref(), "mult")?);
                self.update_kernel(site, "bb", source.as_ref(), &params, tick)
            }
            "atr" => {
                let [length] = args else {
                    return Err(bad_arity("length"));
                };
                let params = ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
                self.run_kernel(site, "atr", tick, &params)
            }
            "tr" => {
                if let [handle] = args {
                    if matches!(handle, Some(PineValue::Bool(true))) {
                        return Err(KernelError::Eval(
                            "ta.tr(true) requests alternate na handling, which is not bridged".to_owned(),
                        ));
                    }
                } else if !args.is_empty() {
                    return Err(bad_arity("zero or one argument"));
                }
                self.run_kernel(site, "tr", tick, &ParamSet::new())
            }
            "obv" => {
                if !args.is_empty() {
                    return Err(bad_arity("no arguments"));
                }
                self.run_kernel(site, "obv", tick, &ParamSet::new())
            }
            "stoch" => {
                let [source, high, low, length] = args else {
                    return Err(bad_arity("source, high, low and length"));
                };
                let params = ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
                let directed = BarTick {
                    high: pine_source(high.as_ref())?,
                    low: pine_source(low.as_ref())?,
                    ..with_source(tick, pine_source(source.as_ref())?)
                };
                self.run_kernel(site, "stoch", directed, &params)
            }
            "vwap" => {
                match args {
                    [] => self.run_kernel(site, "vwap", tick, &ParamSet::new()),
                    [one] => self.update_kernel(site, "vwap", one.as_ref(), &ParamSet::new(), tick),
                    _ => Err(bad_arity("zero or one argument")),
                }
            }
            "wpr" => {
                let [length] = args else {
                    return Err(bad_arity("length"));
                };
                let params = ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
                self.run_kernel(site, "willr", tick, &params)
            }
            "stdev" | "variance" => {
                let (source, length, biased) = match args {
                    [source, length] => (source, length, true),
                    [source, length, biased] => (source, length, pine_bool(method, biased.as_ref())?),
                    _ => return Err(bad_arity("source, length and optional biased")),
                };
                if biased {
                    let kernel = if method == "stdev" { "stddev" } else { "var" };
                    let params = ParamSet::new()
                        .with_int("length", pine_length(method, length.as_ref())?)
                        .with_float("nbdev", 1.0);
                    self.update_kernel(site, kernel, source.as_ref(), &params, tick)
                } else {
                    let length = pine_usize(method, length.as_ref())?;
                    let tail = self.push_history(site, pine_source(source.as_ref())?);
                    let window = window_of(tail, length);
                    let out = if window.len() < length || window.iter().any(|v| v.is_nan()) {
                        None
                    } else if method == "stdev" {
                        Some(crate::math::std_sample(window))
                    } else {
                        Some(crate::math::variance_sample(window))
                    };
                    Ok(out.map(PineValue::Float))
                }
            }
            "highest" | "lowest" => {
                let [source, length] = args else {
                    return Err(bad_arity("source and length"));
                };
                let length = pine_usize(method, length.as_ref())?;
                let tail = self.push_history(site, pine_source(source.as_ref())?);
                let window = window_of(tail, length);
                if window.len() < length {
                    return Ok(None);
                }
                let extreme = if method == "highest" {
                    crate::util::highest(window, length)
                } else {
                    crate::util::lowest(window, length)
                };
                Ok(extreme.last().copied().flatten().map(PineValue::Float))
            }
            "highestbars" | "lowestbars" => {
                let [source, length] = args else {
                    return Err(bad_arity("source and length"));
                };
                let length = pine_usize(method, length.as_ref())?;
                let tail = self.push_history(site, pine_source(source.as_ref())?);
                let window = window_of(tail, length);
                if window.len() < length {
                    return Ok(None);
                }
                // Pine reports bars-ago as a non-positive offset: the current
                // bar is 0, matching the aroon normalization in the library.
                let bars = if method == "highestbars" {
                    crate::util::argmax_window(window, length)
                } else {
                    crate::util::argmin_window(window, length)
                };
                let ago = bars.last().copied().flatten();
                ago.map(|back| {
                    i64::try_from(back).map(|bars_back| PineValue::Int(-bars_back))
                })
                .transpose()
                .map_err(|_| {
                    KernelError::Eval(format!("ta.{method} window does not fit this run"))
                })
            }
            "change" => {
                let (source, length) = match args {
                    [source] => (source, 1),
                    [source, length] => (source, pine_usize(method, length.as_ref())?),
                    _ => return Err(bad_arity("source and optional length")),
                };
                let tail = self.push_history(site, pine_source(source.as_ref())?);
                let back = tail
                    .len()
                    .checked_sub(length.saturating_add(1))
                    .and_then(|i| tail.get(i))
                    .copied();
                let now = tail.last().copied().unwrap_or(f64::NAN);
                Ok(match (now, back) {
                    (a, Some(b)) if !a.is_nan() && !b.is_nan() => Some(PineValue::Float(a - b)),
                    _ => None,
                })
            }
            "crossover" | "crossunder" => {
                let [first, second] = args else {
                    return Err(bad_arity("two series"));
                };
                let key_a = format!("{site}#a");
                let key_b = format!("{site}#b");
                self.push_history(&key_a, pine_source(first.as_ref())?);
                self.push_history(&key_b, pine_source(second.as_ref())?);
                let a = self.history(&key_a);
                let b = self.history(&key_b);
                // Histories only grow, so the shared whole-series computation
                // agrees with the incremental predicate by construction; the
                // bridge suite pins the agreement on seeded frames.
                let crossed = if method == "crossover" {
                    crate::util::crossover(a, b)
                } else {
                    crate::util::crossunder(a, b)
                };
                Ok(crossed.last().copied().flatten().map(PineValue::Bool))
            }
            "cum" => {
                let [source] = args else {
                    return Err(bad_arity("source"));
                };
                let value = pine_source(source.as_ref())?;
                if value.is_nan() {
                    return Ok(None);
                }
                let running = self.cum_sums.get(site).copied().unwrap_or(0.0) + value;
                self.cum_sums.insert(site.to_owned(), running);
                Ok(Some(PineValue::Float(running)))
            }
            "correlation" => {
                let [first, second, length] = args else {
                    return Err(bad_arity("two series and length"));
                };
                let length = pine_usize(method, length.as_ref())?;
                let key_a = format!("{site}#a");
                let key_b = format!("{site}#b");
                self.push_history(&key_a, pine_source(first.as_ref())?);
                self.push_history(&key_b, pine_source(second.as_ref())?);
                let a = self.history(&key_a);
                let b = self.history(&key_b);
                let (wa, wb) = (window_of(a, length), window_of(b, length));
                if wa.len() < length
                    || wb.len() < length
                    || wa.iter().any(|v| v.is_nan())
                    || wb.iter().any(|v| v.is_nan())
                {
                    return Ok(None);
                }
                Ok(crate::math::corr_pearson(wa, wb).map(PineValue::Float))
            }
            "median" => {
                let [source, length] = args else {
                    return Err(bad_arity("source and length"));
                };
                let length = pine_usize(method, length.as_ref())?;
                let tail = self.push_history(site, pine_source(source.as_ref())?);
                let window = window_of(tail, length);
                if window.len() < length || window.iter().any(|v| v.is_nan()) {
                    return Ok(None);
                }
                let mut sorted = window.to_vec();
                sorted.sort_by(f64::total_cmp);
                let middle = sorted.len() / 2;
                let median = if sorted.len() % 2 == 1 {
                    sorted[middle]
                } else {
                    f64::midpoint(sorted[middle - 1], sorted[middle])
                };
                Ok(Some(PineValue::Float(median)))
            }
            "eit" => {
                let [source, alpha] = args else {
                    return Err(bad_arity("source and alpha"));
                };
                let params =
                    ParamSet::new().with_float("alpha", pine_number(method, alpha.as_ref(), "alpha")?);
                self.update_kernel(site, "eit", source.as_ref(), &params, tick)
            }
            "kalman" => {
                let (source, period, alpha, beta) = match args {
                    [source, period] => (
                        source,
                        pine_length(method, period.as_ref())?,
                        crate::filters::KalmanIndicator::DEFAULT_ALPHA,
                        crate::filters::KalmanIndicator::DEFAULT_BETA,
                    ),
                    [source, period, alpha, beta] => (
                        source,
                        pine_length(method, period.as_ref())?,
                        pine_number(method, alpha.as_ref(), "alpha")?,
                        pine_number(method, beta.as_ref(), "beta")?,
                    ),
                    _ => return Err(bad_arity("source, period and optional alpha/beta")),
                };
                let params = ParamSet::new()
                    .with_int("length", period)
                    .with_float("alpha", alpha)
                    .with_float("beta", beta);
                self.update_kernel(site, "kalman", source.as_ref(), &params, tick)
            }
            "voss" => {
                let (source, period, predict, band) = match args {
                    [source, period] => (
                        source,
                        pine_length(method, period.as_ref())?,
                        crate::filters::VossIndicator::DEFAULT_PREDICT,
                        crate::filters::VossIndicator::DEFAULT_BAND,
                    ),
                    [source, period, predict, band] => (
                        source,
                        pine_length(method, period.as_ref())?,
                        pine_int(method, predict.as_ref())?,
                        pine_number(method, band.as_ref(), "band")?,
                    ),
                    _ => return Err(bad_arity("source, period and optional predict/band")),
                };
                let params = ParamSet::new()
                    .with_int("length", period)
                    .with_int("predict", predict)
                    .with_float("band", band);
                self.update_kernel(site, "voss", source.as_ref(), &params, tick)
            }
            _ => Err(KernelError::Eval(format!("unknown ta function 'ta.{method}'"))),
        }
    }

    /// Run one kernel update at one site, overriding `close` with the
    /// evaluated source. `None` is an explicit `na` source and enters as
    /// `NaN`; sourceless kernels bypass this helper via [`run_kernel`](Self::run_kernel)
    /// so chart legs stay untouched.
    fn update_kernel(
        &mut self,
        site: &str,
        kernel: &str,
        source: Option<&PineValue>,
        params: &ParamSet,
        tick: BarTick,
    ) -> Result<Option<PineValue>, KernelError> {
        let directed = with_source(tick, pine_source(source)?);
        self.run_kernel(site, kernel, directed, params)
    }

    /// Run one kernel update at one site with an explicit tick.
    fn run_kernel(
        &mut self,
        site: &str,
        kernel: &str,
        tick: BarTick,
        params: &ParamSet,
    ) -> Result<Option<PineValue>, KernelError> {
        let indicator = self.registry.get(kernel).ok_or_else(|| {
            KernelError::Eval(format!("ta bridge has no '{kernel}' kernel"))
        })?;
        let state = self.kernel_states.entry(site.to_owned()).or_default();
        Ok(indicator
            .update(state, tick, params)?
            .map(PineValue::Float))
    }

    /// Append one source value to a site history, returning the full history.
    fn push_history(&mut self, site: &str, value: f64) -> &[f64] {
        let history = self.histories.entry(site.to_owned()).or_default();
        history.push(value);
        history
    }

    /// Borrow one site history (empty when the site never pushed).
    fn history(&self, site: &str) -> &[f64] {
        self.histories.get(site).map_or(&[], Vec::as_slice)
    }
}

/// The trailing `length` values of a history (the whole history while short).
fn window_of(history: &[f64], length: usize) -> &[f64] {
    let start = history.len().saturating_sub(length);
    &history[start..]
}

/// Override a tick's `close` with an evaluated source value.
fn with_source(tick: BarTick, source: f64) -> BarTick {
    BarTick { close: source, ..tick }
}

/// A script source value as a leg float: absence is `NaN` (`na`, which
/// propagates); non-numeric values refuse instead of degrading silently.
fn pine_source(value: Option<&PineValue>) -> Result<f64, KernelError> {
    match value {
        Some(PineValue::Float(v)) => Ok(*v),
        Some(PineValue::Int(v)) => Ok(*v as f64),
        Some(other) => Err(KernelError::Eval(format!(
            "ta call needs a numeric source, got {other}"
        ))),
        None => Ok(f64::NAN),
    }
}

/// A script length argument: whole, positive, fitting `i64`.
fn pine_length(method: &str, value: Option<&PineValue>) -> Result<i64, KernelError> {
    match value {
        Some(PineValue::Int(v)) if *v >= 1 => Ok(*v),
        Some(PineValue::Float(v)) if v.fract() == 0.0 && *v >= 1.0 && *v <= i64::MAX as f64 => {
            Ok(*v as i64)
        }
        _ => Err(KernelError::Eval(format!(
            "ta.{method} needs a positive whole length"
        ))),
    }
}

/// A script length argument as a `usize` (checked, never wrapping).
fn pine_usize(method: &str, value: Option<&PineValue>) -> Result<usize, KernelError> {
    let length = pine_length(method, value)?;
    usize::try_from(length).map_err(|_| {
        KernelError::Eval(format!("ta.{method} length {length} does not fit this run"))
    })
}

/// A script numeric argument with `int` auto-cast.
fn pine_number(method: &str, value: Option<&PineValue>, what: &str) -> Result<f64, KernelError> {
    match value {
        Some(PineValue::Float(v)) => Ok(*v),
        Some(PineValue::Int(v)) => Ok(*v as f64),
        _ => Err(KernelError::Eval(format!(
            "ta.{method} needs a numeric {what}"
        ))),
    }
}

/// A script whole-number argument (kernel `predict`-style counts).
fn pine_int(method: &str, value: Option<&PineValue>) -> Result<i64, KernelError> {
    match value {
        Some(PineValue::Int(v)) => Ok(*v),
        Some(PineValue::Float(v)) if v.fract() == 0.0 && v.is_finite() => Ok(*v as i64),
        _ => Err(KernelError::Eval(format!(
            "ta.{method} needs a whole number"
        ))),
    }
}

/// A script boolean flag argument.
fn pine_bool(method: &str, value: Option<&PineValue>) -> Result<bool, KernelError> {
    match value {
        Some(PineValue::Bool(flag)) => Ok(*flag),
        _ => Err(KernelError::Eval(format!(
            "ta.{method} needs a bool flag here"
        ))),
    }
}

#[cfg(test)]
#[path = "../../tests/pine_builtins_ta.rs"]
mod tests;
