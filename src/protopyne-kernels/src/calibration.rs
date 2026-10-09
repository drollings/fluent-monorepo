//! Global calibration gate: universal controls plus the parity dashboard.
//!
//! Two axes, matching the roadmap's confidence/correctness split:
//!
//! * Producer-confidence (controls): flat, null-injected, short, and
//!   single-tick inputs must never leak `NaN`/`inf` into a `Some`, never
//!   shift alignment (output length always equals input length), must hold
//!   leading warmup `None`s, and null injection may only turn defined outputs
//!   into `None` — never resurrect a warming `None` into a value. These are
//!   universal invariants: they need no per-kernel expectation table, so a
//!   new kernel is controlled the moment it registers.
//! * Task-value (dashboard): every kernel accepts its `polars.Expr`
//!   reference at its frozen matrix eps over a seeded calibration frame
//!   (flat prefix forces Wilder/EMA seed coincidence, the same device the
//!   per-kernel suites use), with a 0.0 control false-fire rate. Green is 1.0
//!   accept plus 0.0 false-fire, mirroring the spike `CalibrationTally` shape
//!   (accept 1.0 plus reject 1.0) one crate over.
//!
//! [`canonical_params`] is the single home for calibration parameterizations,
//! shared by the family-specific flat suite and the dashboard.

use crate::error::KernelError;
use crate::indicator::{OwnedOhlcv, ParamSet, builtin_registry};
use crate::parity::assert_expr_parity;
use polars::prelude::{
    DataFrame, EWMOptions, Expr, IntoLazy, NamedFrom as _, RollingFnParams, RollingOptionsFixedWindow,
    RollingVarParams, Series, col, lit, when,
};

// ---------------------------------------------------------------------------
// Canonical calibration parameters (single home)
// ---------------------------------------------------------------------------

/// Calibration parameterization per kernel id: canonical lengths with the
/// slowest settings given headroom inside the control frames.
#[must_use]
pub fn canonical_params(id: &str) -> ParamSet {
    let len14 = || ParamSet::new().with_int("length", 14);
    match id {
        "alma" => ParamSet::new()
            .with_int("length", 14)
            .with_float("offset", 0.85)
            .with_float("sigma", 6.0),
        "macd" => ParamSet::new()
            .with_int("fast_length", 12)
            .with_int("slow_length", 26)
            .with_int("signal_length", 9),
        "bb" | "kc" => ParamSet::new()
            .with_int("length", 14)
            .with_float("mult", 2.0),
        "bbands" => ParamSet::new()
            .with_int("length", 14)
            .with_float("nbdevup", 2.0)
            .with_float("nbdevdn", 2.0),
        "apo" | "ppo" => ParamSet::new()
            .with_int("fast_length", 5)
            .with_int("slow_length", 12)
            .with_float("matype", 0.0),
        "stochf" => ParamSet::new()
            .with_int("fastk_length", 5)
            .with_int("fastd_length", 3),
        "adosc" => ParamSet::new()
            .with_int("fast_length", 3)
            .with_int("slow_length", 10),
        // Triple smoothing needs headroom inside the 40-bar control frame.
        "trix" => ParamSet::new().with_int("length", 10),
        "stddev" | "var" => ParamSet::new()
            .with_int("length", 14)
            .with_float("nbdev", 1.0),
        "tr" | "avgprice" | "medprice" | "typprice" | "wclprice" | "ad" | "obv" => ParamSet::new(),
        _ => len14(),
    }
}

// ---------------------------------------------------------------------------
// Synthetic calibration legs (f64; kernels and references share them)
// ---------------------------------------------------------------------------

/// Flat market legs: every price leg constant, steady volume.
fn flat_legs(n: usize) -> OwnedOhlcv {
    OwnedOhlcv {
        open: vec![42.0; n],
        high: vec![42.0; n],
        low: vec![42.0; n],
        close: vec![42.0; n],
        volume: vec![1_000.0; n],
    }
}

/// Trend legs with a flat prefix: forces seed coincidence between hand-rolled
/// recurrences (SMA/Wilder seeds) and `ewm_mean` references, the same device
/// the per-kernel suites use. Prefix covers the longest nested seed
/// (slow 26 + signal 9 for `macd`).
fn seeded_legs(n_flat: usize, n_trend: usize) -> OwnedOhlcv {
    let mut open = Vec::with_capacity(n_flat + n_trend);
    let mut high = Vec::with_capacity(n_flat + n_trend);
    let mut low = Vec::with_capacity(n_flat + n_trend);
    let mut close = Vec::with_capacity(n_flat + n_trend);
    let mut volume = Vec::with_capacity(n_flat + n_trend);
    for _ in 0..n_flat {
        open.push(100.0);
        high.push(100.0);
        low.push(100.0);
        close.push(100.0);
        volume.push(1_000.0);
    }
    let mut prev_close: f64 = 100.0;
    for i in 0..n_trend {
        let shut: f64 = 100.0 + i as f64 * 0.9 + ((i * 7) % 4) as f64 * 0.6;
        let unlock: f64 = if i == 0 { 100.0 } else { prev_close + 0.1 };
        open.push(unlock);
        high.push(unlock.max(shut) + 0.7);
        low.push(unlock.min(shut) - 0.7);
        close.push(shut);
        volume.push(1_000.0 + i as f64 * 11.0);
        prev_close = shut;
    }
    OwnedOhlcv { open, high, low, close, volume }
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

/// Must-NOT-fire control inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Constant market: no kernel may false-signal or leak.
    Flat,
    /// Null-injected trend: `None` may only spread, never shift or resurrect.
    NullInjected,
    /// Three bars: shorter than every windowed warmup.
    Short,
    /// One bar: refuse or emit `None`/first-print only.
    SingleTick,
}

