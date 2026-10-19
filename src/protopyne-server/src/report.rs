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
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use protopyne_kernels::indicator::encode_param_value;
use protopyne_kernels::value::PineValue;
use protopyne_store::identity::VerifiedId;
use protopyne_store::plan::{SpanOrigin, SpanPlan};
use protopyne_types::{Bar, ChainQuote, Money};

use crate::backtest::HistorySource;
use crate::marks::MarkBasis;
use crate::metrics::{self, TRADING_DAYS_PER_YEAR};
use crate::portfolio::PortfolioReport;
use protopyne_venue::{CorporateEvent, CorporateEventArgs};

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
    /// Content digest of the replayed tape (16 hex chars), when the run was
    /// pinned to tape data. `None` keeps the pre-tape JSON byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_digest: Option<String>,
    /// First tape stamp (RFC 3339), parallel to [`RunManifest::first_ts`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_first_ts: Option<String>,
    /// Last tape stamp (RFC 3339), parallel to [`RunManifest::last_ts`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_last_ts: Option<String>,
    /// Tape bars covered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_bar_count: Option<u64>,
    /// How every served span was produced, in ascending order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_provenance: Option<Vec<SpanOriginSummary>>,
}

/// How one served span was produced: the planner's [`SpanPlan`] reduced to
/// JSON-stable facts. The origin rides as a string (not the store enum) so
/// the manifest never couples its serialized form to the store's type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpanOriginSummary {
    /// Where the span was served from (`"stored"` today; the string is the
    /// seam a second tier would extend without breaking old manifests).
    pub origin: String,
    /// Whether the span's buckets are whole. A `false` here is the planner
    /// reporting a span it served but could not certify — the manifest's
    /// honest remainder, surfaced in `data_limits`.
    pub complete: bool,
    /// Served span, lower bound inclusive. `None` means no lower bound.
    pub start_ms: Option<i64>,
    /// Served span, upper bound exclusive. `None` means no upper bound.
    pub end_ms: Option<i64>,
}

impl SpanOriginSummary {
    /// Reduce a planner span to its manifest facts.
    #[must_use]
    pub fn of(span: &SpanPlan) -> Self {
        let origin = match span.origin() {
            SpanOrigin::Stored => "stored",
        };
        Self {
            origin: origin.to_owned(),
            complete: span.complete(),
            start_ms: span.span().start_ms,
            end_ms: span.span().end_ms,
        }
    }
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
            (Some(first), Some(last)) => (first.ts.to_rfc3339(), last.ts.to_rfc3339()),
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
            data_digest: None,
            data_first_ts: None,
            data_last_ts: None,
            data_bar_count: None,
            data_provenance: None,
        }
    }

    /// Pin this manifest to tape data: the replayed bars plus the planner
    /// spans that served them. The digest identifies the exact bytes (a
    /// different coverage digests differently); the summaries name how
    /// every span was produced. A manifest with no tape keeps every new
    /// field `None` and serializes byte-identically to the pre-tape form.
    #[must_use]
    pub fn with_tape_data(mut self, bars: &[Bar], provenance: &[SpanPlan]) -> Self {
        let identity = VerifiedId::of(&tape_canonical_bytes(bars, provenance));
        let (data_first_ts, data_last_ts) = match (bars.first(), bars.last()) {
            (Some(first), Some(last)) => (Some(first.ts.to_rfc3339()), Some(last.ts.to_rfc3339())),
            (None, _) | (_, None) => (None, None),
        };
        self.data_digest = Some(format!("{:016x}", identity.hash()));
        self.data_first_ts = data_first_ts;
        self.data_last_ts = data_last_ts;
        self.data_bar_count = Some(bars.len() as u64);
        self.data_provenance = Some(provenance.iter().map(SpanOriginSummary::of).collect());
        self
    }

    /// Deterministic JSON encoding of this manifest. Serialization of plain
    /// pin data cannot fail in practice; a failure still reports loudly
    /// inside the payload instead of emitting an empty object.
    #[must_use]
    pub fn to_canonical_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            format!(
                "{{\"serialization_error\":{}}}",
                serde_json::to_string(&e.to_string())
                    .unwrap_or_else(|_| "\"unencodable\"".to_owned())
            )
        })
    }
}

