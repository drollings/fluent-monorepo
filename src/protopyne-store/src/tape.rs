//! The file-backed working store: one registry, one log per tape.
//!
//! [`MarketTape`] owns a [`MarketRegistry`](crate::registry::MarketRegistry)
//! and implements [`CandleStore`](crate::store::CandleStore) over in-memory
//! bars plus a per-tape [`AppendLog`] at `tapes/<symbol_id>-<timeframe>.csv`.
//! It mirrors the journal's storage idiom: bars ascend in memory with a
//! parallel stamp slice, reads slice windows with `partition_point`
//! (O(log n + window)), and every mutation logs before it merges, so a
//! crash replays to the same state.
//!
//! Two disciplines hold throughout. First, **ordering is established here**: the stamp slice stays
//! sorted by construction and every served read re-proves ascending order
//! through [`order_and_validate`](crate::store::order_and_validate),
//! so the guarantee lives in code, never in a file's line order. Second,
//! **referential integrity is checked in code**: `append` resolves the
//! market row itself and refuses an unregistered id with its number
//! attached.
//!
//! Storage encoding: prices as canonical `Decimal` text
//! (`normalize().to_string()` — exact round-trip, human-auditable),
//! volume as plain integer text, stamps as epoch milliseconds, adjustments
//! as JSON (empty when absent). Anything that does not decode refuses at
//! replay; a corrupt row is a loud refusal, never a defaulted bar.
//!
//! Boot replays each tape's log over its sealed artifact (when one was
//! compacted): `open` is O(tape) by allowance, while every served read
//! plans from windowed stamps — never the whole tape — through the shared
//! [`coverage`](crate::plan) helpers, so there is exactly one
//! coverage path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use protopyne_types::{adjust_fetch_bounds, trim_indicator_output, Bar, Timeframe};

use crate::append_log::{AppendLog, AppendRecord};
use crate::capability::{check_binding, check_present, StoreStorage};
use crate::catalog::Market;
use crate::commit::KeyedSnapshot as _;
use crate::coverage::{Coverage, FreshnessPolicy};
use crate::error::{PlanError, StoreError};
use crate::fold::{fold_bars, narrow_to_window, FoldPolicy};
use crate::history::HistoryProvider;
use crate::key::{SymbolId, TapeKey};
use crate::plan::{coverage_from_stamps, coverage_windows, wanted_window, ReadPlan};
use crate::range::TimeRange;
use crate::registry::MarketRegistry;
use crate::repair::{fill_headroom, repair_spans, spans_for_want, RepairConfig};
use crate::snapshot::{SealOptions, SealedSnapshot};
use crate::store::order_and_validate;
use crate::store::{AppendOutcome, CandleSeries, CandleStore, ReadRequest, WritePolicy};

/// Directory holding the per-tape logs inside a store root.
pub const TAPES_DIR: &str = "tapes";
/// Directory holding the sealed artifacts inside a store root.
pub const SEALS_DIR: &str = "seals";

/// The default bar-capacity bound: the most bars one store holds in memory.
///
/// A **cost bound** — producer memory capacity, never data trust. It caps
/// the replayed total at open and the projected total at append; it must
/// never be reused as a data-quality gate. Calibrated 2026-10-15: resident
/// ~138 bytes per bar (steady-state VmRSS delta at 1M/5M raw allocations;
/// `size_of::<Bar>()` is 128 plus the 8-byte parallel stamp; small-n
/// deltas are allocator noise),
/// so 5M bars ≈ 660MB for the rows plus process base, and a five-year
/// SPY-like 1-minute tape (~490k bars) replays at ~68MB. The bound holds
/// 10x that working set with headroom while refusing absurd growth before
/// the box pages. Method: raw `Vec<Bar>` + stamp allocation at
/// 1k/100k/1M/5M with VmRSS deltas (`tests/tape_capacity.rs`,
/// `--nocapture` prints the table).
pub const DEFAULT_MAX_BARS: usize = 5_000_000;

/// How a file-backed store bounds its in-memory footprint. Both fields are
/// **cost bounds** in the seal-options sense: they cap what the producer
/// holds, never what the data means.
///
/// NOT-A-BYTE-BUDGET-CACHE (M8.3): this is a fixed-count bar bound with
/// refuse-on-overflow, not the weighted byte-budget LRU in
/// `common_core::cache`. Do not swap policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, bon::Builder)]
#[builder(start_fn = new)]
/// How a file-backed store bounds its in-memory footprint. Both fields are
/// **cost bounds** in the seal-options sense: they cap what the producer
/// holds, never what the data means.
/// 
/// NOT-A-BYTE-BUDGET-CACHE (M8.3): this is a fixed-count bar bound with
/// refuse-on-overflow, not the weighted byte-budget LRU in
/// `common_core::cache`. Do not swap policies.
pub struct TapeOptions {
    /// Most bars the whole store holds across every tape. Checked at open
    /// against the replayed total and at append against the projected
    /// total; exceeding it refuses with the tape, the bound, and the
    /// observed count named, writing nothing.
    #[builder(default = DEFAULT_MAX_BARS)]
    pub max_bars: usize,
}

