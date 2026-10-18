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
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use fluent_concurrency::batch::SupervisedBatch;
use fluent_concurrency::pool::Limiter;
use fluent_concurrency::scope::Scope;
use fluent_wvr::prelude::*;
use tokio::sync::broadcast;

use protopyne_chart_model::rows_of;
use protopyne_kernels::{
    BbIndicator, EmaIndicator, Indicator as _, Ohlcv, ParamSet, SmaIndicator, TrimaIndicator,
};
use protopyne_store::{
    FoldPolicy, MarketTape, PlanError, ReadRequest as TapeReadRequest, StoreError, StoreHealth,
    StoreStorage, TimeRange,
};
use protopyne_strategy::{
    BreakerLimits, Direction, FixedSignal, Grant, GrantBudgets, GrantPatch, GrantPermission,
    GrantScope, PortfolioThresholds, ProposalState, RiskEngine, RiskLimits, SignalThresholds,
    Strategy, StructureKind, TradeThresholds,
};
use protopyne_types::{Bar, Money, Timeframe};
use protopyne_venue::{
    serving_allows_entries, serving_verdict, update_from_fill, HealthVerdict, Position,
    PositionFill, PositionSide, SpreadPosition, StateHandle, TradingState,
};
use protopyne_view::{
    ArmResponse, ChartBars, CommandStatus, DenyCount, HealthView, OverlaySeries, PortfolioView,
    PositionView, ProposalView, ReplayStepView, ReplayView, WorstCase,
};

use crate::backtest::BacktestConfig;
use crate::chain_history::{ChainFeedConfig, HistoricalChainSource};
use crate::error::ServerError;
use crate::gateway::FallbackLatch;
use crate::history_root::HistoryRoot;
use crate::replay_session::ReplayRegistry;
use crate::runner::{
    build_replay, complete_replay, run_backtest, side_for_direction, BacktestRun, BarDecision,
    RunnerConfig,
};

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
    #[field(desc = "Bar history file path (CSV or Parquet)")]
    pub history_path: Option<String>,
    /// Durable tape store root. When set, boot opens the file-backed tape at
    /// this root, asserts the store-permit binding against it, and serves
    /// symbol-scoped chart reads through it (falling back to the boot
    /// history per read). Absent serves the boot history exactly as before.
    /// `#[serde(default)]` keeps older config files (which name no tape)
    /// loading.
    #[field(desc = "Durable tape store root")]
    #[serde(default)]
    pub tape_path: Option<String>,
    /// Option-chain feed config file path (JSON [`ChainFeedConfig`](crate::chain_history::ChainFeedConfig):
    /// chain history file plus the tastytrade credentials path the fetch
    /// path reads). Absent leaves chains unconfigured and every
    /// multi-leg proposal keeps refusing with the missing feed named.
    /// `#[serde(default)]` keeps older config files (which name no chain
    /// feed) loading.
    #[field(desc = "Option-chain feed config file path")]
    #[serde(default)]
    pub chain_feed_path: Option<String>,
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
    /// Bar spacing of the boot history, spelled as a toolbar label (`1M`,
    /// `5M`, `15M`, `1H`, `4H`, `1D`). Symbol- and timeframe-scoped reads
    /// fold the boot history by whole multiples of this base; the `15M`
    /// default is the vendored CLI history, so a deployment that names no
    /// base keeps the exact pre-existing resampling. `#[serde(default)]`
    /// keeps older config files (which name no base) loading.
    #[field(desc = "Boot history bar spacing (1M, 5M, 15M, 1H, 4H, 1D)")]
    #[builder(default = default_history_timeframe())]
    #[serde(default = "default_history_timeframe")]
    pub history_timeframe: String,
    /// Latest observed store liveness, seeded into serving state at boot.
    /// The repair worker publishes live readings over this once it exists;
    /// until then the boot-supplied reading (healthy by default) is what
    /// `/health` reports. Stays out of the field descriptor (`#[field(skip)]`):
    /// a `StoreHealth` has no boundary-string reading. `#[serde(default)]`
    /// keeps older config files (which name no store reading) loading.
    #[field(skip)]
    #[builder(default)]
    #[serde(default)]
    pub store_health: StoreHealth,
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
        if timeframe_of_label(&self.history_timeframe).is_none() {
            return Err(format!(
                "history_timeframe '{}' is not a known bar spacing",
                self.history_timeframe
            ));
        }
        if let Some(path) = &self.tape_path {
            if path.trim().is_empty() {
                return Err("tape_path must not be blank".to_owned());
            }
        }
        if let Some(path) = &self.chain_feed_path {
            if path.trim().is_empty() {
                return Err("chain_feed_path must not be blank".to_owned());
            }
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
/// nothing parses tags back into domain enums. Crate-visible so the gateway's
/// interactive replay state reads the same tags as the preview publisher.
pub(crate) fn direction_tag(direction: Direction) -> &'static str {
    match direction {
        Direction::Long => "Long",
        Direction::Short => "Short",
    }
}

/// See [`direction_tag`]: option-structure tags.
pub(crate) fn structure_tag(structure: StructureKind) -> &'static str {
    match structure {
        StructureKind::Single => "Single",
        StructureKind::Vertical => "Vertical",
        StructureKind::Calendar => "Calendar",
        StructureKind::Condor => "Condor",
        StructureKind::NakedShort => "NakedShort",
    }
}