/// Count bars whose close is non-positive (skipped by quote synthesis).
#[must_use]
pub fn count_unquotable_bars(bars: &[Bar]) -> u64 {
    bars.iter().filter(|bar| bar.c <= Decimal::ZERO).count() as u64
}

/// Canonical bytes for tape data: the exact replayed bars plus the planner
/// spans that served them, framed with the parameter encoding's
/// length-prefixed idiom — one framing implementation, reused, never
/// re-derived. Prices ride as normalized decimal strings (exact, never
/// float); volumes saturate instead of wrapping; order is the slice order,
/// so callers pass bars ascending and identical inputs byte-identify.
/// A different coverage frames different bytes and digests differently.
#[must_use]
pub fn tape_canonical_bytes(bars: &[Bar], provenance: &[SpanPlan]) -> Vec<u8> {
    fn push(bytes: &mut Vec<u8>, value: &PineValue) {
        encode_param_value(bytes, value);
    }
    fn text(value: Decimal) -> PineValue {
        PineValue::Str(value.normalize().to_string())
    }
    fn maybe_int(value: Option<i64>) -> PineValue {
        PineValue::Array(vec![
            Some(PineValue::Bool(value.is_some())),
            Some(PineValue::Int(value.unwrap_or(0))),
        ])
    }
    let mut bytes = Vec::new();
    push(&mut bytes, &PineValue::Str("tape-data-v1".to_owned()));
    push(
        &mut bytes,
        &PineValue::Int(i64::try_from(bars.len()).unwrap_or(i64::MAX)),
    );
    for bar in bars {
        push(&mut bytes, &PineValue::Int(bar.ts.timestamp_millis()));
        push(&mut bytes, &text(bar.o));
        push(&mut bytes, &text(bar.h));
        push(&mut bytes, &text(bar.l));
        push(&mut bytes, &text(bar.c));
        push(
            &mut bytes,
            &PineValue::Int(i64::try_from(bar.volume).unwrap_or(i64::MAX)),
        );
        let (split, dividend) = match &bar.adj {
            None => (None, None),
            Some(adj) => (adj.split_factor, adj.dividend.map(Money::as_decimal)),
        };
        push(
            &mut bytes,
            &PineValue::Array(vec![
                Some(PineValue::Array(vec![
                    Some(PineValue::Bool(split.is_some())),
                    Some(PineValue::Str(
                        split.map_or_else(String::new, |v| v.normalize().to_string()),
                    )),
                ])),
                Some(PineValue::Array(vec![
                    Some(PineValue::Bool(dividend.is_some())),
                    Some(PineValue::Str(
                        dividend.map_or_else(String::new, |v| v.normalize().to_string()),
                    )),
                ])),
            ]),
        );
    }
    push(
        &mut bytes,
        &PineValue::Int(i64::try_from(provenance.len()).unwrap_or(i64::MAX)),
    );
    for span in provenance {
        let origin = match span.origin() {
            SpanOrigin::Stored => "stored",
        };
        push(&mut bytes, &maybe_int(span.span().start_ms));
        push(&mut bytes, &maybe_int(span.span().end_ms));
        push(&mut bytes, &PineValue::Str(origin.to_owned()));
        push(&mut bytes, &PineValue::Bool(span.complete()));
    }
    bytes
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
    /// Portfolio aggregation over the run's fills (`None` for reports built
    /// before the aggregation existed — additive, rerun equality unaffected
    /// since the pin covers manifest plus decisions, never this).
    #[serde(default)]
    pub portfolio: Option<PortfolioReport>,
    /// How the run's option positions are marked for unrealized P&L. Additive:
    /// reports serialized before marks existed decode with the fill-price
    /// default.
    #[serde(default)]
    pub mark_basis: MarkBasis,
}

