//! HTTP surface over the backtest stack: health, proposals, grants, portfolio,
//! charts, and a bar feed.
//!
//! Every mutating preview runs the real decision stack
//! (`Strategy::propose`, `RiskEngine`, pure `evaluate_grant`,
//! `Venue::submit`) through [`run_backtest`](crate::runner::run_backtest) and
//! publishes the verdicts to the proposal inbox the approval UI reads.
//! Endpoint concurrency is capped by the shared
//! [`Limiter`](fluent_concurrency::pool::Limiter); the rehearsal feed pump is
//! a server-owned task supervised by an owned
//! [`Scope`](fluent_concurrency::scope::Scope); boot gates run under
//! containment-only [`SupervisedBatch`](fluent_concurrency::batch::SupervisedBatch)
//! supervision; configuration loads through
//! [`load_json_or_default`](common_core::config::load_json_or_default).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{
    Router,
    extract::{Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
};
use chrono::Utc;
use fluent_concurrency::batch::SupervisedBatch;
use fluent_concurrency::pool::Limiter;
use fluent_concurrency::scope::Scope;
use fluent_wvr::prelude::*;
use tokio::sync::broadcast;

use protopyne_kernels::{
    BbIndicator, EmaIndicator, Indicator as _, Ohlcv, ParamSet, SmaIndicator, TrimaIndicator,
};
use protopyne_strategy::{
    BreakerLimits, Direction, FixedSignal, Grant, GrantBudgets, GrantPermission, GrantScope,
    PortfolioThresholds, ProposalState, RiskEngine, RiskLimits, SignalThresholds,
    Strategy, StructureKind, TradeThresholds,
};
use protopyne_types::{Bar, Money};
use protopyne_venue::{Position, StateHandle, TradingState};

use crate::error::ServerError;
use crate::runner::{BacktestRun, RunnerConfig, run_backtest};
use crate::backtest::BacktestConfig;

/// Server tunables, loadable from JSON with defaults for every field.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct ServerConfig {
    /// Socket to bind, e.g. `127.0.0.1:9060`.
    #[field(desc = "Bind address")]
    #[builder(default = "127.0.0.1:9060".to_owned())]
    pub bind: String,
    /// Bar history path. Required at boot when set nowhere else.
    #[field(desc = "Bar history CSV path")]
    pub history_path: Option<String>,
    /// Maximum concurrent preview executions **(correctness-as-boundedness)**.
    #[field(desc = "Max concurrent previews", min = 1, max = 1024)]
    #[builder(default = 32)]
    pub max_concurrent_requests: u64,
    /// Broadcast channel depth for the rehearsal feed. Slow subscribers
    /// lag-skip by policy (see `feed`); the pump never blocks, so depth
    /// bounds memory, not throughput.
    #[field(desc = "Feed channel depth", min = 16, max = 100_000)]
    #[builder(default = 256)]
    pub feed_capacity: u64,
    /// Milliseconds between rehearsal feed frames.
    #[field(desc = "Feed frame interval in milliseconds", min = 1, max = 60_000)]
    #[builder(default = 50)]
    pub feed_interval_ms: u64,
}

impl protopyne_types::Validate for ServerConfig {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        ServerConfig::validate(self)
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl ServerConfig {
    /// Fail-closed self-consistency check shared by supervision and boot.
    pub fn validate(&self) -> Result<(), String> {
        self.bind
            .parse::<SocketAddr>()
            .map(|_| ())
            .map_err(|_| format!("bind '{}' is not a socket address", self.bind))?;
        if self.max_concurrent_requests < 1 {
            return Err("max_concurrent_requests must be at least 1".to_owned());
        }
        if self.feed_capacity < 16 {
            return Err("feed_capacity must be at least 16".to_owned());
        }
        Ok(())
    }
}

impl WorkUnit for ServerConfig {
    fn name(&self) -> &str {
        "http_server_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        protopyne_types::check_for_execute(self, "server config invalid")?;
        Ok(protopyne_types::valid_output("server config valid"))
    }
}

impl_component!(ServerConfig);

/// One inbox row: what the strategy proposed and where it stands.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProposalView {
    /// Stable proposal identifier.
    pub id: String,
    /// Strategy attribution.
    pub strategy: String,
    /// Underlying symbol.
    pub underlying: String,
    /// Direction of exposure.
    pub direction: Direction,
    /// Option structure.
    pub structure: StructureKind,
    /// Limit price submitted.
    pub limit_price: Money,
    /// Signal conviction of the bundled preview signal.
    pub conviction: f64,
    /// Lifecycle state after the preview run.
    pub state: ProposalState,
}

