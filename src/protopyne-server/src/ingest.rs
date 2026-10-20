//! Always-on candle ingestion: closed bars only, one writer, one recovery
//! coordinator.
//!
//! The service subscribes to a [`MarketDataFeed`], appends closed bars under
//! [`WritePolicy::InsertIfAbsent`], counts duplicate/out-of-order conflicts,
//! suspends entries on a real gap, and delegates repair to the existing
//! [`RecoveryCoordinator`]. Forming bars are never stored.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use fluent_wvr::prelude::*;
use protopyne_store::{
    CandleStore, Coverage, FreshnessPolicy, HistoryProvider, MarketTape, RecoveryCoordinator,
    RepairConfig, StoreStorage, TapeKey, TimeRange, WritePolicy,
};
use protopyne_types::session::SessionCalendar;
use protopyne_types::Timeframe;
use protopyne_venue::{
    BackfillHandle, FeedError, FeedHealth, FeedSession, FeedStatsSnapshot, FeedSubscription,
    MarketDataFeed, MarketEvent,
};

use crate::error::ServerError;

/// Ingestion tunables.
#[derive(
    Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize, FieldAccess, Describable,
)]
#[builder(start_fn = new)]
pub struct IngestConfig {
    /// Tape root this service writes through.
    #[field(desc = "Durable tape root")]
    #[builder(default)]
    #[serde(default)]
    pub tape_root: String,
    /// Registered venue spelling.
    #[field(desc = "Venue spelling")]
    #[builder(default = "tastytrade".to_owned())]
    #[serde(default = "default_venue")]
    pub venue: String,
    /// Symbols to ingest.
    #[field(skip)]
    #[builder(default)]
    #[serde(default)]
    pub symbols: Vec<String>,
    /// Bar spacing label (`1M`, `5M`, ...).
    #[field(desc = "Bar spacing label (1M, 5M, 1H, 1D)")]
    #[builder(default = "1M".to_owned())]
    #[serde(default = "default_timeframe")]
    pub timeframe: String,
    /// Bounded feed outbox depth.
    #[field(desc = "Feed outbox depth", min = 1, max = 1_000_000)]
    #[builder(default = 256)]
    #[serde(default = "default_queue_bound")]
    pub queue_bound: usize,
    /// Backfill bars requested on an empty tape at boot.
    #[field(desc = "Bootstrap bars on an empty tape", min = 1, max = 1_000_000)]
    #[builder(default = 500)]
    #[serde(default = "default_bootstrap_bars")]
    pub bootstrap_bars: u64,
    /// Tail lag tolerated as fresh.
    #[field(desc = "Freshness lag bars", min = 0)]
    #[builder(default = 1)]
    #[serde(default = "default_lag")]
    pub max_lag_bars: u64,
    /// Interior missing bars tolerated as fresh.
    #[field(desc = "Freshness gap bars", min = 0)]
    #[builder(default = 0)]
    #[serde(default)]
    pub max_gap_bars: u64,
    /// One repair attempt's widest span in milliseconds.
    #[field(desc = "Widest repair span in milliseconds", min = 1)]
    #[builder(default = 604_800_000)]
    #[serde(default = "default_repair_span")]
    pub max_repair_span_ms: u64,
    /// Concurrent symbol repairs.
    #[field(desc = "Concurrent repairs", min = 1, max = 64)]
    #[builder(default = 4)]
    #[serde(default = "default_repair_concurrency")]
    pub repair_concurrency: u32,
}

fn default_venue() -> String {
    "tastytrade".to_owned()
}
fn default_timeframe() -> String {
    "1M".to_owned()
}
fn default_queue_bound() -> usize {
    256
}
fn default_bootstrap_bars() -> u64 {
    500
}
fn default_lag() -> u64 {
    1
}
fn default_repair_span() -> u64 {
    604_800_000
}
fn default_repair_concurrency() -> u32 {
    4
}

impl Default for IngestConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl IngestConfig {
    /// Validate the shape fail-closed.
    pub fn validate(&self) -> Result<(), String> {
        if self.venue.trim().is_empty() {
            return Err("ingest venue must not be blank".to_owned());
        }
        if Timeframe::from_chart_label(&self.timeframe).is_none() {
            return Err(format!(
                "ingest timeframe {:?} is not a known bar spacing",
                self.timeframe
            ));
        }
        if self.symbols.is_empty() {
            return Err("ingest symbols must not be empty".to_owned());
        }
        if self.queue_bound < 1 {
            return Err("queue_bound must be at least 1".to_owned());
        }
        if self.bootstrap_bars < 1 {
            return Err("bootstrap_bars must be at least 1".to_owned());
        }
        if self.max_repair_span_ms < 1 || self.repair_concurrency < 1 {
            return Err("repair bounds must be at least 1".to_owned());
        }
        Ok(())
    }

