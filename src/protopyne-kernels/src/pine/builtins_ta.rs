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
//! shared [`math`](crate::math) primitives. Condition lookups
//! (`barssince`, `valuewhen`) and pivots (`pivothigh`, `pivotlow`) run over
//! per-site histories with no kernel; pivots honor the evaluated source
//! series, never the chart legs. Multi-output kernels (`macd`,
//! `bb`, `stoch`) yield their primary series until tuple unpacking arrives;
//! destructuring a call result refuses fail-closed rather than binding a
//! partial tuple. Anything without a kernel or a certain formula
//! (`dmi`, `adx`, `sar`, `dc`, `mode`, `percentrank`, `linreg`,
//! unknown names) refuses with its name.

use std::collections::HashMap;

use crate::error::KernelError;
use crate::indicator::{
    builtin_registry, nan_hole, BarTick, Indicator, IndicatorRegistry, IndicatorState, ParamSet,
};
use crate::value::PineValue;
use protopyne_types::warmup::trim_indicator_output;

/// One script-facing `ta.*` namespace: kernels plus stream state.
///
/// Histories grow with the run (bounded by the runtime's `max_bars`), so
/// per-bar window operators stay `O(history)` — exact shared-`util`
/// semantics beat micro-optimized lookbacks at this scale.
pub struct TaBridge {
    registry: IndicatorRegistry,
    kernel_states: HashMap<String, IndicatorState>,
    histories: HashMap<String, Vec<f64>>,
    cond_histories: HashMap<String, Vec<bool>>,
    cum_sums: HashMap<String, f64>,
}