/// Aggregate portfolio over the latest preview run.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PortfolioView {
    /// Net positions by underlying.
    pub positions: Vec<Position>,
    /// Orders filled in the latest run.
    pub filled_orders: u64,
    /// Commission paid in the latest run.
    pub total_fees: Money,
    /// Net signed contracts across positions.
    pub open_contracts: i64,
    /// Trading state when the view was published: halt/resume status rides
    /// the existing portfolio route (additive — older readers ignore it).
    pub trading_state: TradingState,
}

impl Default for PortfolioView {
    fn default() -> Self {
        Self {
            positions: Vec::new(),
            filled_orders: 0,
            total_fees: Money::from(0),
            open_contracts: 0,
            // Views boot allowing everything, matching a fresh session.
            trading_state: TradingState::Enabled,
        }
    }
}

/// Last operator command the service observed: halt, resume, close-all, or
/// cancel-all status for the existing routes to report. Commands themselves
/// enter through the journaled command plane; this is the read-only echo.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandStatus {
    /// Command kind (`halt`, `resume`, `close_all`, `cancel_all`).
    pub kind: String,
    /// Human-readable outcome.
    pub detail: String,
    /// Outcome time in epoch milliseconds.
    pub at_ms: i64,
}

/// Worst-case exposure of an armed grant, shown before confirmation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorstCase {
    /// Maximum trades authorized.
    pub max_trades: u32,
    /// Maximum contracts authorized.
    pub max_contracts: u32,
    /// Maximum committed loss authorized.
    pub max_loss: Money,
    /// Maximum committed notional authorized.
    pub max_notional: Money,
}

/// Response to a successful arming.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ArmResponse {
    /// Grant identifier armed.
    pub grant_id: String,
    /// Worst case if every budget is consumed at its limit.
    pub worst_case: WorstCase,
    /// Always true on success.
    pub armed: bool,
}

/// Preview request parameters.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PreviewParams {
    /// Seed for deterministic proposal identifiers.
    #[serde(default)]
    pub seed: u64,
}

/// One deny reason and how many bars it kept out of the venue.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DenyCount {
    /// The reason, as recorded on the decision.
    pub reason: String,
    /// Bars carrying this reason.
    pub bars: u64,
}

/// Preview outcome: the run plus why bars did not trade.
///
/// The histogram reports outcome counts (a property of the run), never a
/// confidence score: nothing here measures a component's self-doubt, and
/// nothing fires on these counts. Authorization behavior is unchanged — the
/// same grants produce the same verdicts, now with visible reasons.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PreviewResponse {
    /// The backtest run, unchanged in shape.
    pub run: BacktestRun,
    /// Deny reasons across the run, most frequent first.
    #[serde(default)]
    pub deny_histogram: Vec<DenyCount>,
}

/// One Lightweight-Charts candle row.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CandleRow {
    /// Bar close time as epoch seconds.
    pub time: i64,
    /// Open as chart-native float.
    pub open: f64,
    /// High as chart-native float.
    pub high: f64,
    /// Low as chart-native float.
    pub low: f64,
    /// Close as chart-native float.
    pub close: f64,
    /// Traded volume.
    pub volume: u64,
}

/// Chart bars payload: rows plus how many bars never made it.
///
/// A wrapper (not a header) so JSON clients read one shape.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChartBars {
    /// Chartable bars in history order.
    pub rows: Vec<CandleRow>,
    /// Bars skipped for non-finite or unrepresentable floats.
    #[serde(default)]
    pub dropped: u64,
}

/// One overlay series (indicator values aligned with chart bars).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OverlaySeries {
    /// Kernel id (`sma`, `ema`, `bb`, or `trima`).
    pub kind: String,
    /// Averaging length.
    pub period: i64,
    /// Per-bar values (`None` inside warmup, or where the bar's close has
    /// no chart float). Indices always match bar indices — values are never
    /// dropped, so rows and overlays agree positionally.
    pub values: Vec<Option<f64>>,
    /// Bars without a value (unrepresentable close), mirroring
    /// `ChartBars.dropped`: counted, never shifted.
    #[serde(default)]
    pub dropped: u64,
}

