//! The async store surface: a pooled handle behind concurrency limiters.
//!
//! [`StoreHandle`] owns a `fluent_db :: SqlitePool` and offloads the
//! blocking SQLite work the way `DbCapability` does — through the pool's
//! `spawn_blocking` boundary, never inline on the runtime. A read limiter
//! and a write limiter bound concurrent checkouts separately, so a burst
//! of reads cannot starve the single writer the tape discipline assumes.
//! Every method takes the session's [`StoreStorage`] permit and verifies
//! binding before presence: a foreign permit is refused on its face, and a
//! task that was never granted the store is denied, not defaulted.
//!
//! Two tokens, two meanings. The pool's own calls additionally require a
//! `DbCapability` in the task-local set — "this task may use SQLite at
//! all" — while the permit answers "this task may use *this store file*".
//! A supervised session installs both; a missing `DbCapability` surfaces
//! as [`StoreError::Denied`], exactly like a missing permit, because from
//! the caller's side both are the same fact: the session was not wired
//! for store effects.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fluent_concurrency::pool::Limiter;
use fluent_db::pool::{PoolConfig, SqlitePool};
use fluent_wvr::prelude::*;
use protopyne_types::{Bar, Timeframe, Validate, adjust_fetch_bounds, trim_indicator_output};
use rusqlite::params;

use crate::capability::{StoreStorage, check_binding, check_present};
use crate::catalog::{
    AssetClass, Market, PriceScale, SELECT_MARKET_SQL, check_register, market_of,
};
use crate::coverage::{Coverage, FreshnessPolicy};
use crate::error::{PlanError, StoreError};
use crate::fold::{FoldPolicy, fold_bars, narrow_to_window};
use crate::key::{SymbolId, TapeKey};
use crate::migrate::ensure_schema;
use crate::plan::{ReadPlan, wanted_window};
use crate::range::TimeRange;
use crate::repair::{RepairConfig, fill_headroom, repair_spans, spans_for_want};
use crate::sqlite_tape::{
    TapeExtent, append_rows, coverage_from_stamps, coverage_windows, fetch_all_bars, fetch_bars,
    fetch_stamps, order_and_validate, plan_rows,
};
use crate::store::{AppendOutcome, CandleSeries, ReadRequest, WritePolicy};

/// How the handle is opened and bounded. Schema-configurable through the
/// derived `FieldAccess + Describable`, fail-closed through `Validate`,
/// and schedulable as a supervised unit through `WorkUnit`.
#[derive(Debug, Clone, bon::Builder, FieldAccess, Describable)]
#[builder(start_fn = new)]
pub struct StoreConfig {
    /// Store database path. `":memory:"` opens an ephemeral store that
    /// lives exactly as long as the handle.
    #[field(desc = "Store database path; \":memory:\" for an ephemeral store")]
    #[builder(default = ":memory:".to_owned())]
    pub path: String,
    /// Concurrent read checkouts against the pool.
    #[field(desc = "Concurrent read checkouts", min = 1, max = 64)]
    #[builder(default = 4)]
    pub read_concurrency: u32,
    /// Concurrent write checkouts. One writer is the tape discipline; the
    /// default says so, and raising it only widens contention, never
    /// correctness.
    #[field(desc = "Concurrent write checkouts", min = 1, max = 8)]
    #[builder(default = 1)]
    pub write_concurrency: u32,
    /// Pooled connections to the store file.
    #[field(desc = "Pooled connections to the store file", min = 1, max = 32)]
    #[builder(default = 5)]
    pub pool_size: u32,
    /// Market bound enforced on registration: refusing, never evicting.
    #[field(desc = "Market bound enforced on registration", min = 1)]
    #[builder(default = crate::catalog::DEFAULT_MAX_MARKETS as u32)]
    pub max_markets: u32,
}

impl protopyne_types::Validate for StoreConfig {
    type Err = String;

    fn validate(&self) -> Result<(), Self::Err> {
        if self.path.is_empty() {
            return Err("store path must be nonempty".to_owned());
        }
        if self.read_concurrency < 1 {
            return Err("read_concurrency must be at least 1".to_owned());
        }
        if self.write_concurrency < 1 {
            return Err("write_concurrency must be at least 1".to_owned());
        }
        if self.pool_size < 1 {
            return Err("pool_size must be at least 1".to_owned());
        }
        if self.max_markets < 1 {
            return Err("max_markets must be at least 1".to_owned());
        }
        Ok(())
    }
}