/// See [`direction_tag`]: proposal-lifecycle tags.
pub(crate) fn proposal_state_tag(state: ProposalState) -> &'static str {
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

/// Preview request parameters.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PreviewParams {
    /// Seed for deterministic proposal identifiers.
    #[serde(default)]
    pub seed: u64,
    /// The grant to evaluate under. `None` uses the fixed rehearsal grant; a
    /// caller that wants a live or altered grant to apply supplies it here, so
    /// the manifest is a pure function of the request rather than of an
    /// ambient clock read.
    #[serde(default)]
    pub grant: Option<Grant>,
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
    /// Bar spacing of [`Self::history`], so the gateway folds from the base
    /// the history actually holds — never an assumed one.
    pub(crate) history_timeframe: Timeframe,
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
    /// Latest observed store liveness, seeded from [`ServerConfig`] at boot.
    /// Serving-path reads only; the repair worker publishes live readings
    /// here once it exists.
    store_health: Mutex<StoreHealth>,
    /// The history root the server booted from, when the boot history came
    /// from a directory. Symbol discovery reads it; a file boot has no root
    /// and serves the shipped offline list.
    pub(crate) history_root: Option<HistoryRoot>,
    /// The durable tape the gateway plans its symbol-scoped chart reads
    /// through, when the server was booted with one. `None` serves the boot
    /// history unchanged, so a store-less server behaves exactly as before.
    pub(crate) tape: Option<TapeReader>,
    /// Counters over tape-serve outcomes. Observation only: recording an
    /// outcome never changes what the read serves.
    pub(crate) tape_serve_stats: Arc<TapeServeStats>,
    /// Edge-triggered fallback latch, so a sustained fallback warns once and
    /// a recovery logs once rather than one line per read. Observation only:
    /// the read decides fallback before consulting it.
    pub(crate) fallback_latch: Mutex<FallbackLatch>,
    /// Live interactive replay drivers by session id. A terminal opens,
    /// steps, and closes its slot through the gateway; each driver owns a
    /// session-scoped venue and journal, so a replay never touches the live
    /// paper journal or the inbox/portfolio views a preview publishes.
    pub(crate) replay: Mutex<ReplayRegistry>,
    /// Boot-loaded option-chain history, when the server was booted with a
    /// chain feed config naming a chain data file. The gateway slices the
    /// replay window out of this at open; `None` (or an empty load) keeps
    /// the chain gate exactly where it was.
    pub(crate) chain_history: Option<HistoricalChainSource>,
}

pub(crate) type Shared = Arc<AppState>;
type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorBody>)>;

fn api_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        status,
        Json(ErrorBody {
            error: message.into(),
        }),
    )
}

/// The bundled preview signal: fixed long exposure for rehearsal.
/// Constants sit inside `[0, 1]` by construction; any refusal is a bug the
/// caller sees as a failed preview, never a fallback signal. Crate-visible
/// so the interactive replay opens the same signal (its structure is pinned
/// to singles separately, at the session).
pub(crate) fn preview_strategy() -> Result<Arc<Strategy>, ServerError> {
    let signal = FixedSignal::new(Direction::Long, 0.8, 0.8)
        .map_err(|e| ServerError::Service(format!("bundled signal refused: {e}")))?;
    Ok(Arc::new(Strategy::with_signal(Arc::new(signal))))
}

/// The fixed rehearsal grant used when the caller supplies none. The window is
/// a compile-time constant (no ambient clock), so a preview's pinned config is
/// reproducible; evaluation happens at bar time, never wall-clock time.
fn permissive_grant() -> Grant {
    rehearsal_grant("preview-permissive", vec![StructureKind::Vertical])
}

/// The fixed rehearsal grant an interactive replay runs under when the
/// opener supplies no grant document: the preview shape, scoped to the
/// requested structure. Singles behave exactly as before; a multi-leg scope
/// admits the shape the window's chains cover — uncovered bars are still
/// refused by the session's coverage screen before the grant ever sees them.
pub(crate) fn replay_grant_for(structure: StructureKind) -> Grant {
    rehearsal_grant("replay-permissive", vec![structure])
}