impl Control {
    fn legs(self) -> OwnedOhlcv {
        match self {
            Control::Flat => flat_legs(40),
            Control::NullInjected => {
                let mut legs = seeded_legs(0, 30);
                legs.close[10] = f64::NAN;
                legs.close[20] = f64::NAN;
                legs
            }
            Control::Short => seeded_legs(0, 3),
            Control::SingleTick => seeded_legs(0, 1),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Control::Flat => "flat",
            Control::NullInjected => "null-injected",
            Control::Short => "short",
            Control::SingleTick => "single-tick",
        }
    }
}

/// One broken universal invariant.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlViolation {
    /// Kernel id.
    pub id: String,
    /// Control input name.
    pub control: &'static str,
    /// What broke.
    pub detail: String,
}

/// Run every registry kernel over one control input; empty means quiet.
/// Checks, per kernel: batch succeeds, output lengths equal input length, no
/// `Some(NaN)`/`Some(±inf)` anywhere, and leading warmup positions stay
/// `None` on the primary leg.
#[must_use]
pub fn run_control(control: Control) -> Vec<ControlViolation> {
    let legs = control.legs();
    let n = legs.close.len();
    let registry = builtin_registry().expect("builtins register");
    let mut violations = Vec::new();
    for id in registry.ids() {
        let kernel = registry.get(&id).expect("registered");
        let params = canonical_params(&id);
        let result = match kernel.batch(&legs.views(), &params) {
            Ok(result) => result,
            Err(error) => {
                violations.push(ControlViolation {
                    id: id.clone(),
                    control: control.name(),
                    detail: format!("refused control input: {error}"),
                });
                continue;
            }
        };
        if result.values.len() != n {
            violations.push(ControlViolation {
                id: id.clone(),
                control: control.name(),
                detail: format!("primary length {} != input {n}", result.values.len()),
            });
        }
        for leg in &result.secondary {
            if leg.values.len() != n {
                violations.push(ControlViolation {
                    id: id.clone(),
                    control: control.name(),
                    detail: format!("leg {:?} length {} != input {n}", leg.name, leg.values.len()),
                });
            }
        }
        let finite_or_none = |slot: &Option<f64>| slot.is_none_or(f64::is_finite);
        if !result.values.iter().all(finite_or_none) {
            violations.push(ControlViolation {
                id: id.clone(),
                control: control.name(),
                detail: "non-finite leak in primary leg".to_owned(),
            });
        }
        for leg in &result.secondary {
            if !leg.values.iter().all(finite_or_none) {
                violations.push(ControlViolation {
                    id: id.clone(),
                    control: control.name(),
                    detail: format!("non-finite leak in leg {:?}", leg.name),
                });
            }
        }
        // Slowest-leg warmup honesty: the last leg to warm must hold
        // silence through `warmup - 1`. (A warmup of 1 means the opening
        // print is defined, as with the cumulative kernels.) Faster legs may
        // legitimately lead — a composite's primary ahead of its signal —
        // and those early values are pinned exactly by dashboard parity
        // instead of here.
        match kernel.warmup(&params) {
            Ok(warmup) => {
                let threshold = warmup.saturating_sub(1).min(n);
                let first_defined =
                    |series: &[Option<f64>]| series.iter().position(Option::is_some).unwrap_or(n);
                let mut slowest = first_defined(&result.values);
                for leg in &result.secondary {
                    slowest = slowest.max(first_defined(&leg.values));
                }
                if slowest < threshold {
                    violations.push(ControlViolation {
                        id: id.clone(),
                        control: control.name(),
                        detail: format!("no leg held warmup ({warmup})"),
                    });
                }
            }
            Err(error) => violations.push(ControlViolation {
                id: id.clone(),
                control: control.name(),
                detail: format!("warmup refused: {error}"),
            }),
        }
    }
    violations
}