/// Machine-readable error body.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ErrorBody {
    /// What failed.
    pub error: String,
}

/// Shared server state behind one `Arc`.
pub struct AppState {
    limiter: Limiter,
    history: Vec<Bar>,
    inbox: Mutex<Vec<ProposalView>>,
    armed: Mutex<Option<Grant>>,
    portfolio: Mutex<PortfolioView>,
    feed: broadcast::Sender<String>,
    /// Command-plane authority the bars gate on: shared ownership so an
    /// operator plane driving the same handle halts and resumes this
    /// surface. Serving-path reads only — no route mutates it.
    trading_state: StateHandle,
    /// Read-only echo of the last operator command, if any.
    last_command: Mutex<Option<CommandStatus>>,
}

type Shared = Arc<AppState>;
type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorBody>)>;

fn api_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (status, Json(ErrorBody { error: message.into() }))
}

/// The bundled preview signal: fixed long exposure for rehearsal.
/// Constants sit inside `[0, 1]` by construction; any refusal is a bug the
/// caller sees as a failed preview, never a fallback signal.
fn preview_strategy() -> Result<Arc<Strategy>, ServerError> {
    let signal = FixedSignal::new(Direction::Long, 0.8, 0.8)
        .map_err(|e| ServerError::Service(format!("bundled signal refused: {e}")))?;
    Ok(Arc::new(Strategy::with_signal(Arc::new(signal))))
}

/// Permissive rehearsal grant used only while no live grant is armed.
/// The window covers the vendored history range by construction; evaluation
/// happens at bar time, not wall-clock time.
fn permissive_grant() -> Grant {
    Grant::new()
        .id("preview-permissive".to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(vec![StructureKind::Vertical])
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission { entries: true, exits: true, rolls: false })
                .build(),
        )
        .budgets(
            GrantBudgets::new()
                .max_trades(1_000)
                .max_contracts(10_000)
                .max_loss(Money::from(1_000_000))
                .max_notional(Money::from(100_000_000))
                .build(),
        )
        .signal(
            SignalThresholds::new()
                .min_conviction(0.0)
                .min_activity_score(0.0)
                .build(),
        )
        .trade(
            TradeThresholds::new()
                .max_loss_per_trade(Money::from(1_000))
                .max_spread_width(Money::from(50))
                .min_dte_days(1)
                .max_dte_days(90)
                .build(),
        )
        .portfolio(
            PortfolioThresholds::new()
                .max_open_risk(Money::from(10_000_000))
                .quote_max_age_secs(60)
                .build(),
        )
        .breakers(BreakerLimits::new().max_consecutive_errors(3).build())
        .valid_from(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_else(Utc::now))
        .valid_until(chrono::DateTime::from_timestamp(1_900_000_000, 0).unwrap_or_else(Utc::now))
        .build()
}

fn worst_case_of(grant: &Grant) -> WorstCase {
    WorstCase {
        max_trades: grant.budgets.max_trades,
        max_contracts: grant.budgets.max_contracts,
        max_loss: grant.budgets.max_loss,
        max_notional: grant.budgets.max_notional,
    }
}

/// Whether `next` only tightens `current`. The domain owns the rule set
/// (`Grant::tightens`, mirroring `Grant::tighten` tier for tier); this stays
/// a thin wrapper so the route's refusal carries the route's error mapping
/// and every tier the domain refuses, the route refuses.
fn is_tightening(next: &Grant, current: &Grant) -> bool {
    next.tightens(current)
}

fn validate_grant(grant: &Grant) -> Result<(), String> {
    protopyne_types::Validate::validate(grant)
}

/// Liveness plus command-plane status: the trading state (halt/resume) and
/// the last operator command ride the existing health route — additive
/// fields, no new route, no new auth.
async fn health(State(state): State<Shared>) -> Json<serde_json::Value> {
    let trading = state.trading_state.get();
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "trading_state": trading,
        "allows_entries": trading.allows(false),
        "allows_exits": trading.allows(true),
        "last_command": common_core::sync::lock(&state.last_command).clone(),
    }))
}

async fn proposals(State(state): State<Shared>) -> Json<Vec<ProposalView>> {
    Json(common_core::sync::lock(&state.inbox).clone())
}