impl TaBridge {
    /// Every `ta.*` method the bridge accepts, in dispatch order. The
    /// calibration gate freezes this list: additions and removals fail
    /// there first, and anything absent refuses naming the method.
    #[must_use]
    pub fn supported_methods() -> &'static [&'static str] {
        &[
            "sma",
            "ema",
            "wma",
            "hma",
            "rma",
            "vwma",
            "rsi",
            "mom",
            "roc",
            "cci",
            "cmo",
            "mfi",
            "alma",
            "macd",
            "bb",
            "atr",
            "tr",
            "obv",
            "stoch",
            "vwap",
            "wpr",
            "stdev",
            "variance",
            "highest",
            "lowest",
            "highestbars",
            "lowestbars",
            "change",
            "crossover",
            "crossunder",
            "cum",
            "correlation",
            "median",
            "eit",
            "kalman",
            "voss",
            "barssince",
            "valuewhen",
            "pivothigh",
            "pivotlow",
        ]
    }

    /// Bind the shared kernel registry. Fails only if builtins collide.
    pub fn new() -> Result<Self, KernelError> {
        Ok(Self {
            registry: builtin_registry()?,
            kernel_states: HashMap::new(),
            histories: HashMap::new(),
            cond_histories: HashMap::new(),
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
                let params =
                    ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
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
                let params =
                    ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
                self.run_kernel(site, "atr", tick, &params)
            }
            "tr" => {
                if let [handle] = args {
                    if matches!(handle, Some(PineValue::Bool(true))) {
                        return Err(KernelError::Eval(
                            "ta.tr(true) requests alternate na handling, which is not bridged"
                                .to_owned(),
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
                let params =
                    ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
                let directed = BarTick {
                    high: pine_source(high.as_ref())?,
                    low: pine_source(low.as_ref())?,
                    ..with_source(tick, pine_source(source.as_ref())?)
                };
                self.run_kernel(site, "stoch", directed, &params)
            }
            "vwap" => match args {
                [] => self.run_kernel(site, "vwap", tick, &ParamSet::new()),
                [one] => self.update_kernel(site, "vwap", one.as_ref(), &ParamSet::new(), tick),
                _ => Err(bad_arity("zero or one argument")),
            },
            "wpr" => {
                let [length] = args else {
                    return Err(bad_arity("length"));
                };
                let params =
                    ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
                self.run_kernel(site, "willr", tick, &params)
            }
            "stdev" | "variance" => {
                let (source, length, biased) = match args {
                    [source, length] => (source, length, true),
                    [source, length, biased] => {
                        (source, length, pine_bool(method, biased.as_ref())?)
                    }
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
                    Ok(sample_spread(method, window_of(tail, length), length))
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
                ago.map(|back| i64::try_from(back).map(|bars_back| PineValue::Int(-bars_back)))
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
                let now = nan_hole(tail.last().copied());
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
                Ok(crate::math::median_of(window).map(PineValue::Float))
            }
            "eit" => {
                let [source, alpha] = args else {
                    return Err(bad_arity("source and alpha"));
                };
                let params = ParamSet::new()
                    .with_float("alpha", pine_number(method, alpha.as_ref(), "alpha")?);
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
            "barssince" => {
                let [condition] = args else {
                    return Err(bad_arity("condition"));
                };
                let flag = pine_condition(method, condition.as_ref())?;
                let history = self.push_cond(&format!("{site}#cond"), flag);
                let back = history
                    .iter()
                    .rposition(|&hit| hit)
                    .map(|at| history.len() - 1 - at);
                back.map(|ago| {
                    i64::try_from(ago).map(PineValue::Int).map_err(|_| {
                        KernelError::Eval(format!("ta.{method} window does not fit this run"))
                    })
                })
                .transpose()
            }
            "valuewhen" => {
                let [condition, source, occurrence] = args else {
                    return Err(bad_arity("condition, source and occurrence"));
                };
                let flag = pine_condition(method, condition.as_ref())?;
                let value = pine_source(source.as_ref())?;
                let want = pine_occurrence(method, occurrence.as_ref())?;
                let cond_key = format!("{site}#cond");
                let src_key = format!("{site}#src");
                self.push_cond(&cond_key, flag);
                self.push_history(&src_key, value);
                let conds = self.cond_history(&cond_key);
                let sources = self.history(&src_key);
                let mut seen = 0_usize;
                let mut found = None;
                for (index, &hit) in conds.iter().enumerate().rev() {
                    if hit {
                        if seen == want {
                            found = Some(index);
                            break;
                        }
                        seen += 1;
                    }
                }
                Ok(found
                    .and_then(|index| sources.get(index).copied().filter(|v| !v.is_nan()))
                    .map(PineValue::Float))
            }
            "pivothigh" | "pivotlow" => {
                let [source, left, right] = args else {
                    return Err(bad_arity("source, leftbars and rightbars"));
                };
                let left = pine_usize(method, left.as_ref())?;
                let right = pine_usize(method, right.as_ref())?;
                let tail = self.push_history(site, pine_source(source.as_ref())?);
                Ok(pivot_at(tail, left, right, method == "pivothigh"))
            }
            _ => Err(KernelError::Eval(format!(
                "unknown ta function 'ta.{method}'"
            ))),
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
        let indicator = self
            .registry
            .get(kernel)
            .ok_or_else(|| KernelError::Eval(format!("ta bridge has no '{kernel}' kernel")))?;
        let state = self.kernel_states.entry(site.to_owned()).or_default();
        Ok(indicator.update(state, tick, params)?.map(PineValue::Float))
    }

    /// Append one source value to a site history, returning the full history.
    fn push_history(&mut self, site: &str, value: f64) -> &[f64] {
        let history = self.histories.entry(site.to_owned()).or_default();
        history.push(value);
        history
    }

    /// Append one condition value to a site condition history, returning
    /// the full history. Conditions ride a `bool` history beside the
    /// `f64` source histories so `na` never encodes as a quiet `0.0`.
    fn push_cond(&mut self, site: &str, flag: bool) -> &[bool] {
        let history = self.cond_histories.entry(site.to_owned()).or_default();
        history.push(flag);
        history
    }

    /// Borrow one site condition history (empty when the site never pushed).
    fn cond_history(&self, site: &str) -> &[bool] {
        self.cond_histories.get(site).map_or(&[], Vec::as_slice)
    }

    /// Borrow one site history (empty when the site never pushed).
    fn history(&self, site: &str) -> &[f64] {
        self.histories.get(site).map_or(&[], Vec::as_slice)
    }

    /// Evaluate one kernel-backed `ta.*` call over the **full** confirmed
    /// security history and return its value at the last returned bar: the
    /// headless `request.security` path. `source` is the call's source column
    /// (a bare leg); it overrides `close` exactly like the streaming bridge,
    /// so the source argument is honored, never dropped. `display_count` is
    /// the request's `calc_bars_count`: it bounds the rows *returned*, never
    /// the history the kernel computes over, so a recursive arm seeds at the
    /// series' first row and a warmup longer than the display window still
    /// reads a number. Only single-source kernel arms ride here — window
    /// operators, multi-leg kernels (`stoch`), chart-leg kernels (`tr`, `obv`,
    /// `vwap`), and unknown names refuse naming the subset instead of
    /// silently re-scoping chart state. Batches are causal, so the last value
    /// over a confirmed prefix equals a streaming evaluation over the same
    /// prefix.
    pub fn batch_security(
        &self,
        method: &str,
        source: &[f64],
        args: &[Option<PineValue>],
        ohlcv: &crate::indicator::OwnedOhlcv,
        display_count: Option<usize>,
    ) -> Result<Option<PineValue>, KernelError> {
        let bad_arity = |want: &str| {
            KernelError::Eval(format!(
                "ta.{method} takes {want}, got {} argument(s)",
                args.len()
            ))
        };
        let over = |kernel: &str, params: &crate::indicator::ParamSet| {
            let indicator = self
                .registry
                .get(kernel)
                .ok_or_else(|| KernelError::Eval(format!("ta bridge has no '{kernel}' kernel")))?;
            let mut data = ohlcv.clone();
            data.close = source.to_vec();
            warmed_last(indicator.as_ref(), &data.views(), params, display_count)
        };
        match method {
            "sma" | "ema" | "wma" | "hma" | "rma" | "vwma" | "rsi" | "mom" | "roc" | "cci"
            | "cmo" | "mfi" => {
                let [_, length] = args else {
                    return Err(bad_arity("source and length"));
                };
                let params =
                    ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
                over(method, &params)
            }
            "alma" => {
                let [_, length, offset, sigma] = args else {
                    return Err(bad_arity("source, length, offset and sigma"));
                };
                let params = ParamSet::new()
                    .with_int("length", pine_length(method, length.as_ref())?)
                    .with_float("offset", pine_number(method, offset.as_ref(), "offset")?)
                    .with_float("sigma", pine_number(method, sigma.as_ref(), "sigma")?);
                over("alma", &params)
            }
            "atr" => {
                let [length] = args else {
                    return Err(bad_arity("length"));
                };
                let params =
                    ParamSet::new().with_int("length", pine_length(method, length.as_ref())?);
                let indicator = self
                    .registry
                    .get("atr")
                    .ok_or_else(|| KernelError::Eval("ta bridge has no 'atr' kernel".to_owned()))?;
                warmed_last(indicator.as_ref(), &ohlcv.views(), &params, display_count)
            }
            "stdev" | "variance" => {
                let (length, biased) = match args {
                    [_, length] => (length, true),
                    [_, length, biased] => (length, pine_bool(method, biased.as_ref())?),
                    _ => return Err(bad_arity("source, length and optional biased")),
                };
                if biased {
                    let kernel = if method == "stdev" { "stddev" } else { "var" };
                    let params = ParamSet::new()
                        .with_int("length", pine_length(method, length.as_ref())?)
                        .with_float("nbdev", 1.0);
                    over(kernel, &params)
                } else {
                    let length = pine_usize(method, length.as_ref())?;
                    let start = source.len().saturating_sub(length);
                    Ok(sample_spread(method, &source[start..], length))
                }
            }
            _ => Err(KernelError::Eval(format!(
                "injected security serves kernel-backed single-source ta calls, not 'ta.{method}'"
            ))),
        }
    }
}

/// Batch one kernel over the full confirmed security history and return the
/// value at the last **returned** bar. `display_count` (the request's
/// `calc_bars_count`) trims the returned tail through the shared warmup
/// discipline, so the display bound can never shorten the history a recursive
/// kernel seeds from. A series shorter than the kernel's warmup reads `na`,
/// exactly as the reference does during warmup: absence of history is a data
/// condition, never a reason to refuse the request.
fn warmed_last(
    indicator: &dyn Indicator,
    data: &crate::indicator::Ohlcv<'_>,
    params: &ParamSet,
    display_count: Option<usize>,
) -> Result<Option<PineValue>, KernelError> {
    let warmup = indicator.warmup(params)?;
    let batch = indicator.batch(data, params)?;
    let returned = trim_indicator_output(&batch.values, warmup, display_count);
    Ok(returned
        .last()
        .copied()
        .flatten()
        .and_then(crate::indicator::fin_or_none)
        .map(PineValue::Float))
}

/// A confirmed pivot over an explicit source history: the bar `right`
/// back must be finite and strictly extreme over its `left` left and
/// `right` right neighbors (equal extremes break the pivot, matching the
/// reference's `>=`/`<=` guard). Short or absence-carrying windows read
/// absence — the pivot is confirmed only on the bar that completes the
/// right window, `na` everywhere else. The source history is the
/// evaluated series, so a pivot in an oscillator reads the oscillator,
/// never the chart legs.
fn pivot_at(history: &[f64], left: usize, right: usize, is_high: bool) -> Option<PineValue> {
    let need = left.checked_add(right)?.checked_add(1)?;
    if history.len() < need {
        return None;
    }
    let pivot = history.len() - 1 - right;
    let value = history[pivot];
    if value.is_nan() {
        return None;
    }
    let extreme = |&v: &f64| !v.is_nan() && (if is_high { v < value } else { v > value });
    if history[pivot - left..pivot].iter().all(extreme) && history[pivot + 1..].iter().all(extreme)
    {
        Some(PineValue::Float(value))
    } else {
        None
    }
}
/// The trailing `length` values of a history (the whole history while short).
fn window_of(history: &[f64], length: usize) -> &[f64] {
    let start = history.len().saturating_sub(length);
    &history[start..]
}

/// Unbiased spread over an explicit window: short or absence-carrying
/// windows read absence, otherwise the sample statistic. Shared by the
/// streaming arm (history tail) and the security batch (series tail), so
/// the short/absence rule lives in exactly one place.
pub(crate) fn sample_spread(method: &str, window: &[f64], length: usize) -> Option<PineValue> {
    if window.len() < length || window.iter().any(|v| v.is_nan()) {
        None
    } else if method == "stdev" {
        Some(PineValue::Float(crate::math::std_sample(window)))
    } else {
        Some(PineValue::Float(crate::math::variance_sample(window)))
    }
}

/// Override a tick's `close` with an evaluated source value.
fn with_source(tick: BarTick, source: f64) -> BarTick {
    BarTick {
        close: source,
        ..tick
    }
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

/// A script condition argument: `true`/`false`; `na` (absence) counts as
/// unsatisfied (Pine never treats absence as a firing condition);
/// anything else refuses instead of coercing.
fn pine_condition(method: &str, value: Option<&PineValue>) -> Result<bool, KernelError> {
    match value {
        Some(PineValue::Bool(flag)) => Ok(*flag),
        None => Ok(false),
        Some(other) => Err(KernelError::Eval(format!(
            "ta.{method} needs a bool condition, got {other}"
        ))),
    }
}

/// A script occurrence argument (`valuewhen`): a whole number at or above
/// zero (`0` is the most recent firing).
fn pine_occurrence(method: &str, value: Option<&PineValue>) -> Result<usize, KernelError> {
    let at = pine_int(method, value)?;
    usize::try_from(at).map_err(|_| {
        KernelError::Eval(format!("ta.{method} needs a non-negative whole occurrence"))
    })
}

#[cfg(all(test, feature = "research"))]
#[path = "../../tests/pine_builtins_ta.rs"]
mod tests;

/// Task-closure `ta.*` closed-coverage probe, tuple-destructuring
/// verdict, and `input.*` decorator audit.
#[cfg(test)]
#[path = "../../tests/pine_ta_coverage.rs"]
mod coverage_tests;