impl WorkUnit for StoreConfig {
    fn name(&self) -> &str {
        "store_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        protopyne_types::check_for_execute(self, "store config invalid")?;
        Ok(protopyne_types::valid_output("store config valid"))
    }
}

impl_component!(StoreConfig);

/// The async gateway to one store file: pooled connections, bounded
/// concurrency, capability-gated effects.
pub struct StoreHandle {
    pool: Arc<SqlitePool>,
    path: PathBuf,
    read_gate: Limiter,
    write_gate: Limiter,
    max_markets: u32,
}

impl StoreHandle {
    /// Open (or create) the store: validate the configuration, open the
    /// pool, and bring the schema current on a pooled connection — so the
    /// file-backed and the ephemeral paths provision the exact database
    /// the pool then serves.
    ///
    /// Requires a `DbCapability` in the calling task's capability set for
    /// the pooled provisioning, like every other pool consumer.
    pub async fn open(config: &StoreConfig) -> Result<Self, StoreError> {
        config.validate().map_err(StoreError::Config)?;
        let path = PathBuf::from(&config.path);
        let pool = Arc::new(
            SqlitePool::open(
                &path,
                &PoolConfig {
                    size: config.pool_size as usize,
                    busy_timeout_ms: 5_000,
                },
            )
            .map_err(StoreError::from)?,
        );
        let version: Result<u32, StoreError> = pool
            .with_conn(|conn| Ok(ensure_schema(conn)))
            .await
            .map_err(StoreError::from)?;
        let version = version?;
        tracing::info!(
            path = %path.display(),
            schema_version = version,
            "store opened"
        );
        Ok(Self {
            pool,
            path,
            read_gate: Limiter::new(config.read_concurrency as usize),
            write_gate: Limiter::new(config.write_concurrency as usize),
            max_markets: config.max_markets,
        })
    }

    /// The store file this handle serves.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Binding before presence: a foreign permit is refused on its face,
    /// without consulting the task-local set.
    fn gate(&self, permit: &StoreStorage) -> Result<(), StoreError> {
        check_binding(permit, &self.path)?;
        check_present(permit)?;
        Ok(())
    }

    /// Register a new market, allocating its durable id. Same refusals as
    /// the synchronous core — blank, unrepresentable scale, duplicate,
    /// full — through the same [`check_register`] decision, so the two
    /// layers cannot disagree. The spacing is fixed here, as on the core:
    /// one spacing per symbol.
    pub async fn register(
        &self,
        permit: &StoreStorage,
        symbol: &str,
        venue: &str,
        asset_class: AssetClass,
        price_scale: PriceScale,
        time_zone: &str,
        timeframe: protopyne_types::Timeframe,
    ) -> Result<SymbolId, StoreError> {
        self.gate(permit)?;
        let pool = Arc::clone(&self.pool);
        let symbol_owned = symbol.to_owned();
        let venue_owned = venue.to_owned();
        let (exists, count): (bool, usize) = self
            .read_gate
            .run(|| async move {
                pool.with_conn(move |conn| {
                    let exists: Option<i64> = fluent_db::query::query_row(
                        conn,
                        "SELECT symbol_id FROM market WHERE symbol = ?1 AND venue = ?2",
                        params![symbol_owned, venue_owned],
                        |row| row.get(0),
                    )?;
                    let count: i64 = fluent_db::query::query_row(
                        conn,
                        "SELECT COUNT(*) FROM market",
                        [],
                        |row| row.get(0),
                    )?
                    .unwrap_or(0);
                    Ok((exists.is_some(), count as usize))
                })
                .await
            })
            .await
            .map_err(StoreError::from)?;
        check_register(
            symbol,
            venue,
            price_scale,
            exists,
            count,
            self.max_markets as usize,
        )?;
        let pool = Arc::clone(&self.pool);
        let symbol_owned = symbol.to_owned();
        let venue_owned = venue.to_owned();
        let asset_text = asset_class.as_str().to_owned();
        let scale_digits = price_scale.digits;
        let time_zone_owned = time_zone.to_owned();
        let timeframe_text = timeframe.code().to_owned();
        let id: i64 = self
            .write_gate
            .run(|| async move {
                pool.with_conn(move |conn| {
                    fluent_db::query::execute(
                        conn,
                        "INSERT INTO market(symbol, venue, asset_class, price_scale, time_zone, timeframe)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![symbol_owned, venue_owned, asset_text, i64::from(scale_digits), time_zone_owned, timeframe_text],
                    )?;
                    Ok(fluent_db::query::last_insert_rowid(conn))
                })
                .await
            })
            .await
            .map_err(StoreError::from)?;
        u32::try_from(id)
            .map(SymbolId)
            .map_err(|_| StoreError::InvalidBar(format!("allocated symbol_id {id} exceeds u32")))
    }