/// One rehearsal-grant body over an explicit structure scope and id: preview
/// and replay differ only in which structures they admit, never in budgets,
/// thresholds, or breakers.
fn rehearsal_grant(id: &str, structures: Vec<StructureKind>) -> Grant {
    Grant::new()
        .id(id.to_owned())
        .scope(
            GrantScope::new()
                .strategies(vec!["ribbon".to_owned()])
                .underlyings(vec!["SPY".to_owned()])
                .structures(structures)
                .directions(vec![Direction::Long, Direction::Short])
                .permissions(GrantPermission {
                    entries: true,
                    exits: true,
                    rolls: false,
                })
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
        .valid_from(rehearsal_stamp(1_700_000_000))
        .valid_until(rehearsal_stamp(1_900_000_000))
        .build()
}

/// A fixed rehearsal-window stamp. The constant is a valid instant, so the
/// `expect` states a construction invariant, never reads a clock.
fn rehearsal_stamp(seconds: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(seconds, 0).expect("fixed rehearsal stamp is valid")
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

/// Map the serving state's store reading onto the view's optional fields.
/// An unobserved reading reports nothing, so a server that never observed
/// the store serves the pre-store payload byte-identically; any observed
/// reading reports all three facts together — even one that happens to
/// equal the defaults — because a half-reported reading would be worse
/// than none, and a genuine reading must never read as no reading.
fn store_fields(reading: StoreHealth) -> (Option<bool>, Option<u64>, Option<u64>) {
    if reading.observed {
        (
            Some(reading.reachable),
            Some(reading.open_gaps),
            reading.watermark_age_secs,
        )
    } else {
        (None, None, None)
    }
}

/// The store a serving gateway plans its chart reads through, threadbare by
/// design: the durable tape plus the capability set and permit its effects
/// require and the tape's registered venue spelling. Holding the permit
/// here is what lets the synchronous gateway assert the root binding on
/// every read — through the tape's own scoped read, against the store's own
/// root — without inventing a second capability mechanism. A reader whose
/// permit names another root is denied per read, never served.
pub struct TapeReader {
    /// The durable tape this reader plans and reads.
    pub store: std::sync::Arc<MarketTape>,
    /// The capability set the store's effects are gated on.
    pub caps: fluent_wvr::CapabilitySet,
    /// The permit the per-read binding check runs: bound to this tape's
    /// root at boot, refused on its face when it names another root.
    pub permit: StoreStorage,
    /// The venue spelling the tape prints under.
    pub venue: String,
    /// Trailing-bar cap for an unbounded chart read.
    pub limit: usize,
}

/// Default trailing-bar cap for a store-backed chart read: enough to fill a
/// chart window without an unbounded scan. A **cost bound**, never a
/// correctness threshold.
pub const DEFAULT_TAPE_LIMIT: usize = 5_000;

/// Default bar spacing of the boot history: the vendored CLI history's
/// spacing, so a server that names no base resamples exactly as before.
fn default_history_timeframe() -> String {
    "15M".to_owned()
}

/// Map the terminal toolbar's resolution label onto a [`Timeframe`].
///
/// The one toolbar-label bridge lives on [`Timeframe::from_chart_label`];
/// this delegates to it so serving and display cannot disagree about what a
/// label means.
#[must_use]
pub fn timeframe_of_label(label: &str) -> Option<Timeframe> {
    Timeframe::from_chart_label(label)
}

/// Why a store-backed chart read could not produce bars. A copyable kind
/// ([`TapeMissKind`]) rides alongside so counters and the fallback latch hold
/// the classification without the non-`Copy` store error.
#[derive(Debug)]
pub enum TapeMiss {
    /// No durable tape is wired into this server.
    NoTape,
    /// The requested label names no bar spacing this server knows.
    UnsupportedTimeframe,
    /// The tape holds no market under the requested symbol.
    UnknownSymbol,
    /// The calling task was never granted the store.
    Denied,
    /// The store failed for another reason, carried for diagnosis.
    Store(StoreError),
}

/// The copyable classification of a [`TapeMiss`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TapeMissKind {
    /// See [`TapeMiss::NoTape`].
    NoTape,
    /// See [`TapeMiss::UnsupportedTimeframe`].
    UnsupportedTimeframe,
    /// See [`TapeMiss::UnknownSymbol`].
    UnknownSymbol,
    /// See [`TapeMiss::Denied`].
    Denied,
    /// See [`TapeMiss::Store`].
    Store,
}

impl TapeMiss {
    /// The copyable classification of this miss.
    #[must_use]
    pub fn kind(&self) -> TapeMissKind {
        match self {
            Self::NoTape => TapeMissKind::NoTape,
            Self::UnsupportedTimeframe => TapeMissKind::UnsupportedTimeframe,
            Self::UnknownSymbol => TapeMissKind::UnknownSymbol,
            Self::Denied => TapeMissKind::Denied,
            Self::Store(_) => TapeMissKind::Store,
        }
    }
}

/// The outcome of planning and reading one chart window from the durable
/// tape: either the served bars, or why the tape could not serve them.
/// Collapsing the miss into a bare `None` erases the difference between "no
/// tape" and "the tape refused", which is why provenance is observable.
#[derive(Debug)]
pub enum TapeServe {
    /// The tape served these bars.
    Served(Vec<Bar>),
    /// The tape could not serve the read, with the reason.
    Missing(TapeMiss),
}

/// Counters over tape-serve outcomes: how many reads the tape served, and a
/// per-kind tally of misses with the most recent kind. Pure observation —
/// nothing gates a read on these; the read path records its outcome after
/// deciding it.
#[derive(Debug, Default)]
pub struct TapeServeStats {
    served: AtomicU64,
    no_tape: AtomicU64,
    unsupported_timeframe: AtomicU64,
    unknown_symbol: AtomicU64,
    denied: AtomicU64,
    store_error: AtomicU64,
    last_miss: Mutex<Option<TapeMissKind>>,
}

/// A point-in-time snapshot of [`TapeServeStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TapeServeSnapshot {
    /// Reads the tape served.
    pub served: u64,
    /// Reads with no tape configured.
    pub no_tape: u64,
    /// Reads whose label named an unknown bar spacing.
    pub unsupported_timeframe: u64,
    /// Reads whose symbol the tape does not hold.
    pub unknown_symbol: u64,
    /// Reads the store permit denied.
    pub denied: u64,
    /// Reads that failed for another store reason.
    pub store_error: u64,
    /// The most recent miss kind, if any.
    pub last_miss: Option<TapeMissKind>,
}

