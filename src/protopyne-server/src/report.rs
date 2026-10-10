//! Run manifest and honest backtest report.
//!
//! Every run is reproducible from **(data version, code version,
//! configuration)**: [`RunManifest`] pins all three plus the bar range, and
//! serializes deterministically (struct field order) so the same inputs
//! produce byte-identical JSON. [`BacktestReport`] states its fill
//! assumptions and data limits alongside its metrics, and marks unavailable
//! analytics (greeks need chain data the prototype has no feed for) as
//! unavailable rather than inventing them.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive as _;

use protopyne_types::{Bar, Money};

use crate::metrics::{self, TRADING_DAYS_PER_YEAR};

/// What was run, pinned for replay. Deterministic serialization order makes
/// reruns byte-identical: the same inputs always produce the same JSON.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunManifest {
    /// Pinned history identifier.
    pub data_version: String,
    /// Crate version at run time.
    pub code_version: String,
    /// Canonical JSON of the runner + venue + risk + grant configuration.
    pub config_json: String,
    /// Bars replayed.
    pub bar_count: u64,
    /// First bar timestamp (RFC 3339).
    pub first_ts: String,
    /// Last bar timestamp (RFC 3339).
    pub last_ts: String,
    /// Caller-supplied seed for any randomized sweep choice (0 = none).
    pub seed: u64,
}

impl RunManifest {
    /// Pin a manifest for `bars` under the given versions.
    pub fn new(
        data_version: impl Into<String>,
        config_json: impl Into<String>,
        bars: &[Bar],
        seed: u64,
    ) -> Self {
        let (first_ts, last_ts) = match (bars.first(), bars.last()) {
            (Some(first), Some(last)) => (
                first.ts.to_rfc3339(),
                last.ts.to_rfc3339(),
            ),
            (None, _) | (_, None) => (String::new(), String::new()),
        };
        Self {
            data_version: data_version.into(),
            code_version: env!("CARGO_PKG_VERSION").to_owned(),
            config_json: config_json.into(),
            bar_count: bars.len() as u64,
            first_ts,
            last_ts,
            seed,
        }
    }

    /// Deterministic JSON encoding of this manifest. Serialization of plain
    /// pin data cannot fail in practice; a failure still reports loudly
    /// inside the payload instead of emitting an empty object.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            format!(
                "{{\"serialization_error\":{}}}",
                serde_json::to_string(&e.to_string()).unwrap_or_else(|_| "\"unencodable\"".to_owned())
            )
        })
    }
}

/// Count bars whose close is non-positive (skipped by quote synthesis).
#[must_use]
pub fn count_unquotable_bars(bars: &[Bar]) -> u64 {
    bars.iter()
        .filter(|bar| bar.c <= Decimal::ZERO)
        .count() as u64
}

/// Peak-to-trough drawdown of an equity curve in account-currency units.
///
/// Axis: **(correctness)** invariant input, not a tunable. Empty curves draw
/// down zero.
#[must_use]
pub fn max_drawdown(equity: &[Decimal]) -> Decimal {
    let mut peak = Decimal::ZERO;
    let mut worst = Decimal::ZERO;
    let mut started = false;
    for value in equity {
        if !started || *value > peak {
            peak = *value;
            started = true;
        }
        let drop = peak - *value;
        if drop > worst {
            worst = drop;
        }
    }
    worst
}

/// Worst single-step equity decline (tail-loss proxy for the prototype).
///
/// Negative-or-zero steps are floored at zero: flat or rising steps are not
/// losses. Empty or single-point curves report zero.
#[must_use]
pub fn worst_step_loss(equity: &[Decimal]) -> Decimal {
    let mut worst = Decimal::ZERO;
    for window in equity.windows(2) {
        let step = window[0] - window[1];
        if step > worst {
            worst = step;
        }
    }
    worst
}

/// Total cumulative return of an equity curve: `last / base - 1` where the
/// base is the first deployed (nonzero) capital, the summary analog of the
/// reference skipping nulls to its first print.
///
/// Returns `None` when no capital was ever deployed (empty, single-point,
/// or all-zero curves): there is no base to compound from, and a zero is
/// not manufactured. Mid-curve zeros report `-1` (total loss), which is
/// defined, not missing.
#[must_use]
pub fn cum_return(equity: &[Decimal]) -> Option<Decimal> {
    if equity.len() < 2 {
        return None;
    }
    let base = equity.iter().find(|value| !value.is_zero())?;
    Some(equity[equity.len() - 1] / *base - Decimal::ONE)
}