    /// The store freshness policy this config describes.
    #[must_use]
    pub fn freshness(&self) -> FreshnessPolicy {
        FreshnessPolicy::new()
            .max_lag_bars(self.max_lag_bars)
            .max_gap_bars(self.max_gap_bars)
            .build()
    }

    /// The repair bounds this config describes.
    #[must_use]
    pub fn repair(&self) -> RepairConfig {
        RepairConfig::new()
            .max_repair_span_ms(self.max_repair_span_ms)
            .repair_concurrency(self.repair_concurrency)
            .build()
    }
}

impl protopyne_types::Validate for IngestConfig {
    type Err = String;
    fn validate(&self) -> Result<(), Self::Err> {
        IngestConfig::validate(self)
    }
}

impl WorkUnit for IngestConfig {
    fn name(&self) -> &str {
        "ingest_config"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        protopyne_types::check_for_execute(self, "ingest config invalid")?;
        Ok(protopyne_types::valid_output("ingest config valid"))
    }
}

impl_component!(IngestConfig);

/// One ingestion health reading. Additive observation only.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct IngestHealth {
    /// Last observed feed counters.
    pub feed: FeedStatsSnapshot,
    /// Largest committed watermark age in milliseconds.
    pub watermark_age_ms: Option<u64>,
    /// Symbols with an open, unrepaired gap.
    pub open_gaps: usize,
    /// Last repair status, in words.
    pub last_repair: Option<String>,
    /// Duplicate live bars observed.
    pub conflicts: u64,
    /// Out-of-order bars refused.
    pub out_of_order: u64,
}

/// What one closed event did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Ignored: forming bar or non-bar event.
    Ignored,
    /// A bar was appended.
    Appended,
    /// The bar collided with an existing stamp; no second row.
    Duplicate,
    /// The bar is older than the committed watermark; refused.
    OutOfOrder,
    /// A gap was detected; entries are suspended until one repair pass lands.
    Gap {
        /// Tape the gap belongs to.
        symbol: String,
        /// First missing stamp.
        from_ms: i64,
        /// First present stamp after the gap.
        to_ms: i64,
    },
    /// The event named no registered tape.
    UnknownSymbol(String),
}

/// One always-on ingest writer.
pub struct IngestService {
    config: IngestConfig,
    tape: Arc<MarketTape>,
    permit: StoreStorage,
    keys: HashMap<String, (TapeKey, Timeframe)>,
    last: Mutex<HashMap<TapeKey, i64>>,
    recovery: RecoveryCoordinator,
    backfill: BackfillHandle,
    health: Mutex<IngestHealth>,
    feed_subscriptions: Vec<FeedSubscription>,
    feed: Arc<dyn MarketDataFeed>,
    /// The exchange-session interpretation: only a span with due trading
    /// bars reads as a gap. Weekends, holidays, and overnight stretches
    /// skip nothing that was due, so they append without suspending.
    calendar: SessionCalendar,
}

impl IngestService {
    /// Build the service. Every subscription must resolve to a registered
    /// market at the configured spacing; unknown symbols refuse at boot.
    /// The calendar names which spans were due bars; the default is regular
    /// US equity hours with no dated exceptions.
    pub fn new(
        config: IngestConfig,
        tape: Arc<MarketTape>,
        permit: StoreStorage,
        feed: Arc<dyn MarketDataFeed>,
        providers: Vec<(protopyne_store::SymbolId, Arc<dyn HistoryProvider>)>,
        calendar: SessionCalendar,
    ) -> Result<Self, ServerError> {
        config.validate().map_err(ServerError::Service)?;
        Timeframe::from_chart_label(&config.timeframe).ok_or_else(|| {
            ServerError::Service(format!(
                "ingest timeframe {:?} is not a known bar spacing",
                config.timeframe
            ))
        })?;
        let mut keys = HashMap::new();
        let mut subscriptions = Vec::new();
        for symbol in &config.symbols {
            let (symbol_id, registered) = tape
                .registry()
                .tape_of(symbol, &config.venue)
                .ok_or_else(|| {
                    ServerError::Service(format!(
                        "ingest symbol {symbol:?} is not registered on {:?}",
                        config.venue
                    ))
                })?;
            let key = TapeKey {
                symbol_id,
                timeframe: registered,
            };
            keys.insert(symbol.clone(), (key, registered));
            subscriptions.push(FeedSubscription::bars(symbol.clone(), registered));
        }
        let recovery = RecoveryCoordinator::new(Arc::clone(&tape), providers, config.repair())
            .map_err(|error| ServerError::Service(format!("ingest recovery refused: {error}")))?;
        Ok(Self {
            config,
            tape,
            permit,
            keys,
            last: Mutex::new(HashMap::new()),
            recovery,
            backfill: BackfillHandle::new(),
            health: Mutex::new(IngestHealth::default()),
            feed_subscriptions: subscriptions,
            feed,
            calendar,
        })
    }