async fn preview(
    State(state): State<Shared>,
    Json(params): Json<PreviewParams>,
) -> ApiResult<PreviewResponse> {
    let run = state
        .limiter
        .run(|| async {
            let strategy = preview_strategy()?;
            let grant = {
                let armed = common_core::sync::lock(&state.armed);
                match armed.clone() {
                    Some(grant) if grant.is_live(Utc::now()) => grant,
                    _ => permissive_grant(),
                }
            };
            let runner = RunnerConfig::new(
                "SPY".to_owned(),
                "ribbon".to_owned(),
                Money::from(400),
                Money::from(2_000),
                30,
                0,
            );
            let venue = BacktestConfig::new()
                .underlying("SPY".to_owned())
                .data_version("http-preview".to_owned())
                .build();
            run_backtest(
                &strategy,
                &RiskEngine::new(RiskLimits::new().build()),
                &grant,
                &venue,
                &runner,
                &state.history,
                params.seed,
            )
            .await
        })
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    publish_run(&state, &run);
    let deny_histogram = deny_histogram_of(&run);
    Ok(Json(PreviewResponse { run, deny_histogram }))
}

/// Maximum deny reasons surfaced on a preview; the long tail folds into one
/// `other` bucket so the response stays small.
const DENY_HISTOGRAM_TOP: usize = 8;

/// Count why bars did not trade: skip reasons plus risk and grant refusal
/// reasons, most frequent first. Pure reporting — nothing authorizes or
/// denies on these counts.
fn deny_histogram_of(run: &BacktestRun) -> Vec<DenyCount> {
    let mut counts: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for decision in &run.decisions {
        if let Some(reason) = decision.skipped.as_deref() {
            *counts.entry(reason).or_insert(0) += 1;
        }
        for reason in decision.risk_reasons.iter().chain(decision.grant_reasons.iter()) {
            *counts.entry(reason.as_str()).or_insert(0) += 1;
        }
    }
    let mut histogram: Vec<DenyCount> = counts
        .into_iter()
        .map(|(reason, bars)| DenyCount { reason: reason.to_owned(), bars })
        .collect();
    histogram.sort_by(|left, right| {
        right.bars.cmp(&left.bars).then_with(|| left.reason.cmp(&right.reason))
    });
    if histogram.len() > DENY_HISTOGRAM_TOP {
        let rest: u64 = histogram[DENY_HISTOGRAM_TOP..].iter().map(|entry| entry.bars).sum();
        histogram.truncate(DENY_HISTOGRAM_TOP);
        histogram.push(DenyCount { reason: "other".to_owned(), bars: rest });
    }
    histogram
}

/// Record a preview run's verdicts in the inbox and portfolio views.
fn publish_run(state: &Shared, run: &BacktestRun) {
    let mut inbox = common_core::sync::lock(&state.inbox);
    inbox.clear();
    for decision in &run.decisions {
        if let Some(direction) = decision.direction {
            inbox.push(ProposalView {
                id: decision.proposal_id.clone(),
                strategy: "ribbon".to_owned(),
                underlying: "SPY".to_owned(),
                direction,
                structure: StructureKind::Vertical,
                limit_price: decision.limit_price,
                conviction: 0.8,
                state: if decision.submitted {
                    ProposalState::Filled
                } else {
                    ProposalState::Proposed
                },
            });
        }
    }
    // Preview-run attribution fold, grouped by indexed symbol key through a
    // run-local table (never global): same grouping as before, `Copy`-key
    // lookups on the fold path, strings restored at the `Position`
    // boundary so the wire shape is unchanged.
    let mut symbols = protopyne_types::SymbolIndex::new();
    let mut positions: std::collections::HashMap<
        protopyne_types::SymKey,
        (i64, rust_decimal::Decimal, u64),
    > = std::collections::HashMap::new();
    let mut fees = rust_decimal::Decimal::ZERO;
    let mut filled_orders: u64 = 0;
    for decision in &run.decisions {
        if decision.submitted {
            filled_orders += 1;
            let price = decision.fill_price.map_or(rust_decimal::Decimal::ZERO, Money::as_decimal);
            let fee = decision.fee.map_or(rust_decimal::Decimal::ZERO, Money::as_decimal);
            fees += fee;
            let signed: i64 = match decision.direction {
                Some(Direction::Short) => -1,
                _ => 1,
            };
            let Ok(key) = symbols.intern("SPY") else {
                continue;
            };
            let entry = positions.entry(key).or_insert((0, rust_decimal::Decimal::ZERO, 0));
            entry.0 += signed;
            entry.1 += price;
            entry.2 += 1;
        }
    }
    let trading_state = state.trading_state.get();
    let mut portfolio = common_core::sync::lock(&state.portfolio);
    let mut positions: Vec<Position> = positions
        .into_iter()
        .map(|(key, (quantity, price_sum, count))| {
            let avg = if count == 0 {
                Money::from(0)
            } else {
                Money::new(price_sum / rust_decimal::Decimal::from(count))
            };
            let underlying = symbols.resolve(key).unwrap_or("SPY").to_owned();
            Position::new().underlying(underlying).quantity(quantity).avg_price(avg).build()
        })
        .collect();
    positions.sort_by(|left, right| left.underlying.cmp(&right.underlying));
    portfolio.positions = positions;
    portfolio.filled_orders = filled_orders;
    portfolio.total_fees = Money::new(fees);
    portfolio.open_contracts = portfolio.positions.iter().map(|position| position.quantity).sum();
    portfolio.trading_state = trading_state;
}