/// Positions where null injection resurrected a warming `None` into a value:
/// nulls may degrade defined outputs to `None`, never fabricate. Empty means
/// the alignment contract holds for every kernel, primary and secondaries.
#[must_use]
pub fn null_alignment_diffs() -> Vec<ControlViolation> {
    let clean = seeded_legs(0, 30);
    let mut nulled = seeded_legs(0, 30);
    nulled.close[10] = f64::NAN;
    nulled.close[20] = f64::NAN;
    let registry = builtin_registry().expect("builtins register");
    let mut diffs = Vec::new();
    for id in registry.ids() {
        let kernel = registry.get(&id).expect("registered");
        let params = canonical_params(&id);
        let before = kernel.batch(&clean.views(), &params).expect("clean runs");
        let after = kernel.batch(&nulled.views(), &params).expect("nulled runs");
        let mut legs_before = vec![("values", before.values.as_slice())];
        legs_before.extend(before.secondary.iter().map(|leg| (leg.name.as_str(), leg.values.as_slice())));
        let mut legs_after = vec![("values", after.values.as_slice())];
        legs_after.extend(after.secondary.iter().map(|leg| (leg.name.as_str(), leg.values.as_slice())));
        for ((name, was), (_, is)) in legs_before.iter().zip(legs_after.iter()) {
            for (i, (w, v)) in was.iter().zip(is.iter()).enumerate() {
                if w.is_none() && v.is_some() {
                    diffs.push(ControlViolation {
                        id: id.clone(),
                        control: "null-injected",
                        detail: format!("leg {name:?} resurrected warming None at bar {i}"),
                    });
                }
            }
        }
    }
    diffs
}

// ---------------------------------------------------------------------------
// Parity dashboard
// ---------------------------------------------------------------------------

/// One dashboard row: task-value accept plus control false-fire count.
#[derive(Debug, Clone, PartialEq)]
pub struct ParityRow {
    /// Registry id.
    pub id: &'static str,
    /// Frozen matrix eps the accept was measured at.
    pub eps: f64,
    /// Parity held against the `Expr` reference at frozen eps.
    pub accepted: bool,
    /// Why not, when refused.
    pub note: Option<String>,
    /// Control violations on flat/null/short/single-tick inputs.
    pub false_fires: usize,
}

/// Global parity dashboard: per-id accept rate plus control false-fire rate.
/// Green is 1.0 accept with 0.0 false-fire.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParityDashboard {
    /// One row per registry kernel, in registry order.
    pub rows: Vec<ParityRow>,
}

impl ParityDashboard {
    /// Fraction of kernels accepting their reference at frozen eps.
    #[must_use]
    pub fn accept_rate(&self) -> f64 {
        if self.rows.is_empty() {
            return 0.0;
        }
        self.rows.iter().filter(|row| row.accepted).count() as f64 / self.rows.len() as f64
    }

    /// Fraction of kernels tripping any control.
    #[must_use]
    pub fn false_fire_rate(&self) -> f64 {
        if self.rows.is_empty() {
            return 0.0;
        }
        self.rows.iter().filter(|row| row.false_fires > 0).count() as f64 / self.rows.len() as f64
    }

    /// Whether parity output may be trusted: every must-fire accepted and no
    /// must-NOT-fire control tripped.
    #[must_use]
    pub fn is_green(&self) -> bool {
        (self.accept_rate() - 1.0).abs() < f64::EPSILON && self.false_fire_rate() == 0.0
    }

    /// Measure every row: kernel batch versus staged `Expr` references on the
    /// seeded frame, plus the four control inputs for false fires.
    pub fn build() -> Result<Self, KernelError> {
        let legs = seeded_legs(60, 40);
        let frame = ref_frame(&legs)?;
        let registry = builtin_registry()?;
        let controls = [
            Control::Flat,
            Control::NullInjected,
            Control::Short,
            Control::SingleTick,
        ]
        .into_iter()
        .flat_map(run_control)
        .collect::<Vec<_>>();
        let mut rows = Vec::new();
        for id in registry.ids() {
            let kernel = registry.get(&id).expect("registered");
            let params = canonical_params(&id);
            let row_id: &'static str = matrix_id(&id);
            let eps = crate::compat::compat_for(row_id).expect("matrix row").eps;
            let result = kernel.batch(&legs.views(), &params)?;
            let refs = reference(row_id, &frame)?;
            let mut accepted = true;
            let mut note = None;
            // Leg bijectivity per id: the kernel's leg set must match the
            // reference's exactly — an orphan leg on either side refuses.
            let mut kernel_legs = vec!["values"];
            kernel_legs.extend(result.secondary.iter().map(|leg| leg.name.as_str()));
            let ref_legs: Vec<&str> = refs.iter().map(|(name, _)| *name).collect();
            if kernel_legs == ref_legs {
                let mut got = vec![result.values.as_slice()];
                got.extend(result.secondary.iter().map(|leg| leg.values.as_slice()));
                for ((name, want), have) in refs.iter().zip(got.iter()) {
                    if let Err(report) = assert_expr_parity(have, want, eps, name) {
                        accepted = false;
                        note = Some(report.chars().take(300).collect());
                        break;
                    }
                }
            } else {
                accepted = false;
                note = Some(format!("leg mismatch: kernel {kernel_legs:?} vs ref {ref_legs:?}"));
            }
            let false_fires = controls.iter().filter(|v| v.id == id).count();
            rows.push(ParityRow { id: row_id, eps, accepted, note, false_fires });
        }
        Ok(Self { rows })
    }
}

/// Registry ids are owned strings; the matrix holds the static twin.
fn matrix_id(id: &str) -> &'static str {
    crate::compat::COMPATIBILITY
        .iter()
        .find(|row| row.id == id)
        .expect("matrix row")
        .id
}