    /// Every registered market, in id order.
    pub async fn markets(&self, permit: &StoreStorage) -> Result<Vec<Market>, StoreError> {
        self.gate(permit)?;
        let pool = Arc::clone(&self.pool);
        let mut markets: Vec<Market> = self
            .read_gate
            .run(|| async move {
                pool.with_conn(move |conn| {
                    fluent_db::query::query_rows(conn, SELECT_MARKET_SQL, [], market_of)
                })
                .await
            })
            .await
            .map_err(StoreError::from)?;
        markets.sort_by_key(|market| market.symbol_id);
        Ok(markets)
    }

    /// Resolve `(symbol, venue)` to its durable id. Reads, never creates.
    pub async fn resolve(
        &self,
        permit: &StoreStorage,
        symbol: &str,
        venue: &str,
    ) -> Result<Option<SymbolId>, StoreError> {
        self.gate(permit)?;
        let pool = Arc::clone(&self.pool);
        let symbol_owned = symbol.to_owned();
        let venue_owned = venue.to_owned();
        let id: Option<i64> = self
            .read_gate
            .run(|| async move {
                pool.with_conn(move |conn| {
                    fluent_db::query::query_row(
                        conn,
                        "SELECT symbol_id FROM market WHERE symbol = ?1 AND venue = ?2",
                        params![symbol_owned, venue_owned],
                        |row| row.get(0),
                    )
                })
                .await
            })
            .await
            .map_err(StoreError::from)?;
        id.map(|rowid| {
            u32::try_from(rowid).map(SymbolId).map_err(|_| {
                StoreError::InvalidBar(format!("stored symbol_id {rowid} exceeds u32"))
            })
        })
        .transpose()
    }

    /// Where one tape covers. `Ok(None)` is an answered "no rows"; a
    /// store that could not be reached answers `Err`.
    pub async fn coverage(
        &self,
        permit: &StoreStorage,
        key: TapeKey,
    ) -> Result<Option<Coverage>, StoreError> {
        self.gate(permit)?;
        self.market_for(key).await?;
        let pool = Arc::clone(&self.pool);
        let bars: Vec<Bar> = self
            .read_gate
            .run(|| async move {
                pool.with_conn(move |conn| fetch_all_bars(conn, key.symbol_id))
                    .await
            })
            .await
            .map_err(StoreError::from)?;
        Ok(Coverage::from_bars(
            &order_and_validate(bars)?,
            key.timeframe,
        ))
    }

    /// Store a batch of committed bars under `policy`. Validated, scale-gated,
    /// and encoded through the shared [`plan_rows`] preamble, so the handle
    /// and the synchronous core refuse and count identically; the market row
    /// is resolved in code, because pooled connections do not all carry the
    /// `foreign_keys` pragma.
    pub async fn append(
        &self,
        permit: &StoreStorage,
        key: TapeKey,
        batch: &[Bar],
        policy: WritePolicy,
    ) -> Result<AppendOutcome, StoreError> {
        self.gate(permit)?;
        let market = self.market_for(key).await?;
        let (rows, rejected) = plan_rows(&market, key, batch)?;
        let pool = Arc::clone(&self.pool);
        let mut outcome = self
            .write_gate
            .run(|| async move {
                pool.transaction(move |tx| append_rows(tx, key.symbol_id, &rows, policy))
                    .await
            })
            .await
            .map_err(StoreError::from)?;
        outcome.rejected = rejected;
        Ok(outcome)
    }

