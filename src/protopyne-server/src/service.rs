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

use std::collections::BTreeMap;
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
    BreakerLimits, Direction, FixedSignal, Grant, GrantBudgets, GrantPatch, GrantPermission,
    GrantScope, PortfolioThresholds, ProposalState, RiskEngine, RiskLimits, SignalThresholds,
    Strategy, StructureKind, TradeThresholds,
};
use protopyne_chart_model::{ChartRow, row_of};
use protopyne_types::{Bar, Money};
use protopyne_venue::{Position, StateHandle, TradingState};
use protopyne_view::{
    ArmResponse, ChartBar, ChartBars, CommandStatus, DenyCount, HealthView, OverlaySeries,
    PortfolioView, PositionView, ProposalView, ReplayStepView, ReplayView, WorstCase,
};

use crate::error::ServerError;
use crate::runner::{BacktestRun, RunnerConfig, build_replay, complete_replay, run_backtest};
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

/// Display-enum bridges: the one home mapping domain enums to their stable
/// wire tags. Exhaustive matches, so a new variant breaks the build until
/// its tag is decided — tags can never silently drift. The cast is one-way:
/// nothing parses tags back into domain enums.
fn direction_tag(direction: Direction) -> &'static str {
    match direction {
        Direction::Long => "Long",
        Direction::Short => "Short",
    }
}

/// See [`direction_tag`]: option-structure tags.
fn structure_tag(structure: StructureKind) -> &'static str {
    match structure {
        StructureKind::Single => "Single",
        StructureKind::Vertical => "Vertical",
        StructureKind::Calendar => "Calendar",
        StructureKind::Condor => "Condor",
        StructureKind::NakedShort => "NakedShort",
    }
}

/// See [`direction_tag`]: proposal-lifecycle tags.
fn proposal_state_tag(state: ProposalState) -> &'static str {
    match state {
        ProposalState::Proposed => "Proposed",
        ProposalState::RiskChecked => "RiskChecked",
        ProposalState::AutoApproved => "AutoApproved",
        ProposalState::PendingHuman => "PendingHuman",
        ProposalState::Approved => "Approved",
        ProposalState::Rejected => "Rejected",
        ProposalState::Expired => "Expired",
        ProposalState::Submitted => "Submitted",
        ProposalState::Working => "Working",
        ProposalState::Filled => "Filled",
        ProposalState::Partial => "Partial",
        ProposalState::Cancelled => "Cancelled",
    }
}

/// See [`direction_tag`]: trading-state tags.
fn trading_state_tag(state: TradingState) -> &'static str {
    match state {
        TradingState::Enabled => "Enabled",
        TradingState::Disabled => "Disabled",
        TradingState::ExitOnly => "ExitOnly",
    }
}

/// Map one position to its view: strings restored at the boundary so the
/// wire shape is unchanged.
fn position_view(position: &Position) -> PositionView {
    PositionView {
        underlying: position.underlying.clone(),
        quantity: position.quantity,
        avg_price: position.avg_price,
    }
}

/// Map one chart row to its view bar: field-identical, position for
/// position.
fn chart_bar_of(row: &ChartRow) -> ChartBar {
    ChartBar {
        time: row.time,
        open: row.open,
        high: row.high,
        low: row.low,
        close: row.close,
        volume: row.volume,
    }
}

/// Preview request parameters.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PreviewParams {
    /// Seed for deterministic proposal identifiers.
    #[serde(default)]
    pub seed: u64,
}

/// Preview outcome: the run plus why bars did not trade.
///
/// The histogram reports outcome counts (a property of the run), never a
/// confidence score: nothing here measures a component's self-doubt, and
/// nothing fires on these counts. Authorization behavior is unchanged — the
/// same grants produce the same verdicts, now with visible reasons.
///
/// The response type stays server-local on purpose: the typed run tree
/// (manifest, per-bar verdicts, report) is core-owned, and a neutral copy
/// would be a second implementation. The histogram legs ride the neutral
/// [`DenyCount`] DTO; the run serializes exactly as before, which the
/// golden proves byte-for-byte.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PreviewResponse {
    /// The backtest run, unchanged in shape.
    pub run: BacktestRun,
    /// Deny reasons across the run, most frequent first.
    #[serde(default)]
    pub deny_histogram: Vec<DenyCount>,
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
    pub(crate) history: Vec<Bar>,
    pub(crate) inbox: Mutex<Vec<ProposalView>>,
    armed: Mutex<Option<Grant>>,
    pub(crate) portfolio: Mutex<PortfolioView>,
    feed: broadcast::Sender<String>,
    /// Command-plane authority the bars gate on: shared ownership so an
    /// operator plane driving the same handle halts and resumes this
    /// surface. Serving-path reads only — no route mutates it.
    trading_state: StateHandle,
    /// Read-only echo of the last operator command, if any.
    last_command: Mutex<Option<CommandStatus>>,
}