    /// The entry gate this service drives.
    #[must_use]
    pub fn backfill_handle(&self) -> &BackfillHandle {
        &self.backfill
    }

    /// Current additive health reading.
    #[must_use]
    pub fn health(&self) -> IngestHealth {
        common_core::sync::lock(&self.health).clone()
    }

    /// Resolve markets, read coverage, run one repair pass, and seed last
    /// stamps from the tape (never from memory).
    pub async fn bootstrap(
        &self,
        now_ms: i64,
    ) -> Result<Vec<protopyne_store::SymbolRepair>, ServerError> {
        let mut items = Vec::new();
        for (symbol, (key, timeframe)) in &self.keys {
            let coverage = self
                .tape
                .coverage(*key)
                .map_err(|error| ServerError::Service(format!("{symbol}: {error}")))?;
            let end = now_ms;
            let start = coverage.as_ref().map_or_else(
                || {
                    end.saturating_sub(
                        timeframe
                            .duration_ms()
                            .saturating_mul(self.config.bootstrap_bars as i64),
                    )
                },
                |coverage| coverage.last.ts_ms.saturating_add(timeframe.duration_ms()),
            );
            let want = TimeRange::bounded(
                start.max(end.saturating_sub(self.config.max_repair_span_ms as i64)),
                end,
            )
            .map_err(|error| ServerError::Service(error.to_string()))?;
            items.push((*key, want));
            if let Some(coverage) = coverage {
                self.seed(*key, coverage.last.ts_ms);
            }
        }
        let plan = self
            .recovery
            .plan(&self.permit, &items, now_ms, self.config.freshness())
            .map_err(|error| ServerError::Service(format!("ingest plan refused: {error}")))?;
        let reports = self.recovery.run_once(&self.permit, &plan).await;
        if !reports.is_empty() {
            self.publish_repair(&reports);
            self.seed_after_repair();
        }
        let now = Utc::now().timestamp_millis();
        let age = self.watermark_age(now.max(now_ms));
        self.bump(|health| health.watermark_age_ms = age);
        Ok(reports)
    }

    /// Ingest one market event. Only closed bars write; forming updates are
    /// display-only.
    pub fn handle_event(&self, event: &MarketEvent) -> IngestOutcome {
        let (symbol, bar, closed) = match event {
            MarketEvent::Candle { underlying, bar } => (underlying, bar, true),
            MarketEvent::BarUpdate { update } => (&update.underlying, &update.bar, update.closed),
            _ => return IngestOutcome::Ignored,
        };
        if !closed {
            return IngestOutcome::Ignored;
        }
        let Some((key, timeframe)) = self.keys.get(symbol).copied() else {
            return IngestOutcome::UnknownSymbol(symbol.clone());
        };
        let ts = bar.ts.timestamp_millis();
        let last = common_core::sync::lock(&self.last).get(&key).copied();
        if let Some(last) = last {
            if ts == last {
                self.bump(|health| health.conflicts += 1);
                return IngestOutcome::Duplicate;
            }
            if ts < last {
                self.bump(|health| health.out_of_order += 1);
                return IngestOutcome::OutOfOrder;
            }
        }
        let append = self
            .tape
            .append(key, std::slice::from_ref(bar), WritePolicy::InsertIfAbsent);
        let outcome = match append {
            Ok(outcome) if outcome.inserted > 0 => {
                self.seed(key, ts);
                IngestOutcome::Appended
            }
            Ok(outcome) if outcome.conflicts > 0 => {
                self.bump(|health| health.conflicts += 1);
                return IngestOutcome::Duplicate;
            }
            Ok(_) => IngestOutcome::Appended,
            Err(error) => {
                self.bump(|health| {
                    health.last_repair = Some(format!("append refused: {error}"));
                });
                IngestOutcome::Ignored
            }
        };
        if let Some(last) = last {
            let expected = last.saturating_add(timeframe.duration_ms());
            if ts > expected
                && self
                    .calendar
                    .trading_buckets_between(last, ts, timeframe.duration_ms())
                    > 0
            {
                let reason = format!("ingest gap on {symbol}: missing bars in [{expected}, {ts})");
                self.backfill.suspend(reason.clone());
                self.bump(|health| {
                    health.open_gaps = health.open_gaps.saturating_add(1);
                    health.last_repair = Some(reason);
                });
                return IngestOutcome::Gap {
                    symbol: symbol.clone(),
                    from_ms: expected,
                    to_ms: ts,
                };
            }
        }
        outcome
    }