// ---------------------------------------------------------------------------
// Reference expressions (transcribed from the per-kernel parity suites)
// ---------------------------------------------------------------------------

fn strict(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow { window_size: n, min_periods: n, weights: None, center: false, fn_params: None }
}

fn pop_opts(n: usize) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow {
        window_size: n,
        min_periods: n,
        weights: None,
        center: false,
        fn_params: Some(RollingFnParams::Var(RollingVarParams { ddof: 0 })),
    }
}

fn ewm_span(n: usize) -> EWMOptions {
    EWMOptions { alpha: 2.0 / (n as f64 + 1.0), adjust: false, bias: false, min_periods: n, ignore_nulls: true }
}

fn ewm_wilder(n: usize) -> EWMOptions {
    EWMOptions { alpha: 1.0 / n as f64, adjust: false, bias: false, min_periods: n, ignore_nulls: true }
}

fn weighted(n: usize, weights: Vec<f64>) -> RollingOptionsFixedWindow {
    RollingOptionsFixedWindow { window_size: n, min_periods: n, weights: Some(weights), center: false, fn_params: None }
}

/// Ascending linear weights, oldest lightest: the kernel weights the `i`-th
/// oldest bar `(i + 1) * n`, and the `* n` factor cancels in normalization.
fn linear_weights(n: usize) -> Vec<f64> {
    (1..=n).map(|k| k as f64).collect()
}

fn ref_frame(legs: &OwnedOhlcv) -> Result<DataFrame, KernelError> {
    DataFrame::new(vec![
        Series::new("open".into(), &legs.open).into(),
        Series::new("high".into(), &legs.high).into(),
        Series::new("low".into(), &legs.low).into(),
        Series::new("close".into(), &legs.close).into(),
        Series::new("volume".into(), &legs.volume).into(),
    ])
    .map_err(|e| KernelError::Research(format!("calibration frame: {e}")))
}

fn fin_or_none(value: f64) -> Option<f64> {
    crate::indicator::fin_or_none(value)
}

/// Run staged selects and read every named column back with the parity
/// funnel mapping (`NaN` to `None`, infinities pass through as values).
fn select_legs(frame: &DataFrame, exprs: Vec<Expr>) -> Result<Vec<Vec<Option<f64>>>, KernelError> {
    let selected = frame
        .clone()
        .lazy()
        .select(exprs)
        .collect()
        .map_err(|e| KernelError::Research(format!("calibration select: {e}")))?;
    if selected.height() != frame.height() {
        return Err(KernelError::Research(format!(
            "calibration select height {} != frame {}",
            selected.height(),
            frame.height()
        )));
    }
    selected
        .get_columns()
        .iter()
        .map(|series| {
            let cast = series
                .cast(&polars::prelude::DataType::Float64)
                .map_err(|e| KernelError::Research(format!("calibration cast: {e}")))?;
            let chunked = cast
                .f64()
                .map_err(|e| KernelError::Research(format!("calibration f64 view: {e}")))?;
            Ok((0..chunked.len()).map(|i| chunked.get(i).and_then(fin_or_none)).collect())
        })
        .collect()
}

fn single(frame: &DataFrame, expr: Expr) -> Result<Vec<Option<f64>>, KernelError> {
    Ok(select_legs(frame, vec![expr])?.remove(0))
}

/// Null-safe true range ladder: `max_horizontal` has no unambiguous
/// re-export, so the ladder stands in (null falls through, matching the
/// ignore-nulls horizontals on clean data and the first-bar `high - low`).
fn tr_expr() -> Expr {
    let hl = col("high") - col("low");
    let hpc = (col("high") - col("close").shift(lit(1_i64))).abs();
    let lpc = (col("low") - col("close").shift(lit(1_i64))).abs();
    when(hpc.clone().gt(hl.clone()).and(hpc.clone().gt(lpc.clone())))
        .then(hpc)
        .when(lpc.clone().gt(hl.clone()))
        .then(lpc)
        .otherwise(hl)
        .alias("__tr")
}

fn rsv_expr(n: usize) -> Expr {
    let highest = col("high").rolling_max(strict(n));
    let lowest = col("low").rolling_min(strict(n));
    when(highest.clone().neq(lowest.clone()))
        .then((col("close") - lowest.clone()) / (highest - lowest))
        .otherwise(lit(0.0))
}

fn willr_expr(n: usize) -> Expr {
    let highest = col("high").rolling_max(strict(n));
    let lowest = col("low").rolling_min(strict(n));
    when(highest.clone().neq(lowest.clone()))
        .then((highest.clone() - col("close")) / (highest - lowest))
        .otherwise(lit(0.0))
}