pub(crate) type Shared = Arc<AppState>;
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
/// fields, no new route, no new auth. The DTO's alphabetical field order
/// reproduces the historical sorted-key bytes exactly (golden-pinned).
pub(crate) fn health_view(state: &Shared) -> HealthView {
    let trading = state.trading_state.get();
    HealthView {
        allows_entries: trading.allows(false),
        allows_exits: trading.allows(true),
        last_command: common_core::sync::lock(&state.last_command).clone(),
        status: "ok".to_owned(),
        trading_state: trading_state_tag(trading).to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

async fn health(State(state): State<Shared>) -> Json<HealthView> {
    Json(health_view(&state))
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
                direction: direction_tag(direction).to_owned(),
                structure: structure_tag(StructureKind::Vertical).to_owned(),
                limit_price: decision.limit_price,
                conviction: 0.8,
                state: proposal_state_tag(if decision.submitted {
                    ProposalState::Filled
                } else {
                    ProposalState::Proposed
                })
                .to_owned(),
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
    let trading_state = trading_state_tag(state.trading_state.get()).to_owned();
    let mut portfolio = common_core::sync::lock(&state.portfolio);
    let mut positions: Vec<PositionView> = positions
        .into_iter()
        .map(|(key, (quantity, price_sum, count))| {
            let avg = if count == 0 {
                Money::from(0)
            } else {
                Money::new(price_sum / rust_decimal::Decimal::from(count))
            };
            let underlying = symbols.resolve(key).unwrap_or("SPY").to_owned();
            position_view(
                &Position::new().underlying(underlying).quantity(quantity).avg_price(avg).build(),
            )
        })
        .collect();
    positions.sort_by(|left, right| left.underlying.cmp(&right.underlying));
    portfolio.positions = positions;
    portfolio.filled_orders = filled_orders;
    portfolio.total_fees = Money::new(fees);
    portfolio.open_contracts = portfolio.positions.iter().map(|position| position.quantity).sum();
    portfolio.trading_state = trading_state;
}

/// The live armed grant, if any. The gateway's patch path reads through
/// this (never the field) so the lock discipline stays in this module.
pub(crate) fn live_grant(state: &Shared) -> Option<Grant> {
    common_core::sync::lock(&state.armed).clone()
}

/// Shared arm logic for the route and the gateway: validate, enforce
/// tighten-only against the live grant, then store. The `Err` status rides
/// along so both callers map the same refusal the same way.
pub(crate) fn apply_arm(
    state: &Shared,
    grant: Grant,
) -> Result<ArmResponse, (StatusCode, String)> {
    validate_grant(&grant).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let mut armed = common_core::sync::lock(&state.armed);
    if let Some(current) = armed.clone() {
        if !is_tightening(&grant, &current) {
            return Err((
                StatusCode::CONFLICT,
                "arming must tighten or revoke: loosening needs a new grant after revocation"
                    .to_owned(),
            ));
        }
    }
    let response = ArmResponse {
        grant_id: grant.id.clone(),
        worst_case: worst_case_of(&grant),
        armed: true,
    };
    *armed = Some(grant);
    Ok(response)
}

async fn arm(
    State(state): State<Shared>,
    Json(grant): Json<Grant>,
) -> Result<Json<ArmResponse>, (StatusCode, Json<ErrorBody>)> {
    apply_arm(&state, grant)
        .map(Json)
        .map_err(|(status, reason)| api_error(status, reason))
}

/// Parse a patch envelope's tier texts into a [`GrantPatch`]: each tier
/// rides as a boundary string and parses through its own `FromStr` codec
/// (via `FieldAccess::set_field`), so the whole-patch `from_value` re-parse
/// is gone and there is exactly one parser per tier. Anything else —
/// a non-object patch, a non-string tier, an unknown tier, an unparsable
/// document — refuses naming the tier, before the core's tighten rule.
pub(crate) fn parse_patch(document: &serde_json::Value) -> Result<GrantPatch, (StatusCode, String)> {
    let patch = document.get("patch").ok_or_else(|| {
        (StatusCode::BAD_REQUEST, "grant patch envelope names no patch".to_owned())
    })?;
    let tiers = patch.as_object().ok_or_else(|| {
        (StatusCode::BAD_REQUEST, "grant patch must be an object of tier texts".to_owned())
    })?;
    let mut assembled = GrantPatch::default();
    for (tier, text) in tiers {
        let text = text.as_str().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!("grant patch tier {tier} must be document text, not JSON"),
            )
        })?;
        assembled.set_field(tier, text).map_err(|error| {
            (StatusCode::BAD_REQUEST, format!("grant patch tier {tier} refuses: {error}"))
        })?;
    }
    Ok(assembled)
}