async fn arm(
    State(state): State<Shared>,
    Json(grant): Json<Grant>,
) -> Result<Json<ArmResponse>, (StatusCode, Json<ErrorBody>)> {
    validate_grant(&grant).map_err(|e| api_error(StatusCode::BAD_REQUEST, e))?;
    let mut armed = common_core::sync::lock(&state.armed);
    if let Some(current) = armed.clone() {
        if !is_tightening(&grant, &current) {
            return Err(api_error(
                StatusCode::CONFLICT,
                "arming must tighten or revoke: loosening needs a new grant after revocation",
            ));
        }
    }
    let response = ArmResponse {
        grant_id: grant.id.clone(),
        worst_case: worst_case_of(&grant),
        armed: true,
    };
    *armed = Some(grant);
    Ok(Json(response))
}

async fn current_grant(State(state): State<Shared>) -> ApiResult<Option<Grant>> {
    Ok(Json(common_core::sync::lock(&state.armed).clone()))
}

async fn portfolio(State(state): State<Shared>) -> Json<PortfolioView> {
    Json(common_core::sync::lock(&state.portfolio).clone())
}

fn decimal_to_chart(value: rust_decimal::Decimal) -> Option<f64> {
    protopyne_chart_model::to_display_float(value)
}

async fn chart_bars(State(state): State<Shared>) -> Json<ChartBars> {
    let mut rows = Vec::with_capacity(state.history.len());
    let mut dropped = 0u64;
    for bar in &state.history {
        match candle_row_of(bar) {
            Some(row) => rows.push(row),
            None => dropped += 1,
        }
    }
    Json(ChartBars { rows, dropped })
}

/// One chartable bar, or `None` when a leg has no finite chart float.
/// Conversion runs through the shared headless helper so the HTTP edge and
/// every future native view share one skip policy; the row shape stays the
/// public chart-data contract.
fn candle_row_of(bar: &Bar) -> Option<CandleRow> {
    let row = protopyne_chart_model::row_of(bar)?;
    Some(CandleRow {
        time: row.time,
        open: row.open,
        high: row.high,
        low: row.low,
        close: row.close,
        volume: row.volume,
    })
}

/// Overlay query: `kind` is `sma`, `ema`, `bb` (mid), or `trima`; `period`
/// is the averaging length; `mult` is the band multiplier for `bb` only
/// (default 2.0, matching the calibration parameterization).
#[derive(Debug, serde::Deserialize)]
struct OverlayQuery {
    kind: String,
    period: i64,
    mult: Option<f64>,
}

/// Scatter compacted kernel values back to bar indices: every bar keeps its
/// index with `None` where its close had no chart float, so overlay vectors
/// always agree positionally with the bar rows. `positions[slot]` is the bar
/// index of compacted slot `slot`; the dropped count is the caller's
/// `history_len - positions.len()`.
fn align_overlay(
    history_len: usize,
    positions: &[usize],
    computed: &[Option<f64>],
) -> Vec<Option<f64>> {
    let mut values: Vec<Option<f64>> = vec![None; history_len];
    for (slot, bar_index) in positions.iter().enumerate() {
        values[*bar_index] = computed.get(slot).copied().flatten();
    }
    values
}