impl TapeServeStats {
    /// Record one serve outcome.
    pub(crate) fn record(&self, serve: &TapeServe) {
        match serve {
            TapeServe::Served(_) => {
                self.served.fetch_add(1, Ordering::Relaxed);
            }
            TapeServe::Missing(miss) => {
                match miss.kind() {
                    TapeMissKind::NoTape => &self.no_tape,
                    TapeMissKind::UnsupportedTimeframe => &self.unsupported_timeframe,
                    TapeMissKind::UnknownSymbol => &self.unknown_symbol,
                    TapeMissKind::Denied => &self.denied,
                    TapeMissKind::Store => &self.store_error,
                }
                .fetch_add(1, Ordering::Relaxed);
                *common_core::sync::lock(&self.last_miss) = Some(miss.kind());
            }
        }
    }

    /// A snapshot for diagnostics: the counters are read independently (each
    /// `Relaxed`), which is right for observability — nothing gates behavior
    /// on their exact sums.
    #[must_use]
    pub fn snapshot(&self) -> TapeServeSnapshot {
        TapeServeSnapshot {
            served: self.served.load(Ordering::Relaxed),
            no_tape: self.no_tape.load(Ordering::Relaxed),
            unsupported_timeframe: self.unsupported_timeframe.load(Ordering::Relaxed),
            unknown_symbol: self.unknown_symbol.load(Ordering::Relaxed),
            denied: self.denied.load(Ordering::Relaxed),
            store_error: self.store_error.load(Ordering::Relaxed),
            last_miss: *common_core::sync::lock(&self.last_miss),
        }
    }
}

/// Plan and read a chart window from the durable tape, reporting either the
/// served bars or why the read could not be answered. The store's own gate
/// runs inside the permit's scope on the shared capability-scoped blocking
/// worker (multi-thread runtime → `block_in_place`; current-thread runtime or
/// no runtime → a dedicated scoped OS thread), so a synchronous render loop
/// can plan a read without stalling the executor and a task that was never
/// granted the store is denied exactly as everywhere else. The caller decides
/// the fallback; this layer never silently substitutes another source.
#[must_use]
pub fn tape_serve(
    reader: &TapeReader,
    symbol: &str,
    timeframe: &str,
    limit: Option<usize>,
) -> TapeServe {
    let Some(tf) = timeframe_of_label(timeframe) else {
        return TapeServe::Missing(TapeMiss::UnsupportedTimeframe);
    };
    let request = TapeReadRequest {
        symbol: symbol.to_owned(),
        venue: reader.venue.clone(),
        timeframe: tf,
        range: TimeRange {
            start_ms: None,
            end_ms: None,
        },
        limit: Some(limit.unwrap_or(reader.limit)),
        policy: FoldPolicy::ExtendToFullBuckets,
    };
    let store = Arc::clone(&reader.store);
    let caps = reader.caps.clone();
    let permit = reader.permit.clone();
    // The scoped read asserts the root binding first — a permit minted for
    // another store refuses on its face — then presence, then the read.
    match fluent_wvr::run_gated_blocking(caps, move || store.read_scoped(&permit, &request)) {
        Ok(Ok(series)) => TapeServe::Served(series.bars),
        Ok(Err(error)) => TapeServe::Missing(tape_miss_of(error)),
        Err(join) => TapeServe::Missing(TapeMiss::Store(StoreError::InvalidBar(format!(
            "tape read worker panicked: {join}"
        )))),
    }
}

/// Classify a store failure into the tape-serve miss taxonomy. An unknown
/// symbol and a denied permit are the two refusals the caller can act on;
/// everything else is an unclassified store failure, carried verbatim.
fn tape_miss_of(error: StoreError) -> TapeMiss {
    match error {
        StoreError::Plan(PlanError::UnknownSymbol(_)) => TapeMiss::UnknownSymbol,
        StoreError::Denied(_) => TapeMiss::Denied,
        other => TapeMiss::Store(other),
    }
}

/// Map one tape-miss kind onto its stable wire tag. See [`direction_tag`]:
/// the cast is one-way, and a new kind breaks the build until its tag is
/// decided — tags can never silently drift.
fn tape_miss_tag(kind: TapeMissKind) -> &'static str {
    match kind {
        TapeMissKind::NoTape => "NoTape",
        TapeMissKind::UnsupportedTimeframe => "UnsupportedTimeframe",
        TapeMissKind::UnknownSymbol => "UnknownSymbol",
        TapeMissKind::Denied => "Denied",
        TapeMissKind::Store => "Store",
    }
}