/// Preview a patch envelope's worst case without storing anything: parse
/// the tiers, read the live grant, tighten in memory, and return the worst
/// case the patched grant would authorize. The confirmation surface renders
/// this before submit is actionable; a preview never arms, and the live
/// grant is untouched whether the patch tightens or refuses.
pub(crate) fn preview_arm_of(
    state: &Shared,
    document: &serde_json::Value,
) -> Result<WorstCase, (StatusCode, String)> {
    let patch = parse_patch(document)?;
    let current = live_grant(state).ok_or_else(|| {
        (StatusCode::CONFLICT, "no live grant to tighten".to_owned())
    })?;
    let next = current.tighten(patch).map_err(|error| {
        (StatusCode::CONFLICT, error.to_string())
    })?;
    Ok(worst_case_of(&next))
}

/// Current tier texts of the live grant, for display beside the form's
/// editors: tier name to boundary-string document, composite tiers as JSON
/// document text and the window end as bare RFC 3339 — the same encoding a
/// patch carries back. Read from the grant's own serialized shape through
/// the patch's field list, so no tier is named twice and a new tier appears
/// with no edit here. Empty when no grant is live: placeholders stay blank
/// and the preview refuses, rather than a missing grant looking armed.
pub(crate) fn grant_tiers_of(
    state: &Shared,
) -> Result<BTreeMap<String, String>, (StatusCode, String)> {
    let Some(grant) = live_grant(state) else {
        return Ok(BTreeMap::new());
    };
    let document = serde_json::to_value(&grant)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let mut tiers = BTreeMap::new();
    for tier in GrantPatch::default().field_names() {
        let Some(value) = document.get(*tier) else {
            continue;
        };
        let text = match value.as_str() {
            Some(bare) => bare.to_owned(),
            None => serde_json::to_string(value)
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?,
        };
        tiers.insert((*tier).to_owned(), text);
    }
    Ok(tiers)
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

/// Chart bars over history: converted rows mapped to view bars, with the
/// shared skip policy counting drops. Shared by the route and the gateway.
pub(crate) fn chart_bars_of(history: &[Bar]) -> ChartBars {
    let mut rows = Vec::with_capacity(history.len());
    let mut dropped = 0u64;
    for bar in history {
        match row_of(bar) {
            Some(row) => rows.push(chart_bar_of(&row)),
            None => dropped += 1,
        }
    }
    ChartBars { rows, dropped }
}

async fn chart_bars(State(state): State<Shared>) -> Json<ChartBars> {
    Json(chart_bars_of(&state.history))
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

/// Overlay series over history, shared by the route and the gateway.
/// Refuses unknown kinds, out-of-range periods, and unchartable closes.
/// Per-bar closes run through the shared display-float policy: a bar whose
/// close has no chart float keeps its index with a `None` value instead of
/// shifting every later bar. The `Err` status rides along so both callers
/// map the same refusal the same way.
pub(crate) fn overlay_series_of(
    history: &[Bar],
    kind: &str,
    period: i64,
    mult: f64,
) -> Result<OverlaySeries, (StatusCode, String)> {
    if !(1..=500).contains(&period) {
        return Err((StatusCode::BAD_REQUEST, "period must be within [1, 500]".to_owned()));
    }
    let mut closes: Vec<f64> = Vec::with_capacity(history.len());
    let mut positions: Vec<usize> = Vec::with_capacity(history.len());
    for (index, bar) in history.iter().enumerate() {
        if let Some(close) = decimal_to_chart(bar.c) {
            positions.push(index);
            closes.push(close);
        }
    }
    if closes.is_empty() {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "no chartable closes".to_owned()));
    }
    let frame = Ohlcv::close_only(&closes);
    let params = ParamSet::new().with_int("length", period);
    let computed = match kind {
        "sma" => SmaIndicator::new().batch(&frame, &params).map(|result| result.values),
        "ema" => EmaIndicator::new().batch(&frame, &params).map(|result| result.values),
        // Chartable overlaps only: `bb` serves its mid (the primary leg),
        // `trima` its single output. Momentum, volume, and statistic kernels
        // are not price-chartable and stay refused below.
        "bb" => BbIndicator::new()
            .batch(&frame, &params.clone().with_float("mult", mult))
            .map(|result| result.values),
        "trima" => TrimaIndicator::new().batch(&frame, &params).map(|result| result.values),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "kind must be one of 'sma', 'ema', 'bb', 'trima'".to_owned(),
            ));
        }
    }
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let values = align_overlay(history.len(), &positions, &computed);
    let dropped = (history.len() - closes.len()) as u64;
    // `values` is full-length by construction; the debug assert pins the
    // contract the native terminal relies on (rows and overlays agree).
    debug_assert_eq!(values.len(), history.len());
    Ok(OverlaySeries { kind: kind.to_owned(), period, values, dropped })
}