    /// Run one recovery pass after a gap and resume only when the repaired
    /// window is contiguous.
    pub async fn repair_gap(
        &self,
        symbol: &str,
        from_ms: i64,
        to_ms: i64,
        now_ms: i64,
    ) -> Result<bool, ServerError> {
        let Some((key, timeframe)) = self.keys.get(symbol).copied() else {
            return Ok(false);
        };
        let want = TimeRange::bounded(from_ms, to_ms)
            .map_err(|error| ServerError::Service(error.to_string()))?;
        let plan = self
            .recovery
            .plan(
                &self.permit,
                &[(key, want)],
                now_ms,
                self.config.freshness(),
            )
            .map_err(|error| {
                ServerError::Service(format!("ingest repair plan refused: {error}"))
            })?;
        let reports = self.recovery.run_once(&self.permit, &plan).await;
        self.publish_repair(&reports);
        let coverage = self
            .tape
            .coverage(key)
            .map_err(|error| ServerError::Service(format!("{symbol}: {error}")))?;
        let holes = coverage
            .as_ref()
            .map_or_else(|| vec![want], |coverage| coverage.holes(&want, timeframe));
        let complete = holes.is_empty();
        self.backfill.observe(&protopyne_venue::BackfillReport {
            complete,
            filled: reports.iter().map(|report| report.inserted).sum(),
            alerts: if complete {
                Vec::new()
            } else {
                vec![format!("{symbol}: gap [{from_ms}, {to_ms}) still open")]
            },
        });
        self.bump(|health| {
            health.open_gaps = if complete {
                health.open_gaps.saturating_sub(1)
            } else {
                health.open_gaps
            };
            health.last_repair = Some(format!(
                "{symbol}: {} repair report(s), complete={complete}",
                reports.len()
            ));
        });
        Ok(complete)
    }

    /// Drive the feed until abort or channel close. A gap triggers exactly
    /// one repair pass, never a second repair loop.
    pub async fn run(&self, mut session: FeedSession) -> Result<(), ServerError> {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            tokio::select! {
                () = session.abort.cancelled() => return Ok(()),
                _ = interval.tick() => {}
                received = session.outbox.recv() => {
                    let Some(event) = received else { return Ok(()) };
                    session.stats.record_received();
                    if let IngestOutcome::Gap { symbol, from_ms, to_ms } =
                        self.handle_event(&event)
                    {
                        let now = Utc::now().timestamp_millis();
                        self.repair_gap(&symbol, from_ms, to_ms, now).await?;
                    }
                    let snapshot = session.stats.snapshot();
                    let feed_health = FeedHealth::of(snapshot, Utc::now().timestamp_millis());
                    self.bump(|health| {
                        health.feed = snapshot;
                        if feed_health.degraded {
                            health.last_repair = feed_health.reason;
                        }
                        health.watermark_age_ms = self.watermark_age(Utc::now().timestamp_millis());
                    });
                }
            }
        }
    }

    /// Open one feed session over the configured subscriptions.
    pub async fn subscribe(&self) -> Result<FeedSession, FeedError> {
        self.feed.subscribe(&self.feed_subscriptions).await
    }

    fn seed(&self, key: TapeKey, ts_ms: i64) {
        let mut last = common_core::sync::lock(&self.last);
        let slot = last.entry(key).or_insert(ts_ms);
        *slot = (*slot).max(ts_ms);
    }

    fn seed_after_repair(&self) {
        for (key, _) in self.keys.values() {
            if let Ok(Some(coverage)) = self.tape.coverage(*key) {
                self.seed(*key, coverage.last.ts_ms);
            }
        }
    }

    fn watermark_age(&self, now_ms: i64) -> Option<u64> {
        common_core::sync::lock(&self.last)
            .values()
            .max()
            .map(|last| u64::try_from(now_ms.saturating_sub(*last)).unwrap_or(0))
    }

    fn publish_repair(&self, reports: &[protopyne_store::SymbolRepair]) {
        let summary = reports
            .iter()
            .map(|report| format!("{:?}", report.status))
            .collect::<Vec<_>>()
            .join(",");
        self.bump(|health| health.last_repair = Some(summary));
    }

    fn bump(&self, update: impl FnOnce(&mut IngestHealth)) {
        let mut health = common_core::sync::lock(&self.health);
        update(&mut health);
    }
}

/// Read one coverage's last watermark without requiring a full read.
#[must_use]
pub fn last_stamp(coverage: Option<&Coverage>) -> Option<i64> {
    coverage.map(|coverage| coverage.last.ts_ms)
}

#[cfg(test)]
#[path = "../tests/ingest.rs"]
mod tests;