/// Map the tape-serve counters onto the view's optional fields. An
/// unobserved snapshot reports nothing, so a server that never read through
/// its tape serves the pre-tape payload byte-identically; any observed
/// snapshot reports every counter together — even ones that happen to be
/// zero — because a half-reported tally would be worse than none. The last
/// miss rides only when a miss was observed: a serve-only window names no
/// fault. Field order follows the view's alphabetical field order.
struct TapeFieldSet {
    denied: Option<u64>,
    last_miss: Option<String>,
    no_tape: Option<u64>,
    served: Option<u64>,
    store_error: Option<u64>,
    unknown_symbol: Option<u64>,
    unsupported_timeframe: Option<u64>,
}

fn tape_fields(snapshot: TapeServeSnapshot) -> TapeFieldSet {
    if snapshot == TapeServeSnapshot::default() {
        TapeFieldSet {
            denied: None,
            last_miss: None,
            no_tape: None,
            served: None,
            store_error: None,
            unknown_symbol: None,
            unsupported_timeframe: None,
        }
    } else {
        TapeFieldSet {
            denied: Some(snapshot.denied),
            last_miss: snapshot
                .last_miss
                .map(|kind| tape_miss_tag(kind).to_owned()),
            no_tape: Some(snapshot.no_tape),
            served: Some(snapshot.served),
            store_error: Some(snapshot.store_error),
            unknown_symbol: Some(snapshot.unknown_symbol),
            unsupported_timeframe: Some(snapshot.unsupported_timeframe),
        }
    }
}

/// Open the durable tape at `root` and build the reader the serving gateway
/// plans its symbol-scoped chart reads through.
///
/// The permit binding is asserted once here, never per read: the permit is
/// minted from the opened tape's own root and must equal the requested root,
/// and a probe read for an unregistered symbol must refuse as
/// `UnknownSymbol` — binding and presence both passed — rather than
/// `Denied`. An empty catalog refuses loudly: a boot with no market to
/// serve would otherwise answer every chart as a silent fallback. The reader
/// prints under the tape's first market venue, in id order, so a
/// single-venue deployment names nothing twice.
pub fn open_tape_reader(root: &std::path::Path) -> Result<TapeReader, ServerError> {
    let tape = MarketTape::open(root).map_err(|e| {
        ServerError::Service(format!("tape at {} failed to open: {e}", root.display()))
    })?;
    let permit = StoreStorage::new(tape.root().to_path_buf());
    if permit.store_path() != root {
        return Err(ServerError::Service(format!(
            "tape permit is bound to {}, not {}",
            permit.store_path().display(),
            root.display()
        )));
    }
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let venue = fluent_wvr::CURRENT_CAPS.sync_scope(caps.clone(), || {
        let markets = tape
            .registry()
            .markets()
            .map_err(|e| ServerError::Service(format!("tape registry refused: {e}")))?;
        let venue = markets
            .first()
            .map(|market| market.venue.clone())
            .ok_or_else(|| {
                ServerError::Service(format!(
                    "tape at {} holds no markets: register a market before serving",
                    root.display()
                ))
            })?;
        let probe = TapeReadRequest {
            symbol: "__tape_boot_probe__".to_owned(),
            venue: venue.clone(),
            timeframe: Timeframe::M15,
            range: TimeRange {
                start_ms: None,
                end_ms: None,
            },
            limit: Some(1),
            policy: FoldPolicy::ExtendToFullBuckets,
        };
        match tape.read_scoped(&permit, &probe) {
            Err(StoreError::Plan(PlanError::UnknownSymbol(_))) => Ok(venue),
            Err(other) => Err(ServerError::Service(format!(
                "tape binding probe refused: {other}"
            ))),
            Ok(_) => Err(ServerError::Service(
                "tape binding probe unexpectedly served".to_owned(),
            )),
        }
    })?;
    Ok(TapeReader {
        store: Arc::new(tape),
        caps,
        permit,
        venue,
        limit: DEFAULT_TAPE_LIMIT,
    })
}