/// Simple rate to log rate: `ln(1 + r)`.
///
/// Returns `None` when the log is undefined (`1 + r <= 0`) or the input is
/// non-finite: the reference yields `-inf`/`NaN` there, and the report
/// carries absence instead.
#[must_use]
pub fn simple_to_log_return(rate: Decimal) -> Option<Decimal> {
    use rust_decimal::prelude::ToPrimitive as _;
    let shifted = rate.to_f64()? + 1.0;
    if shifted.is_nan() || shifted <= 0.0 {
        return None;
    }
    Decimal::from_f64_retain(shifted.ln())
}

/// Log rate to simple rate: `exp(r) - 1`.
///
/// Returns `None` on non-finite input or overflow (the exponential leaves
/// `f64` range): honest absence, never an infinite decimal.
#[must_use]
pub fn log_to_simple_return(rate: Decimal) -> Option<Decimal> {
    use rust_decimal::prelude::ToPrimitive as _;
    let simple = rate.to_f64()?.exp() - 1.0;
    Decimal::from_f64_retain(simple)
}

/// Capital efficiency proxy: filled notional per unit of reserved notional.
///
/// Returns `None` when nothing was reserved (no efficiency to report rather
/// than a misleading zero-over-zero).
#[must_use]
pub fn capital_efficiency(filled_notional: Decimal, reserved_notional: Decimal) -> Option<f64> {
    if reserved_notional <= Decimal::ZERO || filled_notional < Decimal::ZERO {
        return None;
    }
    let filled = filled_notional.to_f64()?;
    let reserved = reserved_notional.to_f64()?;
    if reserved == 0.0 {
        return None;
    }
    Some(filled / reserved)
}

/// The honest report for one backtest run.
///
/// Metrics sit beside the assumptions and limits that qualify them. Greeks
/// need option-chain data the prototype has no feed for, so
/// `greeks_note` says so explicitly instead of shipping invented curves.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BacktestReport {
    /// What ran (pin for replay).
    pub manifest: RunManifest,
    /// How fills were priced (quote synthesis + venue params).
    pub fill_assumptions: Vec<String>,
    /// Data limits qualifying the numbers (gaps, skips, warmup).
    pub data_limits: Vec<String>,
    /// Bars replayed.
    pub bars: u64,
    /// Proposals emitted.
    pub proposals: u64,
    /// Orders filled.
    pub fills: u64,
    /// Fill rate over proposals (`None` when no proposal was emitted).
    pub fill_rate: Option<f64>,
    /// Commission paid.
    pub total_fees: Money,
    /// Market value filled (fill price × filled quantity, summed).
    pub filled_notional: Money,
    /// Peak-to-trough drawdown of the marked equity curve.
    pub drawdown: Money,
    /// Worst single-bar equity decline.
    pub tail_loss: Money,
    /// Filled over reserved notional (`None` when nothing reserved).
    pub capital_efficiency: Option<f64>,
    /// Total cumulative return of the marked equity curve (`None` when no
    /// capital was ever deployed).
    pub cum_return: Option<f64>,
    /// Latest RSRS timing score over the replayed bars (`None` when the
    /// history is shorter than the scoring window or a caller passes none).
    pub rsrs: Option<f64>,
    /// Annualized Sharpe ratio of per-step equity returns (`None` on flat,
    /// empty, or zero-variance curves). Annualized with
    /// `metrics::TRADING_DAYS_PER_YEAR` daily-equivalent steps against a zero
    /// risk-free baseline; bar-native callers recompute via
    /// `metrics::sharpe` with their own step size.
    #[serde(default)]
    pub sharpe: Option<f64>,
    /// Annualized Sortino ratio of per-step equity returns (`None` when
    /// nothing ever falls short of the zero baseline). Same annualization
    /// and baseline assumptions as [`BacktestReport::sharpe`]; recompute via
    /// `metrics::sortino` for other targets.
    #[serde(default)]
    pub sortino: Option<f64>,
    /// Annualized Calmar-style ratio: mean step return times
    /// `TRADING_DAYS_PER_YEAR` over the peak-to-trough fraction (`None` when
    /// the curve never draws down). Same annualization assumption as
    /// [`BacktestReport::sharpe`]; recompute via `metrics::calmar` otherwise.
    #[serde(default)]
    pub calmar: Option<f64>,
    /// Winning equity steps over decisive (non-flat) steps (`None` when no
    /// step moved). Flat pre-deployment bars are excluded, never counted as
    /// losses.
    #[serde(default)]
    pub win_rate: Option<f64>,
    /// Gross equity gains over gross equity losses (`None` for empty or
    /// all-win books; all-loss books report `Some(0.0)`).
    #[serde(default)]
    pub profit_factor: Option<f64>,
    /// Why greeks-over-time is unavailable in the prototype.
    pub greeks_note: String,
    /// Marked equity curve (per-bar, account currency).
    pub equity_curve: Vec<Money>,
}