/// What one log row records: an insert of a new stamp or a replace of a
/// stored one. The operation is what makes replay converge: inserts keep
/// the first row they meet, replaces overwrite with the row they carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What one log row records: an insert of a new stamp or a replace of a
/// stored one. The operation is what makes replay converge: inserts keep
/// the first row they meet, replaces overwrite with the row they carry.
enum TapeOp {
    Insert,
    Replace,
}

impl TapeOp {
    fn as_str(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Replace => "replace",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "insert" => Some(Self::Insert),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }
}

/// One logged bar: the operation plus the bar's fields in storage encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One logged bar: the operation plus the bar's fields in storage encoding.
struct TapeRecord {
    op: TapeOp,
    ts_ms: i64,
    open: String,
    high: String,
    low: String,
    close: String,
    volume: u64,
    adj: String,
}

impl TapeRecord {
    fn of(op: TapeOp, bar: &Bar) -> Result<Self, StoreError> {
        Ok(Self {
            op,
            ts_ms: bar.ts.timestamp_millis(),
            open: canonical(bar.o),
            high: canonical(bar.h),
            low: canonical(bar.l),
            close: canonical(bar.c),
            volume: bar.volume,
            adj: bar
                .adj
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| StoreError::InvalidBar(format!("adjustment does not serialize: {e}")))?
                .unwrap_or_default(),
        })
    }

    fn to_bar(&self) -> Result<Bar, String> {
        use std::str::FromStr as _;
        let ts_ms = self.ts_ms;
        let ts = chrono::DateTime::from_timestamp_millis(ts_ms)
            .ok_or_else(|| format!("stamp {ts_ms} out of range"))?;
        let price = |text: &str| {
            rust_decimal::Decimal::from_str(text)
                .map_err(|_| format!("price {text:?} is not a decimal"))
        };
        let adj = if self.adj.is_empty() {
            None
        } else {
            Some(
                serde_json::from_str(&self.adj)
                    .map_err(|_| format!("adjustment {:?} is not valid JSON", self.adj))?,
            )
        };
        Ok(Bar::new()
            .ts(ts)
            .o(price(&self.open)?)
            .h(price(&self.high)?)
            .l(price(&self.low)?)
            .c(price(&self.close)?)
            .volume(self.volume)
            .maybe_adj(adj)
            .build())
    }
}

/// Canonical price text: what the check reads is what storage keeps.
fn canonical(value: rust_decimal::Decimal) -> String {
    value.normalize().to_string()
}

impl AppendRecord for TapeRecord {
    fn domain() -> &'static str {
        "protopyne_tape"
    }

    fn version() -> u32 {
        1
    }

    fn header() -> Vec<&'static str> {
        vec![
            "op", "ts_ms", "open", "high", "low", "close", "volume", "adj",
        ]
    }

    fn encode(&self) -> Vec<String> {
        vec![
            self.op.as_str().to_owned(),
            self.ts_ms.to_string(),
            self.open.clone(),
            self.high.clone(),
            self.low.clone(),
            self.close.clone(),
            self.volume.to_string(),
            self.adj.clone(),
        ]
    }

    fn decode(fields: &[&str]) -> Result<Self, String> {
        let [op, ts_ms, open, high, low, close, volume, adj] = fields else {
            return Err(format!("expected 8 fields, saw {}", fields.len()));
        };
        Ok(Self {
            op: TapeOp::parse(op).ok_or_else(|| format!("unknown op {op:?}"))?,
            ts_ms: ts_ms
                .parse()
                .map_err(|_| format!("stamp {ts_ms:?} is not an integer"))?,
            open: (*open).to_owned(),
            high: (*high).to_owned(),
            low: (*low).to_owned(),
            close: (*close).to_owned(),
            volume: volume
                .parse()
                .map_err(|_| format!("volume {volume:?} is not an integer"))?,
            adj: (*adj).to_owned(),
        })
    }
}

/// One tape's state: ascending bars, their parallel stamps, and the log
/// that replays to them (`None` for ephemeral in-memory tapes).
struct TapeData {
    bars: Vec<Bar>,
    stamps: Vec<i64>,
    log: Option<AppendLog<TapeRecord>>,
}