impl BacktestReport {
    /// Greeks-over-time is unavailable either way; the note says which
    /// absence it is, so a chained run never claims it held no chain feed.
    /// `chained` is the run's own source presence.
    #[must_use]
    pub fn unavailable_greeks_note(chained: bool) -> String {
        if chained {
            "greeks-over-time unavailable: the run holds option-chain data, but no greeks \
             time series is computed from it yet"
                .to_owned()
        } else {
            "greeks-over-time unavailable: prototype has no option-chain feed".to_owned()
        }
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
                serde_json::to_string(&e.to_string())
                    .unwrap_or_else(|_| "\"unencodable\"".to_owned())
            )
        })
    }
}

/// Where a run's bars came from: the one source fact `data_limits` names.
/// Reducing the source to this and rendering it in one place keeps the report
/// from claiming a "single CSV history" for a store-backed tape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    /// File candidates in fallback order.
    Files,
    /// Caller-supplied in-memory bars.
    Dynamic,
    /// A store-backed tape read.
    Tape,
}

impl HistoryKind {
    /// Reduce a source arm. The report names the arm the run replayed; there
    /// is no second place that decides which arm a source is.
    #[must_use]
    pub fn of(source: &HistorySource) -> Self {
        match source {
            HistorySource::Files(_) => Self::Files,
            HistorySource::Dynamic(_) => Self::Dynamic,
            HistorySource::Tape { .. } => Self::Tape,
        }
    }

    /// The arm's noun in `data_limits`.
    const fn label(self) -> &'static str {
        match self {
            Self::Files => "file",
            Self::Dynamic => "in-memory",
            Self::Tape => "store-backed",
        }
    }
}

/// The declared capability set every prototype run lacks: option chains,
/// corporate-action adjustments, multi-symbol coverage, and point-in-time
/// membership. One constant, so the honest absences cannot drift apart.
pub const ABSENT_CAPABILITIES: &str = "no corporate-action adjustments (cash dividends \
     excluded from P&L); no option-chain data (greeks unavailable); single symbol; \
     no point-in-time universe";

/// The same declaration for a run that priced legs off option-chain data:
/// the chain clause drops — the chain-data line names the coverage instead
/// — while every other absence stands.
pub const ABSENT_CAPABILITIES_WITH_CHAINS: &str =
    "no corporate-action adjustments (cash dividends \
     excluded from P&L); single symbol; no point-in-time universe";

/// The chain-data line for `data_limits`: the run's chain coverage (quote
/// count plus stamped range) beside the metrics, so a spread-priced run
/// states what it traded off. `None` when the run held no chain source, in
/// which case reports read exactly as before.
#[must_use]
pub fn chain_data_limit_line(quotes: &[ChainQuote]) -> Option<String> {
    if quotes.is_empty() {
        return None;
    }
    let (first, last) = quotes.iter().fold((None, None), |(low, high), quote| {
        let ts = quote.timestamp;
        (
            Some(low.map_or(ts, |held: chrono::DateTime<chrono::Utc>| held.min(ts))),
            Some(high.map_or(ts, |held: chrono::DateTime<chrono::Utc>| held.max(ts))),
        )
    });
    let (first, last) = (first?, last?);
    let count = quotes.len();
    let noun = if count == 1 {
        "1 quote".to_owned()
    } else {
        format!("{count} quotes")
    };
    Some(format!(
        "option-chain data: {noun} {}..{}",
        rfc3339(&first),
        rfc3339(&last)
    ))
}

/// The source line for `data_limits`: the arm, its pinned version, and its
/// bar count, derived from the source rather than asserted.
#[must_use]
pub fn history_source_limit(kind: HistoryKind, data_version: &str, bar_count: usize) -> String {
    format!(
        "{} history '{data_version}', {bar_count} bars",
        kind.label()
    )
}