impl BacktestReport {
    /// Greeks are unavailable without chain data; this is the single note
    /// every report carries so no consumer mistakes absence for zero.
    #[must_use]
    pub fn unavailable_greeks_note() -> String {
        "greeks-over-time unavailable: prototype has no option-chain feed".to_owned()
    }

    /// Deterministic JSON encoding of this report. Same inputs always
    /// produce the same bytes, so a rerun can prove it changed nothing.
    /// Serialization of plain report data cannot fail in practice; a failure
    /// still reports loudly inside the payload instead of emitting an empty
    /// object.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            format!(
                "{{\"serialization_error\":{}}}",
                serde_json::to_string(&e.to_string()).unwrap_or_else(|_| "\"unencodable\"".to_owned())
            )
        })
    }
}

/// Assemble a report from run facts. Pure function: same inputs, same
/// report.
#[must_use]
pub fn build_report(
    manifest: RunManifest,
    fill_assumptions: Vec<String>,
    data_limits: Vec<String>,
    proposals: u64,
    fills: u64,
    total_fees: Money,
    filled_notional: Money,
    reserved_notional: Money,
    equity_curve: Vec<Money>,
    rsrs: Option<f64>,
) -> BacktestReport {
    let equity_decimals: Vec<Decimal> =
        equity_curve.iter().map(|money| money.as_decimal()).collect();
    let cum_return = cum_return(&equity_decimals).and_then(|value| {
        use rust_decimal::prelude::ToPrimitive as _;
        value.to_f64()
    });
    let annual = Decimal::from(TRADING_DAYS_PER_YEAR);
    let step_rets = metrics::step_returns(&equity_decimals);
    let step_moves: Vec<Decimal> = equity_decimals
        .windows(2)
        .filter_map(|window| window[1].checked_sub(window[0]))
        .collect();
    let as_f64 = |value: Decimal| value.to_f64();
    let fill_rate = if proposals == 0 {
        None
    } else {
        // Direct `as f64` widening: the old `as u32` detour truncated counts
        // past 2^32 into a wrong rate.
        Some(fills as f64 / proposals as f64)
    };
    BacktestReport {
        manifest,
        fill_assumptions,
        data_limits,
        bars: manifest_bar_count(&equity_curve, proposals, fills),
        proposals,
        fills,
        fill_rate,
        total_fees,
        filled_notional,
        capital_efficiency: capital_efficiency(
            filled_notional.as_decimal(),
            reserved_notional.as_decimal(),
        ),
        drawdown: Money::new(max_drawdown(&equity_decimals)),
        tail_loss: Money::new(worst_step_loss(&equity_decimals)),
        cum_return,
        rsrs,
        sharpe: metrics::sharpe(&step_rets, Decimal::ZERO, annual).and_then(as_f64),
        sortino: metrics::sortino(&step_rets, Decimal::ZERO, annual).and_then(as_f64),
        calmar: metrics::calmar(&equity_decimals, Decimal::ZERO, annual).and_then(as_f64),
        win_rate: metrics::win_rate(&step_moves).and_then(as_f64),
        profit_factor: metrics::profit_factor(&step_moves).and_then(as_f64),
        greeks_note: BacktestReport::unavailable_greeks_note(),
        equity_curve,
    }
}

/// Bar count for the report: the equity curve has one entry per replayed bar.
fn manifest_bar_count(equity_curve: &[Money], _proposals: u64, _fills: u64) -> u64 {
    equity_curve.len() as u64
}

/// Describe one bar timestamp for manifest ranges.
#[must_use]
pub fn rfc3339(ts: &DateTime<Utc>) -> String {
    ts.to_rfc3339()
}

#[cfg(test)]
#[path = "../tests/report.rs"]
mod tests;