impl TapeData {
    fn empty() -> Self {
        Self {
            bars: Vec::new(),
            stamps: Vec::new(),
            log: None,
        }
    }

    /// Insert `bar` at its sorted position. The caller proves the stamp is
    /// absent; a present stamp is a caller bug, refused rather than doubled.
    fn insert_sorted(&mut self, bar: Bar) {
        let ts = bar.ts.timestamp_millis();
        let idx = self.stamps.partition_point(|stamp| *stamp < ts);
        assert!(
            self.stamps.get(idx) != Some(&ts),
            "insert_sorted called on a present stamp"
        );
        self.stamps.insert(idx, ts);
        self.bars.insert(idx, bar);
    }
}

/// The file-backed working store: a registry plus its tapes.
pub struct MarketTape {
    registry: MarketRegistry,
    root: PathBuf,
    tapes: Mutex<HashMap<TapeKey, TapeData>>,
    permit: StoreStorage,
    max_bars: usize,
}

impl MarketTape {
    /// Open (or create) the store at `root`: the registry, every tape log
    /// replayed over its sealed artifact, and the store directories.
    /// Boot replay is O(tape) by allowance; served reads stay windowed.
    /// The replayed total is checked against the default capacity bound;
    /// exceeding it refuses with nothing partially loaded.
    pub fn open(root: &Path) -> Result<Self, StoreError> {
        Self::open_with_options(root, TapeOptions::new().build())
    }

    /// Open (or create) the store at `root` under `options`: same replay
    /// as [`open`](Self::open), with the capacity bound the caller chose.
    /// Exceeding `max_bars` refuses with the store, the bound, and the
    /// replayed total named; the caller receives no handle, so no partial
    /// state can be served from.
    pub fn open_with_options(root: &Path, options: TapeOptions) -> Result<Self, StoreError> {
        let registry = MarketRegistry::open(root)?;
        let permit = StoreStorage::new(root.to_path_buf());
        let markets = registry.markets_unchecked();
        // Refuse foreign log files before replaying anything: a file that
        // names no registered tape can never be a tape log.
        let known: HashMap<SymbolId, Timeframe> = markets
            .iter()
            .map(|market| (market.symbol_id, market.timeframe))
            .collect();
        let tapes_dir = root.join(TAPES_DIR);
        if tapes_dir.is_dir() {
            let mut names: Vec<String> = Vec::new();
            for entry in std::fs::read_dir(&tapes_dir).map_err(common_core::error::IoError::from)? {
                let entry = entry.map_err(common_core::error::IoError::from)?;
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
            names.sort();
            for name in &names {
                check_log_name(name, &known)?;
            }
        }
        let mut tapes = HashMap::new();
        for market in &markets {
            let key = TapeKey {
                symbol_id: market.symbol_id,
                timeframe: market.timeframe,
            };
            tapes.insert(key, Self::load_tape(root, market)?);
        }
        let replayed: usize = tapes.values().map(|tape| tape.bars.len()).sum();
        if replayed > options.max_bars {
            return Err(StoreError::Capacity(format!(
                "store {} replayed {replayed} bars over max {}: refusing to open",
                root.display(),
                options.max_bars
            )));
        }
        Ok(Self {
            registry,
            root: root.to_path_buf(),
            tapes: Mutex::new(tapes),
            permit,
            max_bars: options.max_bars,
        })
    }

    /// Open an ephemeral store: registry and tapes live in memory, nothing
    /// persists past the handle. For tests and scratch work.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::open_in_memory_with_options(TapeOptions::new().build())
    }

    /// Open an ephemeral store under `options`: same memory-only shape as
    /// [`open_in_memory`](Self::open_in_memory), with the capacity bound
    /// the caller chose.
    pub fn open_in_memory_with_options(options: TapeOptions) -> Result<Self, StoreError> {
        Ok(Self {
            registry: MarketRegistry::open_in_memory()?,
            root: PathBuf::from(":memory:"),
            tapes: Mutex::new(HashMap::new()),
            permit: StoreStorage::new(PathBuf::from(":memory:")),
            max_bars: options.max_bars,
        })
    }

    /// The registry behind this tape.
    pub fn registry(&self) -> &MarketRegistry {
        &self.registry
    }

    /// The store root this tape was opened at: the binding identity its
    /// permit carries. Setup-time callers assert a presented permit against
    /// this before serving anything.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The permit this tape gates on. Same-crate callers (repair) run the
    /// binding half of the gate against it without minting a second token.
    pub(crate) fn permit(&self) -> &StoreStorage {
        &self.permit
    }