    /// Serve a window of one tape through the same plan the synchronous
    /// core resolves: unknown symbols, unsupported spacings, open-ended
    /// reads, and torn coverage refuse identically, and folded reads narrow
    /// to the requested window with provenance attached. Bars ascend by
    /// `ts`, every adjacent pair `validate_new_bar`-clean, regardless of
    /// write order.
    pub async fn read(
        &self,
        permit: &StoreStorage,
        req: &ReadRequest,
    ) -> Result<CandleSeries, StoreError> {
        self.gate(permit)?;
        // The market row carries the tape's stored spacing; the row is
        // materialized through the shared `market_of`, so a corrupt row
        // refuses here exactly as on the core.
        let pool = Arc::clone(&self.pool);
        let symbol_owned = req.symbol.clone();
        let venue_owned = req.venue.clone();
        let market: Option<Market> = self
            .read_gate
            .run(|| async move {
                pool.with_conn(move |conn| {
                    fluent_db::query::query_row(
                        conn,
                        &format!("{SELECT_MARKET_SQL} WHERE symbol = ?1 AND venue = ?2"),
                        params![symbol_owned, venue_owned],
                        market_of,
                    )
                })
                .await
            })
            .await
            .map_err(StoreError::from)?;
        let market =
            market.ok_or_else(|| StoreError::Plan(PlanError::UnknownSymbol(req.symbol.clone())))?;
        let stored = market.timeframe;
        let symbol_id = market.symbol_id;
        // Stamp-planned coverage over the served window, like the
        // synchronous core: one pooled checkout for the extent, one for the
        // windowed stamps, never a whole-tape bar fetch. The pure window
        // derivation is shared, so both layers plan against the same runs.
        // The plan, the per-span fetches, and the double validation below
        // are unchanged.
        let pool = Arc::clone(&self.pool);
        let extent: Option<TapeExtent> = self
            .read_gate
            .run(|| async move {
                pool.with_conn(move |conn| {
                    crate::sqlite_tape::tape_extent(conn, symbol_id)
                })
                .await
            })
            .await
            .map_err(StoreError::from)?;
        let coverage: Option<Coverage> = match extent {
            None => None,
            Some(extent) => {
                let (fetch, stamp_window) = coverage_windows(stored, &extent, req)?;
                let pool = Arc::clone(&self.pool);
                let stamps: Vec<i64> = self
                    .read_gate
                    .run(|| async move {
                        pool.with_conn(move |conn| {
                            fetch_stamps(
                                conn,
                                symbol_id,
                                stamp_window.start_ms,
                                stamp_window.end_ms,
                            )
                        })
                        .await
                    })
                    .await
                    .map_err(StoreError::from)?;
                Some(coverage_from_stamps(&extent, &fetch, &stamps, stored))
            }
        };
        let plan = ReadPlan::resolve_for(Some((symbol_id, stored)), coverage, req)?;
        let served = wanted_window(req, Some(plan.covered()))?;
        let mut bars: Vec<Bar> = Vec::new();
        for span in plan.spans() {
            let (start, end) = (span.span().start_ms, span.span().end_ms);
            let pool = Arc::clone(&self.pool);
            let fetched: Vec<Bar> = self
                .read_gate
                .run(|| async move {
                    pool.with_conn(move |conn| fetch_bars(conn, symbol_id, start, end))
                        .await
                })
                .await
                .map_err(StoreError::from)?;
            bars.extend(fetched);
        }
        let bars = order_and_validate(bars)?;
        let bars = if req.timeframe == stored {
            bars
        } else {
            narrow_to_window(fold_bars(&bars, req.timeframe)?, &served, req.timeframe)
        };
        let bars = order_and_validate(bars)?;
        Ok(CandleSeries {
            key: plan.key(),
            bars,
            served,
            provenance: plan.spans().to_vec(),
        })
    }