/// Liveness plus command-plane status: the trading state (halt/resume) and
/// the last operator command ride the existing health route — additive
/// fields, no new route, no new auth. The DTO's alphabetical field order
/// reproduces the historical sorted-key bytes exactly (golden-pinned). The
/// store facts ride the same builder the gateway reads, so the route and
/// the terminal cannot drift. Entry gating composes the venue's
/// serving-path functions, so the route and the preview refusal read one
/// verdict.
pub(crate) fn health_view(state: &Shared) -> HealthView {
    let trading = state.trading_state.get();
    let reading: StoreHealth = *common_core::sync::lock(&state.store_health);
    let (store_reachable, store_open_gaps, store_watermark_age_secs) = store_fields(reading);
    let tape = tape_fields(state.tape_serve_stats.snapshot());
    HealthView {
        allows_entries: serving_allows_entries(trading, reading),
        allows_exits: trading.allows(true),
        last_command: common_core::sync::lock(&state.last_command).clone(),
        status: "ok".to_owned(),
        store_open_gaps,
        store_reachable,
        store_watermark_age_secs,
        tape_denied: tape.denied,
        tape_last_miss: tape.last_miss,
        tape_no_tape: tape.no_tape,
        tape_served: tape.served,
        tape_store_error: tape.store_error,
        tape_unknown_symbol: tape.unknown_symbol,
        tape_unsupported_timeframe: tape.unsupported_timeframe,
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
    // A store outage halts entries here, naming every failing gate, before
    // any run executes. It is the same pure verdict `/health` reports, so
    // the route a preview reads and the route an operator reads cannot
    // disagree. Exits are unaffected at this layer because a preview never
    // exits a position.
    let reading: StoreHealth = *common_core::sync::lock(&state.store_health);
    if let HealthVerdict::Halt { reasons } = serving_verdict(reading) {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("halted: {}", reasons.join("; ")),
        ));
    }
    // The grant is an explicit input: a supplied one, else the fixed rehearsal
    // grant. Nothing here reads the clock or the armed state, so the pinned
    // config is reproducible from the request alone.
    let PreviewParams { seed, grant } = params;
    let grant = grant.unwrap_or_else(permissive_grant);
    let run = state
        .limiter
        .run(|| async {
            let strategy = preview_strategy()?;
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
                seed,
            )
            .await
        })
        .await
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, e.to_string()))?;
    publish_run(&state, &run);
    let deny_histogram = deny_histogram_of(&run);
    Ok(Json(PreviewResponse {
        run,
        deny_histogram,
    }))
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
        for reason in decision
            .risk_reasons
            .iter()
            .chain(decision.grant_reasons.iter())
        {
            *counts.entry(reason.as_str()).or_insert(0) += 1;
        }
    }
    let mut histogram: Vec<DenyCount> = counts
        .into_iter()
        .map(|(reason, bars)| DenyCount {
            reason: reason.to_owned(),
            bars,
        })
        .collect();
    histogram.sort_by(|left, right| {
        right
            .bars
            .cmp(&left.bars)
            .then_with(|| left.reason.cmp(&right.reason))
    });
    if histogram.len() > DENY_HISTOGRAM_TOP {
        let rest: u64 = histogram[DENY_HISTOGRAM_TOP..]
            .iter()
            .map(|entry| entry.bars)
            .sum();
        histogram.truncate(DENY_HISTOGRAM_TOP);
        histogram.push(DenyCount {
            reason: "other".to_owned(),
            bars: rest,
        });
    }
    histogram
}

/// Record a preview run's verdicts in the inbox and portfolio views.
///
/// The inbox is a projection of each decision's carried lifecycle: the
/// attribution comes from the proposal itself, and the state is the final
/// journaled transition's target. Nothing here is fabricated.
fn publish_run(state: &Shared, run: &BacktestRun) {
    let mut inbox = common_core::sync::lock(&state.inbox);
    inbox.clear();
    for decision in &run.decisions {
        let Some(direction) = decision.direction else {
            continue;
        };
        let state_tag = decision
            .transitions
            .last()
            .map_or(ProposalState::Proposed, |transition| transition.to);
        inbox.push(ProposalView {
            id: decision.proposal_id.clone(),
            strategy: decision.strategy.clone(),
            underlying: decision.underlying.clone(),
            direction: direction_tag(direction).to_owned(),
            structure: decision.structure.map_or_else(
                || "unrecorded".to_owned(),
                |kind| structure_tag(kind).to_owned(),
            ),
            limit_price: decision.limit_price,
            conviction: decision.conviction,
            state: proposal_state_tag(state_tag).to_owned(),
        });
    }
    drop(inbox);
    let trading_state = trading_state_tag(state.trading_state.get()).to_owned();
    let folded = portfolio_of_decisions(&run.decisions, &trading_state);
    let mut portfolio = common_core::sync::lock(&state.portfolio);
    *portfolio = folded;
}

/// Fold booked fills into a portfolio view: the same engine the live book
/// and the portfolio fold use, so same-direction fills re-average by volume
/// weight while opposite-direction fills reduce (never re-average) and full
/// reductions close the book. A submitted order without a priced fill counts
/// toward orders and fees but adds no exposure. Shared by the preview
/// publisher and the interactive replay state — one fold, two readers.
/// `trading_state` is the caller's own tag: the preview reads the live
/// handle, the replay names its mode.
pub(crate) fn portfolio_of_decisions(
    decisions: &[BarDecision],
    trading_state: &str,
) -> PortfolioView {
    // Preview-run position fold, grouped by the underlying each fill was
    // booked under. Each fill folds through the venue's own
    // `update_from_fill` — the same engine the live book and the portfolio
    // fold use — so same-direction fills re-average by volume weight while
    // opposite-direction fills reduce (never re-average) and full reductions
    // close the book. The served number has a single home; the hand-rolled
    // arithmetic-mean fold is gone. A submitted order without a priced fill
    // counts toward orders and fees but adds no exposure.
    let mut books: BTreeMap<String, SpreadPosition> = BTreeMap::new();
    let mut fees = rust_decimal::Decimal::ZERO;
    let mut filled_orders: u64 = 0;
    for decision in decisions {
        if !decision.submitted {
            continue;
        }
        filled_orders += 1;
        fees += decision
            .fee
            .map_or(rust_decimal::Decimal::ZERO, Money::as_decimal);
        let quantity = decision.filled_quantity;
        let (Some(direction), Some(price)) = (decision.direction, decision.fill_price) else {
            continue;
        };
        if quantity == 0 {
            continue;
        }
        let Ok(at) = chrono::DateTime::parse_from_rfc3339(&decision.ts) else {
            continue;
        };
        let fill = PositionFill::new()
            .underlying(decision.underlying.clone())
            .side(side_for_direction(direction))
            .quantity(quantity)
            .price(price)
            .fee(decision.fee.unwrap_or_else(Money::zero))
            .trade_id(protopyne_venue::client_id_for(&decision.proposal_id, 0))
            .at(at.with_timezone(&chrono::Utc))
            .build();
        let current = books.remove(&decision.underlying);
        // A full reduction closes the book; anything else stays open under
        // the venue's own averaging. The refusal arm is unreachable by
        // construction (positive quantity, keyed underlying — the only two
        // refusals): the view publisher stays infallible, dropping an
        // impossible refusal's exposure while its orders and fees above
        // still count it.
        if let Ok((Some(position), _)) = update_from_fill(current, &fill) {
            books.insert(decision.underlying.clone(), position);
        }
    }
    // BTreeMap iteration is key order, so the served list is sorted by
    // underlying without a second pass.
    let positions: Vec<PositionView> = books
        .into_iter()
        .map(|(underlying, book)| {
            let quantity = match book.side {
                PositionSide::Long => i64::from(book.quantity_abs),
                PositionSide::Short => -i64::from(book.quantity_abs),
            };
            position_view(
                &Position::new()
                    .underlying(underlying)
                    .quantity(quantity)
                    .avg_price(book.price_entry_average)
                    .build(),
            )
        })
        .collect();
    let open_contracts = positions.iter().map(|position| position.quantity).sum();
    PortfolioView {
        positions,
        filled_orders,
        total_fees: Money::new(fees),
        open_contracts,
        trading_state: trading_state.to_owned(),
    }
}