/// Aroon distance ladder: first window offset (oldest first) whose bar holds
/// the extreme, i.e. the most-recent extreme wins ties, matching the
/// kernel's reversed argmax. Fraction scale, like the kernel.
fn aroon_expr(n: usize, use_high: bool) -> Expr {
    fn first_match(rungs: &[((Expr, Expr), usize)]) -> Expr {
        match rungs {
            [] => lit(f64::NAN),
            [((cond, _), k), rest @ ..] => {
                when(cond.clone()).then(lit(*k as f64)).otherwise(first_match(rest))
            }
        }
    }
    let nf = n as f64;
    let leg = if use_high { col("high") } else { col("low") };
    let extreme = if use_high {
        col("high").rolling_max(strict(n))
    } else {
        col("low").rolling_min(strict(n))
    };
    let rungs: Vec<((Expr, Expr), usize)> = (0..n)
        .map(|k| {
            let cond = if k == 0 {
                leg.clone().eq(extreme.clone())
            } else {
                leg.clone().shift(lit(k as i64)).eq(extreme.clone())
            };
            // Distance back, oldest rung last: the current bar wins ties,
            // matching the kernel's most-recent extreme.
            ((cond, leg.clone()), k)
        })
        .collect();
    (lit(1.0) - first_match(&rungs) / lit(nf)).alias(if use_high { "values" } else { "down" })
}

/// Shift-expanded ALMA: weights computed in Rust (bars-back order, matching
/// the kernel), applied through shifts so the first `n - 1` bars stay null.
fn alma_expr(n: usize) -> Expr {
    let (offset, sigma) = (0.85, 6.0);
    let center = offset * (n as f64 - 1.0);
    let spread = n as f64 / sigma;
    let weights: Vec<f64> = (0..n)
        .map(|bars_back| {
            let z = (bars_back as f64 - center) / spread;
            (-z * z / 2.0).exp()
        })
        .collect();
    let norm: f64 = weights.iter().sum();
    let mut sum: Option<Expr> = None;
    for (bars_back, weight) in weights.iter().enumerate() {
        let term = lit(*weight) * col("close").shift(lit(bars_back as i64));
        sum = Some(match sum {
            None => term,
            Some(acc) => acc + term,
        });
    }
    (sum.expect("nonempty weights") / lit(norm)).alias("values")
}