/// The corporate-action tape line for `data_limits`: the event count and its
/// mix. `None` when the run consumed no tape, so untaped reports read exactly
/// as before.
#[must_use]
pub fn corporate_action_limit_line(actions: &[CorporateEventArgs]) -> Option<String> {
    if actions.is_empty() {
        return None;
    }
    let (mut expiry, mut assignment, mut exercise, mut dividend, mut split) = (0, 0, 0, 0, 0);
    for action in actions {
        match action.event {
            CorporateEvent::Expiry => expiry += 1,
            CorporateEvent::Assignment => assignment += 1,
            CorporateEvent::Exercise => exercise += 1,
            CorporateEvent::Dividend => dividend += 1,
            CorporateEvent::Split(_) => split += 1,
        }
    }
    Some(format!(
        "corporate-action tape: {} event(s) (expiry {expiry}, assignment {assignment}, \
         exercise {exercise}, dividend {dividend}, split {split})",
        actions.len()
    ))
}

/// The coverage-omission line: a source with no planner provenance has an
/// uncertified coverage the report must name, never let read as certified.
/// `None` when the source is planner-backed — the tape line off the manifest
/// names that coverage instead.
#[must_use]
pub fn uncertified_coverage_limit(planner_certified: bool) -> Option<String> {
    (!planner_certified).then(|| "no planner provenance: coverage not certified".to_owned())
}

/// The tape-data line for `data_limits`: the tape's coverage (bar count
/// plus range) and any span the planner served but could not certify whole.
/// A partial span is the manifest's honest remainder — the buckets whose
/// edges the planner flagged — reported here so the tearsheet and the JSON
/// name the same unresolved gaps. `None` when the manifest carries no tape
/// data, in which case reports read exactly as before.
#[must_use]
pub fn tape_data_limit_line(manifest: &RunManifest) -> Option<String> {
    let (digest, first, last, count, provenance) = (
        manifest.data_digest.as_ref()?,
        manifest.data_first_ts.as_ref()?,
        manifest.data_last_ts.as_ref()?,
        manifest.data_bar_count?,
        manifest.data_provenance.as_ref()?,
    );
    let whole = provenance.iter().filter(|span| span.complete).count();
    let partial = provenance.len() - whole;
    let spans = if provenance.len() == 1 {
        "1 stored span".to_owned()
    } else {
        format!("{} stored spans", provenance.len())
    };
    let certified = if partial == 0 {
        format!("{whole} whole")
    } else {
        format!("{whole} whole, {partial} partial")
    };
    Some(format!(
        "tape data: {count} bars {first}..{last} digest {} | {spans} ({certified})",
        digest.chars().take(8).collect::<String>(),
    ))
}

/// Assemble a report from run facts. Pure function: same inputs, same
/// report. `chain_quotes` carries the run's chain source facts (`None` when
/// the run held none): a chained run names its coverage in `data_limits`,
/// drops the no-chains clause from the absence line, and says which greeks
/// absence it has, while a chainless run reads exactly as before — absence
/// line verbatim, no chain line. `mark_basis` is the run's own mark basis,
/// read from the same facts.
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
    chain_quotes: Option<&[ChainQuote]>,
    mark_basis: MarkBasis,
) -> BacktestReport {
    let equity_decimals: Vec<Decimal> = equity_curve
        .iter()
        .map(|money| money.as_decimal())
        .collect();
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
    let mut data_limits = data_limits;
    if let Some(tape_line) = tape_data_limit_line(&manifest) {
        data_limits.push(tape_line);
    }
    if let Some(chain_line) = chain_quotes.and_then(chain_data_limit_line) {
        for limit in &mut data_limits {
            if *limit == ABSENT_CAPABILITIES {
                ABSENT_CAPABILITIES_WITH_CHAINS.clone_into(limit);
            }
        }
        data_limits.push(chain_line);
    }
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
        greeks_note: BacktestReport::unavailable_greeks_note(chain_quotes.is_some()),
        equity_curve,
        portfolio: None,
        mark_basis,
    }
}