/// The live armed grant, if any. The gateway's patch path reads through
/// this (never the field) so the lock discipline stays in this module.
pub(crate) fn live_grant(state: &Shared) -> Option<Grant> {
    common_core::sync::lock(&state.armed).clone()
}

/// Shared arm logic for the route and the gateway: validate, enforce
/// tighten-only against the live grant, then store. The `Err` status rides
/// along so both callers map the same refusal the same way.
pub(crate) fn apply_arm(state: &Shared, grant: Grant) -> Result<ArmResponse, (StatusCode, String)> {
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
pub(crate) fn parse_patch(
    document: &serde_json::Value,
) -> Result<GrantPatch, (StatusCode, String)> {
    let patch = document.get("patch").ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "grant patch envelope names no patch".to_owned(),
        )
    })?;
    let tiers = patch.as_object().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "grant patch must be an object of tier texts".to_owned(),
        )
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
            (
                StatusCode::BAD_REQUEST,
                format!("grant patch tier {tier} refuses: {error}"),
            )
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
    let current = live_grant(state)
        .ok_or_else(|| (StatusCode::CONFLICT, "no live grant to tighten".to_owned()))?;
    let next = current
        .tighten(patch)
        .map_err(|error| (StatusCode::CONFLICT, error.to_string()))?;
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

/// Chart bars over history: the one headless conversion to the wire
/// container, with its shared skip policy counting drops. Shared by the
/// route and the gateway — no second field-wise mapping.
pub(crate) fn chart_bars_of(history: &[Bar]) -> ChartBars {
    rows_of(history)
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
        return Err((
            StatusCode::BAD_REQUEST,
            "period must be within [1, 500]".to_owned(),
        ));
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
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "no chartable closes".to_owned(),
        ));
    }
    let frame = Ohlcv::close_only(&closes);
    let params = ParamSet::new().with_int("length", period);
    let computed = match kind {
        "sma" => SmaIndicator::new()
            .batch(&frame, &params)
            .map(|result| result.values),
        "ema" => EmaIndicator::new()
            .batch(&frame, &params)
            .map(|result| result.values),
        // Chartable overlaps only: `bb` serves its mid (the primary leg),
        // `trima` its single output. Momentum, volume, and statistic kernels
        // are not price-chartable and stay refused below.
        "bb" => BbIndicator::new()
            .batch(&frame, &params.clone().with_float("mult", mult))
            .map(|result| result.values),
        "trima" => TrimaIndicator::new()
            .batch(&frame, &params)
            .map(|result| result.values),
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
    Ok(OverlaySeries {
        kind: kind.to_owned(),
        period,
        values,
        dropped,
    })
}

async fn chart_overlay(
    State(state): State<Shared>,
    Query(query): Query<OverlayQuery>,
) -> Result<Json<OverlaySeries>, (StatusCode, Json<ErrorBody>)> {
    overlay_series_of(
        &state.history,
        &query.kind,
        query.period,
        query.mult.unwrap_or(2.0),
    )
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
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("replay step missing: {key:?}"),
            )
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
    Ok(ReplayView {
        cursor: total_bars,
        total_bars,
        steps,
    })
}