/// Staged reference legs per kernel id: `(leg name, values)` with the primary
/// leg first under `"values"`.
type ReferenceLegs = Vec<(&'static str, Vec<Option<f64>>)>;
fn reference(id: &str, frame: &DataFrame) -> Result<ReferenceLegs, KernelError> {
    let n = 14;
    let legs = match id {
        "sma" => vec![("values", single(frame, col("close").rolling_mean(strict(n)).alias("values"))?)],
        "ema" => vec![("values", single(frame, col("close").ewm_mean(ewm_span(n)).alias("values"))?)],
        "rma" => vec![("values", single(frame, col("close").ewm_mean(ewm_wilder(n)).alias("values"))?)],
        "wma" => vec![("values", single(frame, col("close").rolling_mean(weighted(n, linear_weights(n))).alias("values"))?)],
        "hma" => {
            // Shift-expanded weighted mean: the weighted `rolling_mean`
            // panics on null-bearing input in this polars version, so the
            // `2 * half - full` difference leg is folded through shifts
            // instead (nulls propagate through the arithmetic, matching the
            // kernel's `NaN`-poisoned windows position for position).
            let staged = frame
                .clone()
                .lazy()
                .with_columns([
                    col("close").rolling_mean(weighted(7, linear_weights(7))).alias("__half"),
                    col("close").rolling_mean(weighted(14, linear_weights(14))).alias("__full"),
                ])
                .with_columns([(col("__half") * lit(2.0) - col("__full")).alias("__diff")])
                .collect()
                .map_err(|e| KernelError::Research(format!("hma stage: {e}")))?;
            let root = 4;
            let norm: f64 = (1..=root).sum::<usize>() as f64;
            let mut sum: Option<Expr> = None;
            for bars_back in 0..root {
                // Kernel weights the `i`-th oldest bar `(i + 1) * n`; the bar
                // `bars_back` back is the `(n - 1 - bars_back)`-th oldest, and
                // the shared `* n` factor cancels in normalization.
                let term = lit((root - bars_back) as f64) * col("__diff").shift(lit(bars_back as i64));
                sum = Some(match sum {
                    None => term,
                    Some(acc) => acc + term,
                });
            }
            let cols = select_legs(
                &staged,
                vec![(sum.expect("nonempty root") / lit(norm)).alias("values")],
            )?;
            vec![("values", cols.into_iter().next().expect("one column"))]
        }
        "vwma" => vec![(
            "values",
            single(
                frame,
                ((col("close") * col("volume")).rolling_sum(strict(n)) / col("volume").rolling_sum(strict(n)))
                    .alias("values"),
            )?,
        )],
        "alma" => vec![("values", single(frame, alma_expr(n))?)],
        "dema" => {
            let staged = stage_ema_chain(frame, n, 2)?;
            let cols = select_legs(
                &staged,
                vec![(col("__e1") * lit(2.0) - col("__e2")).alias("values")],
            )?;
            vec![("values", cols.into_iter().next().expect("one column"))]
        }
        "tema" => {
            let staged = stage_ema_chain(frame, n, 3)?;
            let cols = select_legs(
                &staged,
                vec![(col("__e1") * lit(3.0) - col("__e2") * lit(3.0) + col("__e3")).alias("values")],
            )?;
            vec![("values", cols.into_iter().next().expect("one column"))]
        }
        "rsi" => {
            let chg = col("close") - col("close").shift(lit(1_i64));
            // Gains open with a true null (the kernel's gains open with
            // `NaN`): a `NaN` here would poison the `ewm_mean` state forever,
            // while the kernel re-seeds past it. Boolean-cast products keep
            // the null structural — null comparisons stay null through the
            // cast and the multiply.
            let float = polars::prelude::DataType::Float64;
            let up = chg.clone() * chg.clone().gt(lit(0.0)).cast(float.clone());
            let dn = -chg.clone() * chg.clone().lt(lit(0.0)).cast(float);
            let staged = frame
                .clone()
                .lazy()
                .with_columns([up.alias("__up"), dn.alias("__dn")])
                .with_columns([
                    col("__up").ewm_mean(ewm_wilder(n)).alias("__ag"),
                    col("__dn").ewm_mean(ewm_wilder(n)).alias("__al"),
                ])
                .select([(lit(100.0) - lit(100.0) / (lit(1.0) + col("__ag") / col("__al"))).alias("values")])
                .collect()
                .map_err(|e| KernelError::Research(format!("rsi stage: {e}")))?;
            vec![("values", column_of(&staged, "values")?)]
        }
        "macd" => {
            let staged = frame
                .clone()
                .lazy()
                .with_columns([
                    col("close").ewm_mean(ewm_span(12)).alias("__fast"),
                    col("close").ewm_mean(ewm_span(26)).alias("__slow"),
                ])
                .with_columns([(col("__fast") - col("__slow")).alias("__line")])
                .with_columns([col("__line").ewm_mean(ewm_span(9)).alias("signal")])
                .select([
                    col("__line").alias("values"),
                    col("signal"),
                    (col("__line") - col("signal")).alias("hist"),
                ])
                .collect()
                .map_err(|e| KernelError::Research(format!("macd stage: {e}")))?;
            vec![
                ("values", column_of(&staged, "values")?),
                ("signal", column_of(&staged, "signal")?),
                ("hist", column_of(&staged, "hist")?),
            ]
        }
        "stoch" => {
            let highest = col("high").rolling_max(strict(n));
            let lowest = col("low").rolling_min(strict(n));
            // No `otherwise` floor here (unlike `rsv`): a flat window is
            // `NaN` on both sides, surfacing as `None` through the funnel.
            let k = when(highest.clone().neq(lowest.clone()))
                .then(lit(100.0) * (col("close") - lowest.clone()) / (highest - lowest))
                .otherwise(lit(f64::NAN))
                .alias("__k");
            let staged = frame
                .clone()
                .lazy()
                .with_columns([k])
                .select([col("__k").alias("values"), col("__k").rolling_mean(strict(3)).alias("d")])
                .collect()
                .map_err(|e| KernelError::Research(format!("stoch stage: {e}")))?;
            vec![("values", column_of(&staged, "values")?), ("d", column_of(&staged, "d")?)]
        }
        "tr" => vec![("values", single(frame, tr_expr().alias("values"))?)],
        "atr" => {
            let staged = frame
                .clone()
                .lazy()
                .with_columns([tr_expr()])
                .select([col("__tr").ewm_mean(ewm_wilder(n)).alias("values")])
                .collect()
                .map_err(|e| KernelError::Research(format!("atr stage: {e}")))?;
            vec![("values", column_of(&staged, "values")?)]
        }
        "bb" => {
            let mid = col("close").rolling_mean(strict(n));
            let std = col("close").rolling_std(pop_opts(n));
            let staged = frame
                .clone()
                .lazy()
                .select([
                    mid.clone().alias("values"),
                    (mid.clone() + lit(2.0) * std.clone()).alias("upper"),
                    (mid - lit(2.0) * std).alias("lower"),
                ])
                .collect()
                .map_err(|e| KernelError::Research(format!("bb stage: {e}")))?;
            vec![
                ("values", column_of(&staged, "values")?),
                ("upper", column_of(&staged, "upper")?),
                ("lower", column_of(&staged, "lower")?),
            ]
        }
        "kc" => {
            let basis = col("close").rolling_mean(strict(n));
            let staged = frame
                .clone()
                .lazy()
                .with_columns([tr_expr()])
                .with_columns([col("__tr").rolling_mean(strict(n)).alias("__span")])
                .select([
                    basis.clone().alias("values"),
                    (basis.clone() + lit(2.0) * col("__span")).alias("upper"),
                    (basis - lit(2.0) * col("__span")).alias("lower"),
                ])
                .collect()
                .map_err(|e| KernelError::Research(format!("kc stage: {e}")))?;
            vec![
                ("values", column_of(&staged, "values")?),
                ("upper", column_of(&staged, "upper")?),
                ("lower", column_of(&staged, "lower")?),
            ]
        }
        "natr" => {
            // Ratio, not percent: the kernel divides without the `* 100`.
            let staged = frame
                .clone()
                .lazy()
                .with_columns([tr_expr()])
                .with_columns([col("__tr").ewm_mean(ewm_wilder(n)).alias("__atr")])
                .select([(col("__atr") / col("close")).alias("values")])
                .collect()
                .map_err(|e| KernelError::Research(format!("natr stage: {e}")))?;
            vec![("values", column_of(&staged, "values")?)]
        }
        "bbands" => {
            let mid = col("close").rolling_mean(strict(n));
            let std = col("close").rolling_std(pop_opts(n));
            let staged = frame
                .clone()
                .lazy()
                .select([
                    mid.clone().alias("values"),
                    (mid.clone() + lit(2.0) * std.clone()).alias("upper"),
                    (mid - lit(2.0) * std).alias("lower"),
                ])
                .collect()
                .map_err(|e| KernelError::Research(format!("bbands stage: {e}")))?;
            vec![
                ("values", column_of(&staged, "values")?),
                ("upper", column_of(&staged, "upper")?),
                ("lower", column_of(&staged, "lower")?),
            ]
        }
        "midpoint" => vec![(
            "values",
            single(
                frame,
                ((col("close").rolling_max(strict(n)) + col("close").rolling_min(strict(n))) / lit(2.0))
                    .alias("values"),
            )?,
        )],
        "midprice" => vec![(
            "values",
            single(
                frame,
                ((col("high").rolling_max(strict(n)) + col("low").rolling_min(strict(n))) / lit(2.0))
                    .alias("values"),
            )?,
        )],
        "trima" => vec![(
            "values",
            single(
                frame,
                col("close").rolling_mean(strict(7)).rolling_mean(strict(8)).alias("values"),
            )?,
        )],
        "avgprice" => vec![(
            "values",
            single(
                frame,
                ((col("open") + col("high") + col("low") + col("close")) / lit(4.0)).alias("values"),
            )?,
        )],
        "medprice" => vec![(
            "values",
            single(frame, ((col("high") + col("low")) / lit(2.0)).alias("values"))?,
        )],
        "typprice" => vec![(
            "values",
            single(
                frame,
                ((col("high") + col("low") + col("close")) / lit(3.0)).alias("values"),
            )?,
        )],
        "wclprice" => vec![(
            "values",
            single(
                frame,
                ((col("high") + col("low") + col("close") * lit(2.0)) / lit(4.0)).alias("values"),
            )?,
        )],
        "apo" => {
            let (fast, slow) = (5, 12);
            vec![(
                "values",
                single(
                    frame,
                    (col("close").rolling_mean(strict(fast)) - col("close").rolling_mean(strict(slow)))
                        .alias("values"),
                )?,
            )]
        }
        "ppo" => {
            let (fast, slow) = (5, 12);
            let (f, s) = (
                col("close").rolling_mean(strict(fast)),
                col("close").rolling_mean(strict(slow)),
            );
            vec![("values", single(frame, (f.clone() / s - lit(1.0)).alias("values"))?)]
        }
        "mom" => vec![(
            "values",
            single(frame, (col("close") - col("close").shift(lit(n as i64))).alias("values"))?,
        )],
        "roc" => {
            let prev = col("close").shift(lit(n as i64));
            vec![(
                "values",
                single(frame, ((col("close") - prev.clone()) / prev * lit(100.0)).alias("values"))?,
            )]
        }
        "rocp" => {
            let prev = col("close").shift(lit(n as i64));
            vec![("values", single(frame, ((col("close") - prev.clone()) / prev).alias("values"))?)]
        }
        "rocr" => vec![(
            "values",
            single(frame, (col("close") / col("close").shift(lit(n as i64))).alias("values"))?,
        )],
        "rocr100" => vec![(
            "values",
            single(
                frame,
                (col("close") / col("close").shift(lit(n as i64)) * lit(100.0)).alias("values"),
            )?,
        )],
        "rsv" => vec![("values", single(frame, rsv_expr(n).alias("values"))?)],
        "stochf" => {
            let staged = frame
                .clone()
                .lazy()
                .with_columns([rsv_expr(5).alias("__k")])
                .select([col("__k").alias("values"), col("__k").rolling_mean(strict(3)).alias("fastd")])
                .collect()
                .map_err(|e| KernelError::Research(format!("stochf stage: {e}")))?;
            vec![("values", column_of(&staged, "values")?), ("fastd", column_of(&staged, "fastd")?)]
        }
        "trix" => {
            let staged = stage_ema_chain(frame, 10, 3)?;
            let prev = col("__e3").shift(lit(1_i64));
            let cols = select_legs(&staged, vec![((col("__e3") - prev.clone()) / prev).alias("values")])?;
            vec![("values", cols.into_iter().next().expect("one column"))]
        }
        "willr" => vec![("values", single(frame, willr_expr(n).alias("values"))?)],
        "aroon" => {
            let staged = frame
                .clone()
                .lazy()
                .select([aroon_expr(n, true), aroon_expr(n, false)])
                .collect()
                .map_err(|e| KernelError::Research(format!("aroon stage: {e}")))?;
            vec![("values", column_of(&staged, "values")?), ("down", column_of(&staged, "down")?)]
        }
        "stddev" => vec![(
            "values",
            single(frame, (col("close").rolling_std(pop_opts(n)) * lit(1.0)).alias("values"))?,
        )],
        "var" => vec![(
            "values",
            single(frame, (col("close").rolling_var(pop_opts(n)) * lit(1.0)).alias("values"))?,
        )],
        "correl" => {
            // Staged moment form over shift-demeaned legs: per-window
            // demeaning is un-stageable through rolling ops, and the
            // raw-moment form cancels on price-scale inputs.
            let staged = frame
                .clone()
                .lazy()
                .with_columns([
                    (col("high") - col("high").first()).alias("__hx"),
                    (col("low") - col("low").first()).alias("__hy"),
                ])
                .with_columns([
                    col("__hx").rolling_sum(strict(n)).alias("__sx"),
                    col("__hy").rolling_sum(strict(n)).alias("__sy"),
                    (col("__hx") * col("__hy")).rolling_sum(strict(n)).alias("__sxy"),
                    (col("__hx") * col("__hx")).rolling_sum(strict(n)).alias("__sxx"),
                    (col("__hy") * col("__hy")).rolling_sum(strict(n)).alias("__syy"),
                ])
                .collect()
                .map_err(|e| KernelError::Research(format!("correl stage: {e}")))?;
            let nf = lit(n as f64);
            let num = col("__sxy") * nf.clone() - col("__sx") * col("__sy");
            let den = (col("__sxx") * nf.clone() - col("__sx") * col("__sx"))
                * (col("__syy") * nf.clone() - col("__sy") * col("__sy"));
            let sqrt = den.clone().sqrt();
            let cols = select_legs(
                &staged,
                vec![
                    when(sqrt.clone().eq(lit(0.0)))
                        .then(lit(f64::NAN))
                        .otherwise(num / sqrt)
                        .alias("values"),
                ],
            )?;
            vec![("values", cols.into_iter().next().expect("one column"))]
        }
        "ad" => {
            let mfm = when(col("high").neq(col("low")))
                .then(
                    ((col("close") - col("low")) - (col("high") - col("close")))
                        / (col("high") - col("low")),
                )
                .otherwise(lit(0.0));
            vec![(
                "values",
                single(frame, ((mfm * col("volume")).cum_sum(false)).alias("values"))?,
            )]
        }
        "adosc" => {
            let (fast, slow) = (3, 10);
            let mfm = when(col("high").neq(col("low")))
                .then(
                    ((col("close") - col("low")) - (col("high") - col("close")))
                        / (col("high") - col("low")),
                )
                .otherwise(lit(0.0));
            let staged = frame
                .clone()
                .lazy()
                .with_columns([(mfm * col("volume")).cum_sum(false).alias("__ad")])
                .select([(col("__ad").ewm_mean(ewm_span(fast)) - col("__ad").ewm_mean(ewm_span(slow)))
                    .alias("values")])
                .collect()
                .map_err(|e| KernelError::Research(format!("adosc stage: {e}")))?;
            vec![("values", column_of(&staged, "values")?)]
        }
        "obv" => {
            let d = col("close") - col("close").shift(lit(1_i64));
            // First bar counts long (`fill_null(1)` analog): a null
            // difference is the opening print, not a flat bar.
            let sign = when(d.clone().is_null()).then(lit(1.0)).otherwise(
                when(d.clone().gt(lit(0.0))).then(lit(1.0)).otherwise(
                    when(d.clone().lt(lit(0.0))).then(lit(-1.0)).otherwise(lit(0.0)),
                ),
            );
            vec![("values", single(frame, ((sign * col("volume")).cum_sum(false)).alias("values"))?)]
        }
        other => return Err(KernelError::Research(format!("no calibration reference for {other:?}"))),
    };
    Ok(legs)
}

/// Stage `depth` chained EMA passes over close as `__e1..=__eN`.
fn stage_ema_chain(frame: &DataFrame, n: usize, depth: usize) -> Result<DataFrame, KernelError> {
    const NAMES: [&str; 3] = ["__e1", "__e2", "__e3"];
    assert!((1..=3).contains(&depth), "ema chain depth 1..=3");
    let mut lazy = frame.clone().lazy();
    let mut prev = "close";
    for name in NAMES.iter().take(depth) {
        lazy = lazy.with_columns([col(prev).ewm_mean(ewm_span(n)).alias(*name)]);
        prev = name;
    }
    lazy.collect().map_err(|e| KernelError::Research(format!("ema chain stage: {e}")))
}

fn column_of(staged: &DataFrame, name: &str) -> Result<Vec<Option<f64>>, KernelError> {
    let series = staged
        .column(name)
        .map_err(|e| KernelError::Research(format!("calibration column {name}: {e}")))?;
    let cast = series
        .cast(&polars::prelude::DataType::Float64)
        .map_err(|e| KernelError::Research(format!("calibration cast: {e}")))?;
    let chunked = cast
        .f64()
        .map_err(|e| KernelError::Research(format!("calibration f64 view: {e}")))?;
    Ok((0..chunked.len()).map(|i| chunked.get(i).and_then(fin_or_none)).collect())
}