async fn chart_overlay(
    State(state): State<Shared>,
    Query(query): Query<OverlayQuery>,
) -> Result<Json<OverlaySeries>, (StatusCode, Json<ErrorBody>)> {
    if query.period < 1 || query.period > 500 {
        return Err(api_error(StatusCode::BAD_REQUEST, "period must be within [1, 500]"));
    }
    // Per-bar closes through the shared display-float policy: a bar whose
    // close has no chart float keeps its index with a `None` value instead
    // of shifting every later bar (the same count-don't-shift policy as the
    // bar rows). The kernel runs over the representable closes and values
    // scatter back to their bar indices.
    let mut closes: Vec<f64> = Vec::with_capacity(state.history.len());
    let mut positions: Vec<usize> = Vec::with_capacity(state.history.len());
    for (index, bar) in state.history.iter().enumerate() {
        if let Some(close) = decimal_to_chart(bar.c) {
            positions.push(index);
            closes.push(close);
        }
    }
    if closes.is_empty() {
        return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, "no chartable closes"));
    }
    let frame = Ohlcv::close_only(&closes);
    let period = ParamSet::new().with_int("length", query.period);
    let computed = match query.kind.as_str() {
        "sma" => SmaIndicator::new()
            .batch(&frame, &period)
            .map(|result| result.values),
        "ema" => EmaIndicator::new()
            .batch(&frame, &period)
            .map(|result| result.values),
        // Chartable overlaps only: `bb` serves its mid (the primary leg),
        // `trima` its single output. Momentum, volume, and statistic kernels
        // are not price-chartable and stay refused below.
        "bb" => BbIndicator::new()
            .batch(&frame, &period.clone().with_float("mult", query.mult.unwrap_or(2.0)))
            .map(|result| result.values),
        "trima" => TrimaIndicator::new()
            .batch(&frame, &period)
            .map(|result| result.values),
        _ => {
            return Err(api_error(
                StatusCode::BAD_REQUEST,
                "kind must be one of 'sma', 'ema', 'bb', 'trima'",
            ));
        }
    }
    .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    let values = align_overlay(state.history.len(), &positions, &computed);
    let dropped = (state.history.len() - closes.len()) as u64;
    // `values` is full-length by construction; the debug assert pins the
    // contract the native terminal relies on (rows and overlays agree).
    debug_assert_eq!(values.len(), state.history.len());
    Ok(Json(OverlaySeries { kind: query.kind, period: query.period, values, dropped }))
}