    /// Serve `req` only for the store this permit was minted for: the
    /// binding half of the gate runs first — a foreign permit refuses on
    /// its face, without consulting the task-local set — then presence,
    /// then the read. Per-request callers that carry a permit use this
    /// instead of [`read`](CandleStore::read), so a permit minted for one
    /// store can never operate another.
    pub fn read_scoped(
        &self,
        permit: &StoreStorage,
        req: &ReadRequest,
    ) -> Result<CandleSeries, StoreError> {
        check_binding(permit, &self.root)?;
        self.read(req)
    }

    /// Fill the seed window for an indicator run: the bars `warmup` needs,
    /// trimmed to exactly what was asked.
    ///
    /// Composes existing functions only: [`adjust_fetch_bounds`](protopyne_types::adjust_fetch_bounds)
    /// widens the fetch by the warmup headroom, `coverage` plus the shared
    /// classifier names the verdict, only the uncovered span is repaired
    /// (verdict spans through the repair path, leading headroom the tape
    /// never held through the fill path), the window is re-read through the
    /// same [`read`](CandleStore::read), and
    /// [`trim_indicator_output`](protopyne_types::trim_indicator_output)
    /// narrows back to the asked window. No new fetch-bounds math anywhere.
    ///
    /// The seed rule is fail-closed: a cached window shorter than `warmup`
    /// is a miss, never merged — after the fill attempt the re-read must
    /// hold at least `warmup` bars or the fill refuses with the shortfall
    /// named. A half-warmed buffer produces wrong values for its first bars
    /// and reports nothing, so it reports the miss instead. Repair here is a
    /// single attempt: a `Deferred` outcome reports rather than sleeping on
    /// the read path, and the coordinator owns retries. The permit binds
    /// the whole fill to this store root, checked before anything fetches.
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
        provider: &dyn HistoryProvider,
    ) -> Result<Vec<Bar>, StoreError> {
        check_binding(permit, &self.root)?;
        self.gate()?;
        let bounds = adjust_fetch_bounds(start_ms, limit, timeframe, warmup);
        let symbol_id = self
            .registry
            .tape_of(symbol, venue)
            .map(|(symbol_id, _)| symbol_id)
            .ok_or_else(|| StoreError::Plan(PlanError::UnknownSymbol(symbol.to_owned())))?;
        let market = self.market_for(TapeKey {
            symbol_id,
            timeframe,
        })?;
        let key = TapeKey {
            symbol_id,
            timeframe: market.timeframe,
        };
        let coverage = self.coverage(key)?;
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
                fill_headroom(self, provider, key, &spans, repair.max_repair_span_ms).await?;
            }
            _ => {}
        }
        let series = self.read(&request)?;
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

    /// Register a new market. The registry gates and refuses; this is the
    /// setup-time spelling of that call.
    pub fn register(
        &self,
        symbol: &str,
        venue: &str,
        asset_class: crate::catalog::AssetClass,
        price_scale: crate::catalog::PriceScale,
        time_zone: &str,
        timeframe: Timeframe,
    ) -> Result<SymbolId, StoreError> {
        let symbol_id = self.registry.register(
            symbol,
            venue,
            asset_class,
            price_scale,
            time_zone,
            timeframe,
        )?;
        common_core::sync::lock(&self.tapes).insert(
            TapeKey {
                symbol_id,
                timeframe,
            },
            TapeData::empty(),
        );
        Ok(symbol_id)
    }

    /// Seal the whole tape and truncate its log: the explicit compaction
    /// operator. The seal (artifact plus manifest row) commits first and the
    /// log truncates only after, in that order — a crash between the two
    /// replays the seal plus the untruncated log to the same bars on the
    /// next open. No automatic trigger ever calls this.
    pub fn compact(
        &self,
        key: TapeKey,
        options: SealOptions,
        now_ms: i64,
    ) -> Result<SealedSnapshot, StoreError> {
        self.gate()?;
        let bars = self.bars_of(key)?;
        let sealed = crate::snapshot::seal(
            &self.registry,
            key,
            &bars,
            &self.root.join(SEALS_DIR),
            options,
            now_ms,
        )?;
        let mut tapes = common_core::sync::lock(&self.tapes);
        let tape = tapes.get_mut(&key).ok_or_else(|| {
            StoreError::Plan(PlanError::UnknownSymbol(format!(
                "symbol_id {}",
                key.symbol_id.0
            )))
        })?;
        if let Some(log) = tape.log.as_mut() {
            log.truncate_all()?;
        }
        Ok(sealed)
    }

    /// The presence half of the gate, over the tape's own permit: proves
    /// the calling task was granted store access.
    fn gate(&self) -> Result<(), StoreError> {
        check_present(&self.permit)
    }

    /// Resolve `key` to its market row, refusing an unregistered id with
    /// its number attached — the code-level referential check.
    fn market_for(&self, key: TapeKey) -> Result<Market, StoreError> {
        self.registry.market_by_id(key.symbol_id).ok_or_else(|| {
            StoreError::Plan(PlanError::UnknownSymbol(format!(
                "symbol_id {}",
                key.symbol_id.0
            )))
        })
    }

    /// A clone of one tape's bars, for the seal path. The clone keeps the
    /// lock hold short: the seal's writes happen outside it.
    fn bars_of(&self, key: TapeKey) -> Result<Vec<Bar>, StoreError> {
        let tapes = common_core::sync::lock(&self.tapes);
        let tape = tapes.get(&key).ok_or_else(|| {
            StoreError::Plan(PlanError::UnknownSymbol(format!(
                "symbol_id {}",
                key.symbol_id.0
            )))
        })?;
        Ok(tape.bars.clone())
    }

    /// Load one tape at boot: sealed bars (when a compaction committed a
    /// seal) with the log tail replayed over them. Inserts keep the first
    /// row they meet, replaces overwrite with the row they carry — the same
    /// rule the live append path applies, so boot converges with runtime.
    fn load_tape(root: &Path, market: &Market) -> Result<TapeData, StoreError> {
        let key = TapeKey {
            symbol_id: market.symbol_id,
            timeframe: market.timeframe,
        };
        let sealed = load_sealed(root, key)?;
        let path = log_path(root, market.symbol_id, market.timeframe);
        let (bars, log) = if path.exists() {
            let log = AppendLog::<TapeRecord>::open(&path)?;
            let mut rows = Vec::new();
            for record in log.read_all()? {
                let bar = record.to_bar().map_err(StoreError::InvalidBar)?;
                rows.push((record.op, bar));
            }
            (replay_log_records(sealed, &rows), Some(log))
        } else {
            (sealed, None)
        };
        let bars = order_and_validate(bars)?;
        Ok(TapeData {
            stamps: bars.iter().map(|bar| bar.ts.timestamp_millis()).collect(),
            bars,
            log,
        })
    }
}