async fn chart_overlay(
    State(state): State<Shared>,
    Query(query): Query<OverlayQuery>,
) -> Result<Json<OverlaySeries>, (StatusCode, Json<ErrorBody>)> {
    overlay_series_of(&state.history, &query.kind, query.period, query.mult.unwrap_or(2.0))
        .map(Json)
        .map_err(|(status, reason)| api_error(status, reason))
}

/// Bars between replay checkpoints when the route is asked for no cadence.
/// One anchor per pane-window of a session's history keeps the published step
/// list short enough to scrub while every bar stays individually reachable.
pub const DEFAULT_REPLAY_CHECKPOINT_EVERY: usize = 50;

/// Replay query: `checkpoint_every` is the bar cadence the core records
/// anchors at. Absent means [`DEFAULT_REPLAY_CHECKPOINT_EVERY`].
#[derive(Debug, serde::Deserialize)]
struct ReplayQuery {
    checkpoint_every: Option<usize>,
}

/// Replay read over history, shared by the route and the gateway.
///
/// Composes the same research replay graph the runner builds
/// ([`build_replay`]) instead of a second step walk, so the published order,
/// the anchors, and the "reached" flags are the graph's own answers lifted
/// field-for-field. The read stands at the head: a fresh window has nothing
/// rewound, and a terminal that scrubs holds the moved cursor itself.
///
/// The `Err` status rides along so both callers map the same refusal the
/// same way.
pub(crate) fn replay_view_of(
    history: &[Bar],
    checkpoint_every: usize,
) -> Result<ReplayView, (StatusCode, String)> {
    let mut graph = build_replay(history, checkpoint_every)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    complete_replay(&mut graph);
    // Own the key list so each step's state and anchor flag are read from
    // the graph rather than re-derived from the bar index.
    let keys: Vec<String> = graph.step_ids().to_vec();
    let mut steps = Vec::with_capacity(keys.len());
    for key in &keys {
        let step = graph.status(key).ok_or_else(|| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("replay step missing: {key:?}"))
        })?;
        steps.push(ReplayStepView {
            bar_index: step.bar_index as u64,
            ts: step.ts.clone(),
            done: step.done,
            checkpoint: graph.is_checkpoint(key),
        });
    }
    // The head is one past the last bar index, so an untouched window reads
    // `cursor == total_bars` — the same shape the terminal cursor validates.
    let total_bars = steps.len() as u64;
    Ok(ReplayView { cursor: total_bars, total_bars, steps })
}

async fn replay(
    State(state): State<Shared>,
    Query(query): Query<ReplayQuery>,
) -> Result<Json<ReplayView>, (StatusCode, Json<ErrorBody>)> {
    replay_view_of(
        &state.history,
        query.checkpoint_every.unwrap_or(DEFAULT_REPLAY_CHECKPOINT_EVERY),
    )
    .map(Json)
    .map_err(|(status, reason)| api_error(status, reason))
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
    state: Shared,
}

impl ServerHandle {
    /// Read gateway over the live state: the same views the routes serve,
    /// without the HTTP hop (the gateway-route parity surface).
    #[must_use]
    pub fn gateway(&self) -> crate::gateway::ServerGateway {
        crate::gateway::ServerGateway::new(Arc::clone(&self.state))
    }

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
        .route("/replay", get(replay))
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
    Ok(ServerHandle { addr, serve, scope, state })
}

#[cfg(test)]
#[path = "../tests/api.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/views_golden.rs"]
mod views_golden_tests;