/// Render one human-readable tearsheet from the same structs the JSON report
/// serializes: every line reads a report field, so text and JSON cannot
/// disagree. Absent stats print `n/a` (never zero); money prints in its
/// exact decimal form. The portfolio section appears only when the report
/// carries an aggregation.
#[must_use]
pub fn render_tearsheet(report: &BacktestReport) -> String {
    fn stat(value: Option<f64>) -> String {
        value.map_or_else(|| "n/a".to_owned(), |number| number.to_string())
    }
    // An absent mark prints `n/a`, never a zero that never existed.
    fn money_opt(value: Option<Money>) -> String {
        value.map_or_else(|| "n/a".to_owned(), |money| money.to_string())
    }
    let mut rows = vec![
        "backtest tearsheet".to_owned(),
        format!(
            "manifest: data={} code={} bars={} seed={}",
            report.manifest.data_version,
            report.manifest.code_version,
            report.manifest.bar_count,
            report.manifest.seed,
        ),
        format!(
            "proposals: {} fills: {} fill_rate: {}",
            report.proposals,
            report.fills,
            stat(report.fill_rate),
        ),
        format!(
            "fees: {} notional: {} efficiency: {}",
            report.total_fees,
            report.filled_notional,
            stat(report.capital_efficiency),
        ),
        format!(
            "drawdown: {} tail: {} cum_return: {} rsrs: {}",
            report.drawdown,
            report.tail_loss,
            stat(report.cum_return),
            stat(report.rsrs),
        ),
        format!(
            "sharpe: {} sortino: {} calmar: {} win_rate: {} profit_factor: {}",
            stat(report.sharpe),
            stat(report.sortino),
            stat(report.calmar),
            stat(report.win_rate),
            stat(report.profit_factor),
        ),
    ];
    match &report.portfolio {
        None => rows.push("portfolio: n/a".to_owned()),
        Some(summary) => {
            for totals in &summary.portfolios {
                rows.push(format!(
                    "portfolio {}: realized {} unrealized {} fees {} fills {} closed {} open {}",
                    totals.portfolio,
                    totals.pnl_realised,
                    money_opt(totals.pnl_unrealised),
                    totals.fees,
                    totals.fills,
                    totals.closed_trades,
                    totals.open_quantity,
                ));
                for bucket in &totals.buckets {
                    let open = bucket
                        .open_side
                        .map_or_else(|| "flat".to_owned(), |side| side.to_string());
                    rows.push(format!(
                        "bucket {}/{}/{}/{}/{}: realized {} unrealized {} fees {} fills {} closed {} open {} {open}",
                        bucket.key.portfolio,
                        bucket.key.underlying,
                        bucket.key.structure,
                        bucket.key.strategy,
                        bucket.key.grant,
                        bucket.pnl_realised,
                        money_opt(bucket.pnl_unrealised),
                        bucket.fees,
                        bucket.fills,
                        bucket.closed_trades,
                        bucket.open_quantity,
                    ));
                }
                rows.push(format!(
                    "portfolio stats: win_rate {} profit_factor {} sharpe {} sortino {} calmar {}",
                    stat(totals.win_rate),
                    stat(totals.profit_factor),
                    stat(totals.sharpe),
                    stat(totals.sortino),
                    stat(totals.calmar),
                ));
            }
        }
    }
    rows.push("assumptions:".to_owned());
    for assumption in &report.fill_assumptions {
        rows.push(format!("- {assumption}"));
    }
    rows.push("limits:".to_owned());
    for limit in &report.data_limits {
        rows.push(format!("- {limit}"));
    }
    rows.push(format!("greeks: {}", report.greeks_note));
    let mut text = rows.join("\n");
    text.push('\n');
    text
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

#[cfg(test)]
#[path = "../tests/manifest_backward_compatible.rs"]
mod tests_manifest_backward_compatible;

#[cfg(test)]
#[path = "../tests/manifest_rerun_equality.rs"]
mod tests_manifest_rerun_equality;

#[cfg(test)]
#[path = "../tests/report_data_limits.rs"]
mod tests_report_data_limits;