/// Fold decoded log rows over sealed bars, by stamp index: inserts keep the
/// first row they meet, replaces overwrite with the row they carry (pushing
/// when the stamp is novel) — the same rule the live append path applies, so
/// boot converges with runtime. One index build over the sealed bars plus one
/// hash lookup per row: each record resolves its bar without scanning the
/// tape, so replay is linear in the log instead of quadratic in the tape.
/// Replay semantics are unchanged: same rows in, same bars out, in whatever
/// order the log carried them (the caller sorts and validates after).
fn replay_log_records(sealed: Vec<Bar>, rows: &[(TapeOp, Bar)]) -> Vec<Bar> {
    let mut bars = sealed;
    let mut index: HashMap<i64, usize> = bars
        .iter()
        .enumerate()
        .map(|(at, bar)| (bar.ts.timestamp_millis(), at))
        .collect();
    for (op, bar) in rows {
        let stamp = bar.ts.timestamp_millis();
        match op {
            TapeOp::Insert => {
                if let std::collections::hash_map::Entry::Vacant(slot) = index.entry(stamp) {
                    slot.insert(bars.len());
                    bars.push(bar.clone());
                }
            }
            TapeOp::Replace => {
                if let Some(&at) = index.get(&stamp) {
                    bars[at] = bar.clone();
                } else {
                    index.insert(stamp, bars.len());
                    bars.push(bar.clone());
                }
            }
        }
    }
    bars
}

/// Load the latest sealed artifact for `key`, or nothing when no compaction
/// has committed one. A stale directory without a manifest reads empty —
/// seals only exist where compaction wrote them.
fn load_sealed(root: &Path, key: TapeKey) -> Result<Vec<Bar>, StoreError> {
    let seals = root.join(SEALS_DIR);
    let manifest_path = crate::manifest::SealManifest::path_for(&seals);
    if !manifest_path.is_file() {
        return Ok(Vec::new());
    }
    let manifest = crate::manifest::SealManifest::load(&seals)?;
    let Some(record) = manifest
        .records()
        .iter()
        .find(|record| record.symbol_id == key.symbol_id && record.timeframe == key.timeframe)
    else {
        return Ok(Vec::new());
    };
    crate::snapshot::read_snapshot_ungated(key, Path::new(&record.path))
}

/// The log file for one tape: deterministic, so reopening finds it.
fn log_path(root: &Path, symbol_id: SymbolId, timeframe: Timeframe) -> PathBuf {
    root.join(TAPES_DIR)
        .join(format!("{:010}-{}.csv", symbol_id.0, timeframe.code()))
}

