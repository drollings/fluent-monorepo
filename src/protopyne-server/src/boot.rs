//! Boot and wiring: the server config, the shared [`AppState`], the durable
//! tape reader, and the two boot entry points. Serving lives here so the
//! routes stay thin over the state this module owns.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use fluent_concurrency::batch::SupervisedBatch;
use fluent_concurrency::pool::Limiter;
use fluent_concurrency::scope::Scope;
use fluent_wvr::prelude::*;
use tokio::sync::broadcast;

use protopyne_store::{
    FoldPolicy, HistoryProvider, MarketTape, PlanError, ReadRequest as TapeReadRequest, StoreError,
    StoreHealth, StoreStorage, SymbolId, TimeRange,
};
use protopyne_strategy::{Grant, RiskLimits};
use protopyne_types::session::SessionCalendar;
use protopyne_types::{Bar, Timeframe};
use protopyne_venue::{
    BrokerRegistry, Journal, MarketDataFeed, StateHandle, TicketTracker, TradingState, VenueStorage,
};
use protopyne_view::{CommandStatus, PortfolioView, ProposalView};

use crate::backtest_registry::BacktestRegistry;
use crate::chain_history::{ChainFeedConfig, HistoricalChainSource};
use crate::error::ServerError;
use crate::gateway::FallbackLatch;
use crate::history_root::HistoryRoot;
use crate::ingest::IngestService;
use crate::replay_session::ReplayRegistry;
use crate::views::{bar_frame, live_grant};

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
    /// Optional always-on candle ingestion configuration. Absent keeps the
    /// server path byte-identical; a boot that attaches an
    /// [`IngestService`](crate::ingest::IngestService) publishes its health
    /// through [`AppState::publish_ingest`].
    #[field(skip)]
    #[serde(default)]
    pub ingest: Option<crate::ingest::IngestConfig>,
}

impl fluent_wvr::validate::Validate for ServerConfig {
    type Err = String;
    /// Fail-closed self-consistency check shared by supervision and boot.
    fn validate(&self) -> Result<(), Self::Err> {
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
        if let Some(ingest) = &self.ingest {
            ingest.validate()?;
        }
        Ok(())
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self::new().build()
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
        fluent_wvr::validate::check_for_execute(self, "server config invalid")?;
        Ok(fluent_wvr::validate::valid_output("server config valid"))
    }
}

impl_component!(ServerConfig);

/// Shared server state behind one `Arc`.
pub struct AppState {
    pub(crate) limiter: Limiter,
    pub(crate) history: Vec<Bar>,
    /// Bar spacing of [`Self::history`], so the gateway folds from the base
    /// the history actually holds — never an assumed one.
    pub(crate) history_timeframe: Timeframe,
    pub(crate) inbox: Mutex<Vec<ProposalView>>,
    pub(crate) armed: Mutex<Option<Grant>>,
    /// Server command journal: the authority for arming. `live_grant` folds
    /// its rows rather than trusting the in-memory cache, which is only a
    /// projection of that fold.
    pub(crate) journal: Arc<Journal>,
    /// Storage permit for the server command journal.
    pub(crate) journal_permit: VenueStorage,
    pub(crate) portfolio: Mutex<PortfolioView>,
    /// Venue-booked fills from the last executed run: the authoritative
    /// portfolio source. The rehearsal projection above is a separate cache.
    pub(crate) booked: Mutex<Vec<crate::portfolio::BookedFill>>,
    pub(crate) feed: broadcast::Sender<String>,
    /// Command-plane authority the bars gate on: shared ownership so an
    /// operator plane driving the same handle halts and resumes this
    /// surface. Serving-path reads only — no route mutates it.
    pub(crate) trading_state: StateHandle,
    /// Manual tickets awaiting the human's fill report. Mutated only through
    /// the tracker's journal-first path, so the in-memory wait always matches
    /// the journaled rows a restart would replay.
    pub(crate) tickets: Mutex<TicketTracker>,
    /// Read-only echo of the last operator command, if any.
    pub(crate) last_command: Mutex<Option<CommandStatus>>,
    /// Latest observed store liveness, seeded from [`ServerConfig`] at boot.
    /// Serving-path reads only; the repair worker publishes live readings
    /// here once it exists.
    pub(crate) store_health: Mutex<StoreHealth>,
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
    /// Stored backtest runs by run id. A run is projected once and read many
    /// times, so the terminal never re-runs the tape to re-read a report.
    pub(crate) backtests: Mutex<BacktestRegistry>,
    /// Boot-loaded option-chain history, when the server was booted with a
    /// chain feed config naming a chain data file. The gateway slices the
    /// replay window out of this at open; `None` (or an empty load) keeps
    /// the chain gate exactly where it was.
    pub(crate) chain_history: Option<HistoricalChainSource>,
    /// Broker read/feed/auth directory. Empty until a boot or integration
    /// registers a bundle; an empty directory refuses reads honestly.
    pub(crate) broker: BrokerRegistry,
    /// Last ingest health reading, when an ingest service is attached.
    pub(crate) ingest_health: Mutex<Option<crate::ingest::IngestHealth>>,
}