    /// Fill the seed window for an indicator run: the bars `warmup` needs,
    /// trimmed to exactly what was asked.
    ///
    /// Composes existing functions only: [`adjust_fetch_bounds`](protopyne_types::adjust_fetch_bounds)
    /// widens the fetch by the warmup headroom, `coverage` plus the shared
    /// classifier names the verdict, only the uncovered span is repaired
    /// (verdict spans through the repair path, leading headroom the tape
    /// never held through the fill path), the window is re-read through the
    /// same [`read`](Self::read), and
    /// [`trim_indicator_output`](protopyne_types::trim_indicator_output)
    /// narrows back to the asked window. No new fetch-bounds math anywhere.
    ///
    /// The seed rule is fail-closed: a cached window shorter than `warmup`
    /// is a miss, never merged — after the fill attempt the re-read must
    /// hold at least `warmup` bars or the fill refuses with the shortfall
    /// named. A half-warmed buffer produces wrong values for its first bars
    /// and reports nothing, so it reports the miss instead. Repair here is a
    /// single attempt: a `Deferred` outcome reports rather than sleeping on
    /// the read path, and the coordinator owns retries.
    pub async fn bars_filled(
        &self,
        permit: &StoreStorage,
        symbol: &str,
        venue: &str,
        timeframe: Timeframe,
        start_ms: Option<i64>,
        limit: Option<usize>,
        warmup: usize,
        now_ms: i64,
        freshness: FreshnessPolicy,
        repair: &RepairConfig,
        provider: &dyn crate::history::HistoryProvider,
    ) -> Result<Vec<Bar>, StoreError> {
        self.gate(permit)?;
        let bounds = adjust_fetch_bounds(start_ms, limit, timeframe, warmup);
        let symbol_id = self
            .resolve(permit, symbol, venue)
            .await?
            .ok_or_else(|| StoreError::Plan(PlanError::UnknownSymbol(symbol.to_owned())))?;
        let market = self
            .market_for(TapeKey {
                symbol_id,
                timeframe,
            })
            .await?;
        let key = TapeKey {
            symbol_id,
            timeframe: market.timeframe,
        };
        let coverage = self.coverage(permit, key).await?;
        let class =
            crate::coverage::classify(coverage.clone(), now_ms, market.timeframe, freshness);
        let request = ReadRequest {
            symbol: symbol.to_owned(),
            venue: venue.to_owned(),
            timeframe,
            range: TimeRange {
                start_ms: bounds.fetch_start_ms,
                end_ms: None,
            },
            limit: bounds.fetch_limit,
            policy: FoldPolicy::ExtendToFullBuckets,
        };
        let want = wanted_window(&request, coverage.as_ref())?;
        if matches!(class, crate::coverage::GapClass::Empty) && !want.is_bounded() {
            return Err(StoreError::Plan(PlanError::UncoveredRange {
                span: want,
                want: timeframe,
            }));
        }
        let (verdict, spans) = spans_for_want(&class, coverage.as_ref(), &want);
        match verdict {
            Some(reason) if !spans.is_empty() => {
                let report = repair_spans(
                    self,
                    provider,
                    permit,
                    key,
                    want,
                    &spans,
                    reason,
                    repair.max_repair_span_ms,
                )
                .await;
                match report.status {
                    crate::repair::RepairStatus::Repaired
                    | crate::repair::RepairStatus::AlreadyComplete => {}
                    crate::repair::RepairStatus::Unavailable
                    | crate::repair::RepairStatus::Deferred => {
                        return Err(StoreError::History(report.detail));
                    }
                }
            }
            None if !spans.is_empty() => {
                fill_headroom(
                    self,
                    provider,
                    permit,
                    key,
                    &spans,
                    repair.max_repair_span_ms,
                )
                .await?;
            }
            _ => {}
        }
        let series = self.read(permit, &request).await?;
        if series.bars.len() < warmup {
            return Err(StoreError::Plan(PlanError::UncoveredRange {
                span: series.served,
                want: timeframe,
            }));
        }
        // Leading completeness: positional trimming assumes the warmup
        // headroom is fully present, so a partially served headroom would
        // shift every trimmed bar silently. When the fetch asked from a
        // bound, the first served bar must hold it — within the policy's own
        // hole tolerance, so a tape the classifier calls fresh is never
        // refused here for what the policy already accepts. A shortfall
        // refuses with the missing leading span named.
        if let (Some(fetch_start), Some(first_bar)) = (bounds.fetch_start_ms, series.bars.first()) {
            let first_ts = first_bar.ts.timestamp_millis();
            let tolerated: i64 = freshness
                .max_gap_bars
                .saturating_mul(timeframe.duration_ms().max(0) as u64)
                .try_into()
                .unwrap_or(i64::MAX);
            if first_ts.saturating_sub(fetch_start) > tolerated {
                return Err(StoreError::Plan(PlanError::UncoveredRange {
                    span: TimeRange {
                        start_ms: Some(fetch_start),
                        end_ms: Some(first_ts),
                    },
                    want: timeframe,
                }));
            }
        }
        Ok(trim_indicator_output(&series.bars, warmup, limit))
    }

    /// Resolve `key` to its market row, refusing an unregistered id with
    /// its number attached. The caller gates; this performs the read.
    async fn market_for(&self, key: TapeKey) -> Result<Market, StoreError> {
        let pool = Arc::clone(&self.pool);
        let market: Option<Market> = self
            .read_gate
            .run(|| async move {
                pool.with_conn(move |conn| {
                    fluent_db::query::query_row(
                        conn,
                        &format!("{SELECT_MARKET_SQL} WHERE symbol_id = ?1"),
                        params![i64::from(key.symbol_id.0)],
                        market_of,
                    )
                })
                .await
            })
            .await
            .map_err(StoreError::from)?;
        market.ok_or_else(|| {
            StoreError::Plan(PlanError::UnknownSymbol(format!(
                "symbol_id {}",
                key.symbol_id.0
            )))
        })
    }
}

impl std::fmt::Debug for StoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreHandle")
            .field("path", &self.path)
            .field("max_markets", &self.max_markets)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "../tests/handle.rs"]
mod tests;