async fn replay(
    State(state): State<Shared>,
    Query(query): Query<ReplayQuery>,
) -> Result<Json<ReplayView>, (StatusCode, Json<ErrorBody>)> {
    replay_view_of(
        &state.history,
        query
            .checkpoint_every
            .unwrap_or(DEFAULT_REPLAY_CHECKPOINT_EVERY),
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
    Some(
        serde_json::json!({
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
        .to_string(),
    )
}

/// Build a boot-gate unit: `check` runs inside `execute` so a failing gate
/// cancels its dependents through the batch graph.
fn gate_unit(
    name: &str,
    check: impl Fn() -> Result<String, String> + Send + Sync + 'static,
) -> Arc<dyn Component> {
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

    /// A snapshot of the tape-serve outcome counters the gateway records as
    /// it serves chart reads. Observation only: the counts never gate a read.
    #[must_use]
    pub fn tape_serve_stats(&self) -> TapeServeSnapshot {
        self.state.tape_serve_stats.snapshot()
    }

    /// Stop serving: close the feed scope, then stop the listener task.
    pub async fn shutdown(self) {
        let mut scope = self.scope;
        scope.close().await;
        self.serve.abort();
    }
}

/// Boot the HTTP surface over `history`, with no durable tape behind it.
pub async fn serve(config: ServerConfig, history: Vec<Bar>) -> Result<ServerHandle, ServerError> {
    serve_with_tape(config, history, None).await
}

/// Boot-load option-chain history from the chain feed config, when one is
/// named. A named-but-unreadable config, a failed validation, or a named
/// chain file that will not parse refuses to bind (fail closed): a replay
/// must never open against half-loaded chains. No config — or a config
/// naming no chain file, or an empty file — loads `None`, and the chain
/// gate stays exactly where it was.
fn load_chain_history_at_boot(
    config: &ServerConfig,
) -> Result<Option<HistoricalChainSource>, ServerError> {
    let Some(feed_path) = config.chain_feed_path.as_deref() else {
        return Ok(None);
    };
    let feed = ChainFeedConfig::load(Path::new(feed_path));
    feed.validate()?;
    let Some(chain_path) = feed.chain_data_path.as_deref() else {
        return Ok(None);
    };
    let quotes = crate::chain_history::load_chain_history(Path::new(chain_path))?;
    if quotes.is_empty() {
        return Ok(None);
    }
    Ok(Some(HistoricalChainSource::new(quotes)))
}

/// Boot the HTTP surface over `history`, optionally with a durable tape the
/// symbol-scoped chart reads plan and read through.
///
/// Boot gates (config validity, non-empty history) run under
/// containment-only supervision; any failure refuses to bind. The rehearsal
/// feed pump runs as a server-owned task under the returned handle's scope.
/// When `tape` is supplied, the gateway's `chart_bars_for` plans through it
/// and falls back to the boot history when the tape cannot serve a read; a
/// `None` tape serves the boot history exactly as before.
pub async fn serve_with_tape(
    config: ServerConfig,
    history: Vec<Bar>,
    tape: Option<TapeReader>,
) -> Result<ServerHandle, ServerError> {
    config.validate().map_err(ServerError::Service)?;
    if history.is_empty() {
        return Err(ServerError::Service("history must not be empty".to_owned()));
    }
    // Validated above; parsed once here so the gateway folds from a real
    // base and never re-parses a string per read.
    let history_timeframe = timeframe_of_label(&config.history_timeframe).ok_or_else(|| {
        ServerError::Service(format!(
            "history_timeframe '{}' is not a known bar spacing",
            config.history_timeframe
        ))
    })?;
    // Symbol discovery needs the root the history came from. Only a
    // directory boot has one; a file boot serves the shipped offline list.
    let history_root = config
        .history_path
        .as_deref()
        .map(Path::new)
        .filter(|path| path.is_dir())
        .map(HistoryRoot::open)
        .transpose()
        .map_err(|error| ServerError::Service(format!("history root unreadable: {error}")))?;
    // Option chains load once at boot from the chain feed config file, when
    // one is named — never per replay open, and never from the network.
    // Absent config (or absent chain file) leaves `None`, and the chain gate
    // stays exactly where it was.
    let chain_history = load_chain_history_at_boot(&config)?;
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
        return Err(ServerError::Service(
            "boot gates refused to bind".to_owned(),
        ));
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
        history_timeframe,
        inbox: Mutex::new(Vec::new()),
        armed: Mutex::new(None),
        portfolio: Mutex::new(PortfolioView::default()),
        feed: feed_tx.clone(),
        trading_state: StateHandle::new(TradingState::Enabled),
        last_command: Mutex::new(None),
        store_health: Mutex::new(config.store_health),
        tape,
        tape_serve_stats: Arc::new(TapeServeStats::default()),
        fallback_latch: Mutex::new(FallbackLatch::default()),
        replay: Mutex::new(ReplayRegistry::new()),
        chain_history,
        history_root,
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
    Ok(ServerHandle {
        addr,
        serve,
        scope,
        state,
    })
}

#[cfg(test)]
#[path = "../tests/api.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/views_golden.rs"]
mod views_golden_tests;

#[cfg(test)]
#[path = "../tests/health_view_store.rs"]
mod health_view_store_tests;

#[cfg(test)]
#[path = "../tests/store_halt_end_to_end.rs"]
mod store_halt_end_to_end_tests;

#[cfg(test)]
#[path = "../tests/trading_status.rs"]
mod trading_status_tests;

#[cfg(test)]
#[path = "../tests/labels.rs"]
mod labels_tests;

#[cfg(test)]
#[path = "../tests/tape_boot.rs"]
mod tape_boot_tests;

#[cfg(test)]
#[path = "../tests/chart_contract.rs"]
mod chart_contract_tests;