pub(crate) type Shared = Arc<AppState>;

impl AppState {
    /// Publish the latest ingest health reading. Observation only.
    pub fn publish_ingest(&self, health: crate::ingest::IngestHealth) {
        *common_core::sync::lock(&self.ingest_health) = Some(health);
    }

    /// The latest ingest reading, when one was published.
    #[must_use]
    pub fn ingest_health(&self) -> Option<crate::ingest::IngestHealth> {
        common_core::sync::lock(&self.ingest_health).clone()
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

/// Build a boot-gate unit: the shared venue `check_unit` constructor over a
/// throwaway default base. A failing gate cancels its dependents through
/// the batch graph, exactly like the per-bar checks.
fn gate_unit(
    name: &str,
    check: impl Fn() -> Result<String, String> + Send + Sync + 'static,
) -> Arc<dyn Component> {
    protopyne_venue::check_unit(Arc::new(RiskLimits::new().build()), name, check)
}

/// How often the supervised ingest publisher mirrors the service health
/// into shared state, in milliseconds. The run loop observes every event;
/// the mirror only feeds the health view.
const INGEST_HEALTH_PUBLISH_MS: u64 = 250;

/// Optional ingest attachment: the live feed to drain plus one repair
/// provider per registered symbol id. The tunables (symbols, spacing, lag)
/// ride `ServerConfig.ingest`; this carries the two live ends the config
/// cannot name. `None` keeps the boot byte-identical.
pub struct IngestBoot {
    /// The feed the service subscribes to.
    pub feed: Arc<dyn MarketDataFeed>,
    /// Repair providers keyed by registered symbol id.
    pub providers: Vec<(SymbolId, Arc<dyn HistoryProvider>)>,
}

/// Owned server: listener task plus the feed supervision scope.
pub struct ServerHandle {
    /// Bound address.
    pub addr: SocketAddr,
    serve: tokio::task::JoinHandle<()>,
    scope: Scope,
    pub(crate) state: Shared,
}

impl ServerHandle {
    /// Read gateway over the live state: the same views the routes serve,
    /// without the HTTP hop (the gateway-route parity surface).
    #[must_use]
    pub fn gateway(&self) -> crate::gateway::ServerGateway {
        crate::gateway::ServerGateway::new(Arc::clone(&self.state))
    }

    /// Subscribe to the rehearsal feed: one JSON bar frame per tick. The
    /// terminal drains it once per frame; a lagged channel just repaints.
    #[must_use]
    pub fn feed_receiver(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.state.feed.subscribe()
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
    serve_with_tape(config, history, None, None).await
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
/// symbol-scoped chart reads plan and read through, and optionally with an
/// always-on ingest service draining `ingest.feed` into that tape.
///
/// Boot gates (config validity, non-empty history) run under
/// containment-only supervision; any failure refuses to bind. The rehearsal
/// feed pump runs as a server-owned task under the returned handle's scope.
/// When `tape` is supplied, the gateway's `chart_bars_for` plans through it
/// and falls back to the boot history when the tape cannot serve a read; a
/// `None` tape serves the boot history exactly as before.
///
/// Ingest is fail-closed on shape: config without a feed, a feed without
/// config, ingest without a tape, or an unknown ingest symbol all refuse
/// before binding. The disabled path returns before touching anything, so a
/// boot without ingest is byte-identical. The enabled path bootstraps
/// (resolve markets, one repair pass, seed stamps), subscribes, and spawns
/// the run loop plus a health publisher under the server scope — both
/// re-enter the tape's capability scope, since tokio does not propagate
/// task-locals — so shutdown closes them with the server.
pub async fn serve_with_tape(
    config: ServerConfig,
    history: Vec<Bar>,
    tape: Option<TapeReader>,
    ingest: Option<IngestBoot>,
) -> Result<ServerHandle, ServerError> {
    config.validate().map_err(ServerError::Service)?;
    if history.is_empty() {
        return Err(ServerError::Service("history must not be empty".to_owned()));
    }
    if let Some(error) = ingest_shape_error(&config, tape.as_ref(), ingest.as_ref()) {
        return Err(error);
    }
    let history_timeframe = timeframe_of_label(&config.history_timeframe).ok_or_else(|| {
        ServerError::Service(format!(
            "history_timeframe '{}' is not a known bar spacing",
            config.history_timeframe
        ))
    })?;
    let history_root = config
        .history_path
        .as_deref()
        .map(Path::new)
        .filter(|path| path.is_dir())
        .map(HistoryRoot::open)
        .transpose()
        .map_err(|error| ServerError::Service(format!("history root unreadable: {error}")))?;
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
    let (feed_tx, _) = broadcast::channel(config.feed_capacity as usize);
    let journal = Arc::new(Journal::ephemeral());
    let journal_permit = VenueStorage::new(std::path::PathBuf::from("server-command-journal"));
    let state = Arc::new(AppState {
        // M6.7: composes fluent_concurrency::Limiter directly (no fork).
        limiter: Limiter::new(config.max_concurrent_requests as usize),
        history,
        history_timeframe,
        inbox: Mutex::new(Vec::new()),
        armed: Mutex::new(None),
        journal,
        journal_permit,
        portfolio: Mutex::new(PortfolioView::default()),
        booked: Mutex::new(Vec::new()),
        feed: feed_tx.clone(),
        trading_state: StateHandle::new(TradingState::Enabled),
        tickets: Mutex::new(TicketTracker::new()),
        last_command: Mutex::new(None),
        store_health: Mutex::new(config.store_health),
        tape,
        tape_serve_stats: Arc::new(TapeServeStats::default()),
        fallback_latch: Mutex::new(FallbackLatch::default()),
        replay: Mutex::new(ReplayRegistry::new()),
        backtests: Mutex::new(BacktestRegistry::new()),
        chain_history,
        history_root,
        broker: BrokerRegistry::new(),
        ingest_health: Mutex::new(None),
    });
    live_grant(&state);
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
    if let Err(error) = boot_ingest(&config, &state, ingest, &mut scope).await {
        scope.close().await;
        return Err(error);
    }
    let app = crate::routes::router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind(&config.bind)
        .await
        .map_err(|e| ServerError::Service(format!("bind failed: {e}")))?;
    let addr = listener
        .local_addr()
        .map_err(|e| ServerError::Service(format!("bound address unknown: {e}")))?;
    // `serve` stays a separate JoinHandle (serve lifetime vs scope-drain
    // semantics); the pump loop above is Scope-owned and aborts on close.
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

/// The wiring-shape matrix, fail-closed before anything binds: config
/// without a feed, a feed without config, or ingest without a tape all
/// refuse. The disabled path is `None` here.
fn ingest_shape_error(
    config: &ServerConfig,
    tape: Option<&TapeReader>,
    boot: Option<&IngestBoot>,
) -> Option<ServerError> {
    if config.ingest.is_none() {
        return boot.map(|_| {
            ServerError::Service(
                "ingest feed supplied without ingest config: set `ingest` or drop the feed"
                    .to_owned(),
            )
        });
    }
    if boot.is_none() {
        return Some(ServerError::Service(
            "ingest configured but no feed attached: the served boot cannot ingest without a feed"
                .to_owned(),
        ));
    }
    if tape.is_none() {
        return Some(ServerError::Service(
            "ingest configured but the server booted without a durable tape".to_owned(),
        ));
    }
    None
}

/// Build, bootstrap, and supervise the optional ingest service. The shape
/// matrix above already refused every half-wired combination before the
/// scope existed, so only the fully-wired path arrives here; a build failure
/// (unknown symbol, failed bootstrap, refused subscribe) still closes the
/// scope on the way out. Construction, bootstrap, and both spawned tasks run
/// inside the tape's capability scope — the store gates on it, and the
/// scope spawn only inherits what is ambient at spawn time.
async fn boot_ingest(
    config: &ServerConfig,
    state: &Shared,
    boot: Option<IngestBoot>,
    scope: &mut Scope,
) -> Result<(), ServerError> {
    let (Some(ingest_config), Some(boot), Some(tape)) =
        (config.ingest.clone(), boot, state.tape.as_ref())
    else {
        return Ok(());
    };
    let service = Arc::new(
        fluent_wvr::CURRENT_CAPS
            .scope(tape.caps.clone(), async {
                let service = IngestService::new(
                    ingest_config,
                    Arc::clone(&tape.store),
                    tape.permit.clone(),
                    boot.feed,
                    boot.providers,
                    // Production default: regular US equity hours, weekends
                    // closed, no dated exceptions. Dated holiday and half-day
                    // lists ride a future config surface; until then a holiday
                    // reads as an unfillable gap rather than a silent skip.
                    SessionCalendar::us_equities(),
                )?;
                service.bootstrap(Utc::now().timestamp_millis()).await?;
                Ok::<_, ServerError>(service)
            })
            .await?,
    );
    let session = service
        .subscribe()
        .await
        .map_err(|error| ServerError::Service(format!("ingest subscribe refused: {error}")))?;
    let run_service = Arc::clone(&service);
    let publish_service = Arc::clone(&service);
    let publish_state = Arc::clone(state);
    fluent_wvr::CURRENT_CAPS
        .scope(tape.caps.clone(), async {
            scope.spawn(async move {
                // The loop only exits on abort or channel close; either way
                // the publisher keeps mirroring the last reading, whose
                // growing watermark age names the stall.
                let _ = run_service.run(session).await;
            });
            scope.spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_millis(
                    INGEST_HEALTH_PUBLISH_MS,
                ));
                loop {
                    publish_state.publish_ingest(publish_service.health());
                    interval.tick().await;
                }
            });
        })
        .await;
    Ok(())
}