/// Refuse a tape-log file that names no registered market — or no parseable
/// tape at all — instead of silently ignoring it. Runs at open, before any
/// replay, so a foreign file can never hide behind a boot.
fn check_log_name(
    file_name: &str,
    markets: &HashMap<SymbolId, Timeframe>,
) -> Result<(), StoreError> {
    let Some(stem) = file_name.strip_suffix(".csv") else {
        return Ok(());
    };
    let Some((id_text, tf_text)) = stem.rsplit_once('-') else {
        return Err(StoreError::InvalidBar(format!(
            "tape log {file_name:?} names no tape"
        )));
    };
    use std::str::FromStr as _;
    let id: u32 = id_text.parse().map_err(|_| {
        StoreError::InvalidBar(format!("tape log {file_name:?} names no symbol id"))
    })?;
    let timeframe = Timeframe::from_str(tf_text).map_err(|_| {
        StoreError::InvalidBar(format!("tape log {file_name:?} names no timeframe"))
    })?;
    if markets.get(&SymbolId(id)) != Some(&timeframe) {
        return Err(StoreError::InvalidBar(format!(
            "tape log {file_name:?} names an unregistered tape"
        )));
    }
    Ok(())
}

/// Whole-tape coverage from validated bars: the helper the validated
/// coverage path — and only that path — names. Audits, repair, and health
/// may call this through `coverage`; the served read must not (the budget
/// suite pins that rule by name).
pub(crate) fn whole_tape_coverage(bars: &[Bar], tf: Timeframe) -> Option<Coverage> {
    Coverage::from_bars(bars, tf)
}

impl CandleStore for MarketTape {
    fn markets(&self) -> Result<Vec<Market>, StoreError> {
        self.gate()?;
        self.registry.markets()
    }

    fn resolve(&self, symbol: &str, venue: &str) -> Result<Option<SymbolId>, StoreError> {
        self.registry.resolve(symbol, venue)
    }

    fn coverage(&self, key: TapeKey) -> Result<Option<Coverage>, StoreError> {
        self.gate()?;
        let market = self.market_for(key)?;
        let tapes = common_core::sync::lock(&self.tapes);
        let tape = tapes.get(&key).ok_or_else(|| {
            StoreError::Plan(PlanError::UnknownSymbol(format!(
                "symbol_id {}",
                key.symbol_id.0
            )))
        })?;
        Ok(whole_tape_coverage(&tape.bars, market.timeframe))
    }