async fn feed(State(state): State<Shared>, upgrade: WebSocketUpgrade) -> impl IntoResponse {
    let feed = state.feed.subscribe();
    upgrade.on_upgrade(move |mut socket| async move {
        let mut feed = feed;
        loop {
            match feed.recv().await {
                Ok(frame) => {
                    let message = axum::extract::ws::Message::Text(frame.into());
                    if socket.send(message).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Bar frame published to feed subscribers. `cycle` counts completed passes
/// over history so a resuming subscriber can tell a repeat from a stall.
fn bar_frame(cycle: u64, index: usize, bar: &Bar) -> Option<String> {
    Some(serde_json::json!({
        "type": "bar",
        "cycle": cycle,
        "index": index,
        "time": bar.ts.timestamp(),
        "open": decimal_to_chart(bar.o)?,
        "high": decimal_to_chart(bar.h)?,
        "low": decimal_to_chart(bar.l)?,
        "close": decimal_to_chart(bar.c)?,
        "volume": bar.volume,
    })
    .to_string())
}

/// Build a boot-gate unit: `check` runs inside `execute` so a failing gate
/// cancels its dependents through the batch graph.
fn gate_unit(name: &str, check: impl Fn() -> Result<String, String> + Send + Sync + 'static) -> Arc<dyn Component> {
    let adapter = ComponentAdapter::new(Arc::new(RiskLimits::new().build()))
        .with_name_override(name.to_owned())
        .with_execute_override(Arc::new(move |_ctx: &WorkContext| match check() {
            Ok(message) => Ok(WorkOutput::ok(message)),
            Err(reason) => Err(WorkError::Execution(reason)),
        }));
    Arc::new(adapter)
}

/// Owned server: listener task plus the feed supervision scope.
pub struct ServerHandle {
    /// Bound address.
    pub addr: SocketAddr,
    serve: tokio::task::JoinHandle<()>,
    scope: Scope,
}

impl ServerHandle {
    /// Stop serving: close the feed scope, then stop the listener task.
    pub async fn shutdown(self) {
        let mut scope = self.scope;
        scope.close().await;
        self.serve.abort();
    }
}

/// Boot the HTTP surface over `history`.
///
/// Boot gates (config validity, non-empty history) run under
/// containment-only supervision; any failure refuses to bind. The rehearsal
/// feed pump runs as a server-owned task under the returned handle's scope.
pub async fn serve(config: ServerConfig, history: Vec<Bar>) -> Result<ServerHandle, ServerError> {
    config
        .validate()
        .map_err(ServerError::Service)?;
    if history.is_empty() {
        return Err(ServerError::Service("history must not be empty".to_owned()));
    }
    let history_len = history.len();
    let boot_message = format!("boot gates: {history_len} bars");
    let mut batch = SupervisedBatch::new(
        fluent_concurrency::tokio_runtime(),
        fluent_wvr::CapabilitySet::new(),
    );
    let boot_config = config.clone();
    batch
        .register(gate_unit("config_gate", move || {
            boot_config.validate().map(|()| "config valid".to_owned())
        }))
        .map_err(|e| ServerError::Service(format!("boot gate rejected: {e:?}")))?;
    batch
        .register(gate_unit("history_gate", move || {
            if history_len == 0 {
                Err("history must not be empty".to_owned())
            } else {
                Ok(boot_message.clone())
            }
        }))
        .map_err(|e| ServerError::Service(format!("boot gate rejected: {e:?}")))?;
    let summary = batch.await;
    if !(summary.failed.is_empty() && summary.panicked.is_empty() && summary.cancelled.is_empty()) {
        return Err(ServerError::Service("boot gates refused to bind".to_owned()));
    }
    // Bounded fan-out: the broadcast ring holds at most `feed_capacity`
    // frames (validated 16..=100_000); lagged receivers skip by policy and
    // the pump's send never blocks, so a slow subscriber cannot grow memory.
    let (feed_tx, _) = broadcast::channel(config.feed_capacity as usize);
    let state = Arc::new(AppState {
        // Endpoint concurrency cap: at most `max_concurrent_requests`
        // previews execute at once; excess waits on the semaphore, never
        // spawns.
        limiter: Limiter::new(config.max_concurrent_requests as usize),
        history,
        inbox: Mutex::new(Vec::new()),
        armed: Mutex::new(None),
        portfolio: Mutex::new(PortfolioView::default()),
        feed: feed_tx.clone(),
        trading_state: StateHandle::new(TradingState::Enabled),
        last_command: Mutex::new(None),
    });
    let mut scope = Scope::new();
    {
        let pump_bars = Arc::clone(&state).history.clone();
        let interval = std::time::Duration::from_millis(config.feed_interval_ms);
        scope.spawn(async move {
            let mut index = 0usize;
            loop {
                if let Some(bar) = pump_bars.get(index % pump_bars.len()) {
                    if let Some(frame) = bar_frame((index / pump_bars.len()) as u64, index, bar) {
                        let _ = feed_tx.send(frame);
                    }
                }
                index = index.wrapping_add(1);
                tokio::time::sleep(interval).await;
            }
        });
    }
    let app = Router::new()
        .route("/health", get(health))
        .route("/proposals", get(proposals))
        .route("/proposals/preview", post(preview))
        .route("/grants/arm", post(arm))
        .route("/grants/current", get(current_grant))
        .route("/portfolio", get(portfolio))
        .route("/chart/bars", get(chart_bars))
        .route("/chart/overlays", get(chart_overlay))
        .route("/feed", get(feed))
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .map_err(|e| ServerError::Service(format!("bind failed: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| ServerError::Service(format!("bound address unknown: {e}")))?;
    // The one sanctioned raw spawn: the listener root task, owned by the
    // returned handle and aborted on `shutdown` (every other task in this
    // crate is `Scope`- or pool-owned).
    let serve = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(ServerHandle { addr, serve, scope })
}

#[cfg(test)]
#[path = "../tests/api.rs"]
mod tests;
