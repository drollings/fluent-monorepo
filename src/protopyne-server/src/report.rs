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

    /// Deterministic JSON encoding of this manifest.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned())
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
) -> BacktestReport {
    let equity_decimals: Vec<Decimal> =
        equity_curve.iter().map(|money| money.as_decimal()).collect();
    let fill_rate = if proposals == 0 {
        None
    } else {
        let rate = f64::from(fills as u32) / f64::from(proposals as u32);
        Some(rate)
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