    fn append(
        &self,
        key: TapeKey,
        batch: &[Bar],
        policy: WritePolicy,
    ) -> Result<AppendOutcome, StoreError> {
        self.gate()?;
        let market = self.market_for(key)?;
        // The request-independent preamble: the batch's spacing must match
        // the tape's, every bar validates before anything lands (a malformed
        // batch refuses without storing its good prefix), and scale-gating
        // counts per row before any write is planned.
        if key.timeframe != market.timeframe {
            return Err(StoreError::Plan(PlanError::UnsupportedTimeframe {
                symbol: market.symbol.clone(),
                timeframe: key.timeframe,
            }));
        }
        for bar in batch {
            bar.validate()
                .map_err(|e| StoreError::InvalidBar(e.to_string()))?;
        }
        let mut outcome = AppendOutcome::default();
        let mut storable = Vec::with_capacity(batch.len());
        for bar in batch {
            match crate::catalog::check_scale(&market, bar) {
                Ok(()) => storable.push(bar),
                Err(StoreError::PriceScale { .. }) => outcome.rejected += 1,
                Err(other) => return Err(other),
            }
        }
        let mut tapes = common_core::sync::lock(&self.tapes);
        // Capacity reads the held total before the mutable borrow below:
        // the gate caps held plus genuinely new stamps.
        let held: usize = tapes.values().map(|held| held.bars.len()).sum();
        let tape = tapes.get_mut(&key).ok_or_else(|| {
            StoreError::Plan(PlanError::UnknownSymbol(format!(
                "symbol_id {}",
                key.symbol_id.0
            )))
        })?;
        // Compute the merged state first (reads only): inserts land sorted,
        // restatements fold through the one restatement rule, and every
        // applied row has its log record ready — so the log write below
        // either commits the whole batch or refuses it with memory
        // untouched. A stamp touched twice in one batch resolves in batch
        // order through the batch's own computed value.
        let mut computed: HashMap<i64, Bar> = HashMap::new();
        let mut logged = Vec::with_capacity(storable.len());
        for bar in storable {
            let ts = bar.ts.timestamp_millis();
            if let Some(final_bar) = computed.get(&ts) {
                outcome.conflicts += 1;
                if let WritePolicy::UpsertBySeq { .. } = policy {
                    let merged = final_bar.merge(bar).map_err(StoreError::from)?;
                    logged.push(TapeRecord::of(TapeOp::Replace, &merged)?);
                    computed.insert(ts, merged);
                }
            } else if let Ok(idx) = tape.stamps.binary_search(&ts) {
                outcome.conflicts += 1;
                if let WritePolicy::UpsertBySeq { .. } = policy {
                    let merged = tape.bars[idx].merge(bar).map_err(StoreError::from)?;
                    logged.push(TapeRecord::of(TapeOp::Replace, &merged)?);
                    computed.insert(ts, merged);
                }
            } else {
                outcome.inserted += 1;
                let op = match policy {
                    WritePolicy::InsertIfAbsent => TapeOp::Insert,
                    WritePolicy::UpsertBySeq { .. } => TapeOp::Replace,
                };
                logged.push(TapeRecord::of(op, bar)?);
                computed.insert(ts, (*bar).clone());
            }
        }
        // Durability before memory: the log batch commits (or refuses, with
        // memory untouched), then the computed mutations apply. Capacity
        // gates before either: the projected total (held plus genuinely
        // new stamps — restatement merges replace in place) must fit the
        // bound, or nothing is logged and memory stays untouched.
        let fresh = computed
            .keys()
            .filter(|ts| tape.stamps.binary_search(ts).is_err())
            .count();
        let projected = held + fresh;
        if projected > self.max_bars {
            return Err(StoreError::Capacity(format!(
                "tape {} {} holds {held} bars, batch adds {fresh} new: projected {projected} exceeds max {}",
                market.symbol,
                key.timeframe.code(),
                self.max_bars
            )));
        }
        if let Some(log) = tape.log.as_mut() {
            log.append_batch(&logged)?;
        } else if self.root.as_os_str() != ":memory:" {
            let path = log_path(&self.root, key.symbol_id, key.timeframe);
            let mut log = AppendLog::open(&path)?;
            log.append_batch(&logged)?;
            tape.log = Some(log);
        }
        for (ts, bar) in computed {
            match tape.stamps.binary_search(&ts) {
                Ok(idx) => tape.bars[idx] = bar,
                Err(_) => tape.insert_sorted(bar),
            }
        }
        Ok(outcome)
    }

    fn repair_one(&self, key: TapeKey, ts_ms: i64, bar: &Bar) -> Result<(), StoreError> {
        self.gate()?;
        let market = self.market_for(key)?;
        bar.validate()
            .map_err(|e| StoreError::InvalidBar(e.to_string()))?;
        if bar.ts.timestamp_millis() != ts_ms {
            return Err(StoreError::InvalidBar(format!(
                "repair names ts_ms {ts_ms} but the bar stamps {}",
                bar.ts.timestamp_millis()
            )));
        }
        // Corrections respect the scale exactly as appends do: an
        // unrepresentable replacement refuses loudly here, where there is
        // no batch outcome to count it into.
        crate::catalog::check_scale(&market, bar)?;
        let record = TapeRecord::of(TapeOp::Replace, bar)?;
        let mut tapes = common_core::sync::lock(&self.tapes);
        let tape = tapes.get_mut(&key).ok_or_else(|| {
            StoreError::Plan(PlanError::UnknownSymbol(format!(
                "symbol_id {}",
                key.symbol_id.0
            )))
        })?;
        let idx = tape.stamps.binary_search(&ts_ms).map_err(|_| {
            StoreError::InvalidBar(format!("no committed row at ts_ms {ts_ms} for repair"))
        })?;
        // Durability before memory, as in `append`: the replace record
        // commits before the committed row is overwritten.
        if let Some(log) = tape.log.as_mut() {
            log.append_batch(std::slice::from_ref(&record))?;
        } else if self.root.as_os_str() != ":memory:" {
            let path = log_path(&self.root, key.symbol_id, key.timeframe);
            let mut log = AppendLog::open(&path)?;
            log.append_batch(std::slice::from_ref(&record))?;
            tape.log = Some(log);
        }
        let old = std::mem::replace(&mut tape.bars[idx], (*bar).clone());
        tracing::warn!(
            symbol = %market.symbol,
            ts_ms,
            old_close = %old.c,
            new_close = %bar.c,
            old_volume = old.volume,
            new_volume = bar.volume,
            "repair_one overwrote a committed row"
        );
        Ok(())
    }

    fn read(&self, req: &ReadRequest) -> Result<CandleSeries, StoreError> {
        self.gate()?;
        // The tape lookup is pure (in-memory index, no I/O): unknown
        // refuses before anything is touched.
        let (symbol_id, stored) = self
            .registry
            .tape_of(&req.symbol, &req.venue)
            .ok_or_else(|| StoreError::Plan(PlanError::UnknownSymbol(req.symbol.clone())))?;
        let key = TapeKey {
            symbol_id,
            timeframe: stored,
        };
        let tapes = common_core::sync::lock(&self.tapes);
        let tape = tapes.get(&key).ok_or_else(|| {
            StoreError::Plan(PlanError::UnknownSymbol(format!(
                "symbol_id {}",
                symbol_id.0
            )))
        })?;
        // Stamp-planned coverage over the served window — extent plus
        // windowed runs — never a whole-tape bar walk.
        let extent = match (tape.stamps.first(), tape.stamps.last()) {
            (Some(first), Some(last)) => Some(crate::plan::TapeExtent {
                first_ms: *first,
                last_ms: *last,
            }),
            _ => None,
        };
        let coverage = extent
            .as_ref()
            .map(|ext| {
                let (fetch, stamp_window) = coverage_windows(stored, ext, req)?;
                let stamps = stamp_slice(&tape.stamps, &stamp_window);
                Ok::<_, StoreError>(coverage_from_stamps(ext, &fetch, stamps, stored))
            })
            .transpose()?;
        let plan = ReadPlan::resolve_for(Some((symbol_id, stored)), coverage, req)?;
        let served = wanted_window(req, Some(plan.covered()))?;
        let mut bars = Vec::new();
        for span in plan.spans() {
            bars.extend(span_slice(&tape.bars, &tape.stamps, &span.span()));
        }
        // Ascending before the fold: buckets assume order, and the log
        // order never promised any.
        let bars = order_and_validate(bars)?;
        let bars = if req.timeframe == stored {
            bars
        } else {
            narrow_to_window(fold_bars(&bars, req.timeframe)?, &served, req.timeframe)
        };
        // And ascending after: the boundary proves what it serves,
        // including what the fold produced.
        let bars = order_and_validate(bars)?;
        Ok(CandleSeries {
            key: plan.key(),
            bars,
            served,
            provenance: plan.spans().to_vec(),
        })
    }
}

/// The stamps inside a half-open window, by binary search: O(log n) bounds,
/// no scan. Both bounds are always present on planner windows.
fn stamp_slice<'a>(stamps: &'a [i64], window: &TimeRange) -> &'a [i64] {
    let start = window.start_ms.unwrap_or(i64::MIN);
    let end = window.end_ms.unwrap_or(i64::MAX);
    let lo = stamps.partition_point(|stamp| *stamp < start);
    let hi = stamps.partition_point(|stamp| *stamp < end);
    &stamps[lo..hi]
}

/// The bars whose stamps fall in `span`, by binary search over the parallel
/// stamp slice: O(log n + window), never O(tape).
fn span_slice(bars: &[Bar], stamps: &[i64], span: &TimeRange) -> Vec<Bar> {
    let start = span.start_ms.unwrap_or(i64::MIN);
    let end = span.end_ms.unwrap_or(i64::MAX);
    let lo = stamps.partition_point(|stamp| *stamp < start);
    let hi = stamps.partition_point(|stamp| *stamp < end);
    bars[lo..hi].to_vec()
}

impl std::fmt::Debug for MarketTape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarketTape")
            .field("registry", &self.registry)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "../tests/common/market_tape_setup.rs"]
pub(crate) mod test_setup;

#[cfg(test)]
#[path = "../tests/common/tape_conformance.rs"]
pub(crate) mod test_conformance;

#[cfg(test)]
#[path = "../tests/common/oracle.rs"]
pub(crate) mod test_oracle;

#[cfg(test)]
#[path = "../tests/market_tape_conformance.rs"]
mod tests_conformance;

#[cfg(test)]
#[path = "../tests/tape_budget.rs"]
mod tests_budget;

#[cfg(test)]
#[path = "../tests/tape_compact.rs"]
mod tests_compact;

#[cfg(test)]
#[path = "../tests/tape_capacity.rs"]
mod tests_capacity;

#[cfg(test)]
#[path = "../tests/read_windowed_oracle.rs"]
mod tests_read_oracle;

#[cfg(test)]
#[path = "../tests/append_idempotent.rs"]
mod tests_append;

#[cfg(test)]
#[path = "../tests/rejected_is_counted.rs"]
mod tests_rejected;

#[cfg(test)]
#[path = "../tests/exactness_versus_f64.rs"]
mod tests_exactness;

#[cfg(test)]
#[path = "../tests/price_scale_calibration.rs"]
mod tests_price_scale;