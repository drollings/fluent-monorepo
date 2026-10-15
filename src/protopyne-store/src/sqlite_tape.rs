//! The SQLite working store: one connection, one catalog, one tape.
//!
//! [`SqliteTape`] owns a [`MarketCatalog`] and implements
//! [`CandleStore`](crate::store::CandleStore) against its connection,
//! mirroring the journal's storage idiom: `open_wal` open, pragmas applied
//! outside the migration transaction, `Mutex<Connection>` behind
//! `common_core :: sync :: lock` (never held across an await — every method
//! here is synchronous), and multi-statement writes through
//! `fluent_db :: query :: transaction`.
//!
//! Two disciplines hold throughout. First, **ordering is established here**,
//! never assumed from the database: reads materialize rows and sort them in
//! Rust before validating, so the ascending-`ts` guarantee lives in code,
//! not in a SQL string. Second, **referential integrity is checked in
//! code**: the `candle` rows carry a `REFERENCES market` clause, but pool
//! connections (the async handle's) do not all carry the `foreign_keys`
//! pragma, so `append` resolves the market row itself and refuses an
//! unregistered id with its number attached.
//!
//! Storage encoding: prices as canonical `Decimal` text
//! (`normalize().to_string()` — exact round-trip, human-auditable),
//! volume as `INTEGER`, stamps as epoch milliseconds, adjustments as JSON
//! (`NULL` when absent). Anything that does not decode refuses at
//! materialization; a corrupt row is a database failure, never a
//! defaulted bar.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

use chrono::DateTime;
use protopyne_types::{Bar, Timeframe, validate_new_bar};
use rusqlite::{Connection, params};
use rust_decimal::Decimal;

use crate::capability::check_present;
use crate::catalog::{Market, MarketCatalog};
use crate::coverage::Coverage;
use crate::error::{PlanError, StoreError};
use crate::fold::{fold_bars, narrow_to_window};
use crate::key::{SymbolId, TapeKey};
use crate::plan::{ReadPlan, wanted_window};
use crate::range::TimeRange;
use crate::store::{AppendOutcome, CandleSeries, CandleStore, ReadRequest, WritePolicy};

/// The full candle row select. Column order is the contract
/// [`bar_of`] reads; every reader shares this string so it cannot drift.
const SELECT_CANDLE_SQL: &str =
    "SELECT symbol_id, ts_ms, open, high, low, close, volume, adj FROM candle";

/// Plain insert: a colliding row errors, and the caller classifies the
/// collision through `is_unique_violation` — the constraint is the
/// arbiter, so concurrent writers agree without a lock between them.
const INSERT_CANDLE_SQL: &str =
    "INSERT INTO candle(symbol_id, ts_ms, open, high, low, close, volume, adj)
 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

/// Restating insert: a colliding row is overwritten with the batch's row.
const UPSERT_CANDLE_SQL: &str =
    "INSERT INTO candle(symbol_id, ts_ms, open, high, low, close, volume, adj)
 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
 ON CONFLICT(symbol_id, ts_ms) DO UPDATE SET
 open = excluded.open, high = excluded.high, low = excluded.low,
 close = excluded.close, volume = excluded.volume, adj = excluded.adj";

/// Explicit correction: overwrite one committed row. The caller names the
/// row; this statement writes exactly it.
const UPDATE_CANDLE_SQL: &str = "UPDATE candle
 SET open = ?1, high = ?2, low = ?3, close = ?4, volume = ?5, adj = ?6
 WHERE symbol_id = ?7 AND ts_ms = ?8";

/// Refuse a bar the market cannot represent: every price must fit the
/// market's scale. The first offender names the refusal, with the symbol,
/// the price as carried, and the scale it failed — never rounded, never
/// truncated, and the reason rides the error, not a log line the caller
/// never sees.
pub(crate) fn check_scale(market: &Market, bar: &Bar) -> Result<(), StoreError> {
    for price in [bar.o, bar.h, bar.l, bar.c] {
        if !market.price_scale.fits(price) {
            return Err(StoreError::PriceScale {
                symbol: market.symbol.clone(),
                value: price,
                scale: market.price_scale.digits,
            });
        }
    }
    Ok(())
}

/// Validate, scale-gate, and encode one batch: the request-independent
/// preamble both append layers share, so the synchronous core and the async
/// handle cannot disagree about what lands. Returns the storable rows plus
/// the count of rows the price-scale policy refused — refused rows are
/// counted, never stored, never rounded, never truncated.
///
/// The batch's spacing must match the tape's: two grids on one `(symbol_id,
/// ts_ms)` primary key would fold into each other's buckets, so a batch keyed
/// at any other spacing refuses before any bar is examined.
pub(crate) fn plan_rows(
    market: &Market,
    key: TapeKey,
    batch: &[Bar],
) -> Result<(Vec<EncodedRow>, usize), StoreError> {
    if key.timeframe != market.timeframe {
        return Err(StoreError::Plan(PlanError::UnsupportedTimeframe {
            symbol: market.symbol.clone(),
            timeframe: key.timeframe,
        }));
    }
    // Validate before the write, so a malformed batch refuses without
    // storing its good prefix — the transaction below would roll it back
    // anyway, but refusing early names the bar, not the statement.
    for bar in batch {
        bar.validate()
            .map_err(|e| StoreError::InvalidBar(e.to_string()))?;
    }
    // Scale-gate before encoding, so a batch that cannot be stored refuses
    // per-row before the transaction opens rather than inside it.
    let mut rejected = 0_usize;
    let mut rows = Vec::with_capacity(batch.len());
    for bar in batch {
        match check_scale(market, bar) {
            Ok(()) => rows.push(encode_row(key.symbol_id, bar)?),
            Err(StoreError::PriceScale { .. }) => rejected += 1,
            Err(other) => return Err(other),
        }
    }
    Ok((rows, rejected))
}

/// Encode one price for storage: canonical decimal text.
fn encode_price(value: Decimal) -> String {
    value.normalize().to_string()
}

/// Materialize one candle row into a committed bar. Anything that does
/// not decode — bad decimal text, out-of-range stamp, negative volume, bad
/// adjustment JSON — refuses here as a database failure, never as a
/// defaulted bar.
fn bar_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Bar> {
    fn bad(col: usize, what: String) -> rusqlite::Error {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            std::io::Error::new(std::io::ErrorKind::InvalidData, what).into(),
        )
    }
    let ts_ms: i64 = row.get(1)?;
    let ts = DateTime::from_timestamp_millis(ts_ms)
        .ok_or_else(|| bad(1, format!("stamp {ts_ms} out of range")))?;
    let price = |col: usize| -> rusqlite::Result<Decimal> {
        let text: String = row.get(col)?;
        text.parse()
            .map_err(|_| bad(col, format!("price {text:?} is not a decimal")))
    };
    let volume = u64::try_from(row.get::<_, i64>(6)?)
        .map_err(|_| bad(6, "stored volume is negative".to_owned()))?;
    let adj_text: Option<String> = row.get(7)?;
    let adj = adj_text
        .map(|text| {
            serde_json::from_str(&text)
                .map_err(|_| bad(7, format!("adjustment {text:?} is not valid JSON")))
        })
        .transpose()?;
    Ok(Bar::new()
        .ts(ts)
        .o(price(2)?)
        .h(price(3)?)
        .l(price(4)?)
        .c(price(5)?)
        .volume(volume)
        .maybe_adj(adj)
        .build())
}

/// Fetch one symbol's rows, optionally bounded to a half-open
/// `[start, end)` stamp window. No `ORDER BY`: the caller sorts in Rust,
/// so the ascending guarantee never depends on a SQL string.
pub(crate) fn fetch_bars(
    conn: &Connection,
    symbol_id: SymbolId,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
) -> Result<Vec<Bar>, fluent_db::error::DbError> {
    let id = i64::from(symbol_id.0);
    match (start_ms, end_ms) {
        (Some(start), Some(end)) => fluent_db::query::query_rows(
            conn,
            &format!("{SELECT_CANDLE_SQL} WHERE symbol_id = ?1 AND ts_ms >= ?2 AND ts_ms < ?3"),
            params![id, start, end],
            bar_of,
        ),
        (Some(start), None) => fluent_db::query::query_rows(
            conn,
            &format!("{SELECT_CANDLE_SQL} WHERE symbol_id = ?1 AND ts_ms >= ?2"),
            params![id, start],
            bar_of,
        ),
        (None, Some(end)) => fluent_db::query::query_rows(
            conn,
            &format!("{SELECT_CANDLE_SQL} WHERE symbol_id = ?1 AND ts_ms < ?2"),
            params![id, end],
            bar_of,
        ),
        (None, None) => fluent_db::query::query_rows(
            conn,
            &format!("{SELECT_CANDLE_SQL} WHERE symbol_id = ?1"),
            params![id],
            bar_of,
        ),
    }
}

/// Fetch one symbol's stamps, optionally bounded to a half-open
/// `[start, end)` window. Stamps only — no decimal decode, no `Bar`
/// materialization — for coverage planning over the window a read will
/// actually serve. Sorted ascending in Rust, like the bar fetch: the
/// ascending guarantee lives in code, never in a SQL string.
pub(crate) fn fetch_stamps(
    conn: &Connection,
    symbol_id: SymbolId,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
) -> Result<Vec<i64>, fluent_db::error::DbError> {
    const SELECT_STAMPS_SQL: &str = "SELECT ts_ms FROM candle";
    let id = i64::from(symbol_id.0);
    let mut stamps = match (start_ms, end_ms) {
        (Some(start), Some(end)) => fluent_db::query::query_rows(
            conn,
            &format!("{SELECT_STAMPS_SQL} WHERE symbol_id = ?1 AND ts_ms >= ?2 AND ts_ms < ?3"),
            params![id, start, end],
            |row| row.get(0),
        ),
        (Some(start), None) => fluent_db::query::query_rows(
            conn,
            &format!("{SELECT_STAMPS_SQL} WHERE symbol_id = ?1 AND ts_ms >= ?2"),
            params![id, start],
            |row| row.get(0),
        ),
        (None, Some(end)) => fluent_db::query::query_rows(
            conn,
            &format!("{SELECT_STAMPS_SQL} WHERE symbol_id = ?1 AND ts_ms < ?2"),
            params![id, end],
            |row| row.get(0),
        ),
        (None, None) => fluent_db::query::query_rows(
            conn,
            &format!("{SELECT_STAMPS_SQL} WHERE symbol_id = ?1"),
            params![id],
            |row| row.get(0),
        ),
    }?;
    stamps.sort_unstable();
    Ok(stamps)
}

/// One tape's bounds from the index alone: first stamp, last stamp.
/// `None` is an empty table — answered, not failed.
///
/// Deliberately no row count: `COUNT(*)` walks every row of the symbol,
/// which would put the read path back at O(tape) per read — measured
/// linear before it was cut. The watermark anchor and hole clipping need
/// bounds, never a count; the audit path counts the bars it validates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TapeExtent {
    /// First stored stamp.
    pub(crate) first_ms: i64,
    /// Last stored stamp.
    pub(crate) last_ms: i64,
}

/// Read the tape's bounds without touching a single bar row: one `MIN`
/// query and one `MAX` query over the stamp index. Two queries, not one:
/// each alone resolves as a boundary seek through the primary key, while
/// the combined `MIN`/`MAX` row walks the symbol's whole range — measured
/// linear before the split. Interior holes are invisible here by design —
/// that is the stamp fetch's question, not this query's.
pub(crate) fn tape_extent(
    conn: &Connection,
    symbol_id: SymbolId,
) -> Result<Option<TapeExtent>, fluent_db::error::DbError> {
    let id = i64::from(symbol_id.0);
    // Aggregates always return exactly one row — `None` on an empty table
    // arrives as a `NULL` value, not as no rows — so the mapper reads a
    // nullable stamp and the outer absence flattens through it.
    let first: Option<i64> = fluent_db::query::query_row(
        conn,
        "SELECT MIN(ts_ms) FROM candle WHERE symbol_id = ?1",
        params![id],
        |row| row.get::<_, Option<i64>>(0),
    )?
    .flatten();
    let last: Option<i64> = fluent_db::query::query_row(
        conn,
        "SELECT MAX(ts_ms) FROM candle WHERE symbol_id = ?1",
        params![id],
        |row| row.get::<_, Option<i64>>(0),
    )?
    .flatten();
    Ok(match (first, last) {
        (Some(first_ms), Some(last_ms)) => Some(TapeExtent { first_ms, last_ms }),
        _ => None,
    })
}

/// The stamp-fetch window for a request against a known extent: pure over
/// the request, so both read layers derive the same bounds before touching
/// the database. Returns the fetch window the plan resolves against plus
/// the margined stamp window — one stored step each side, so a run
/// crossing the fetch edge has its straddling pair inside the stamp fetch
/// and the walk sees the same continuity the whole-tape walk sees.
pub(crate) fn coverage_windows(
    stored: Timeframe,
    extent: &TapeExtent,
    req: &ReadRequest,
) -> Result<(TimeRange, TimeRange), StoreError> {
    // The row count is unused below: `wanted_window` reads only the last
    // stamp, so the bounds carry a zero count rather than a tape-wide
    // `COUNT(*)` that would cost O(tape) per read.
    let bounds = Coverage::from_extent_and_runs(extent.first_ms, extent.last_ms, 0, Vec::new());
    let wanted = wanted_window(req, Some(&bounds))?;
    let fetch = crate::plan::fetch_span(req, stored, &wanted)?;
    let step = stored.duration_ms();
    let stamp_window = TimeRange {
        start_ms: fetch.start_ms.map(|start| start.saturating_sub(step)),
        end_ms: fetch.end_ms.map(|end| end.saturating_add(step)),
    };
    Ok((fetch, stamp_window))
}

/// Runs from window stamps clipped to the fetch window, over the tape-wide
/// extent bounds: the planning input both read layers resolve against.
/// Clipped runs equal whole-tape runs intersected with the fetch — run
/// adjacency is local, and the stamp margin keeps both straddling pairs
/// visible — so holes, spans, and bucket-wholeness resolve identically.
/// The bar count is the stamps observed inside the fetch window, matching
/// the runs' scope; nothing on the read path reads a tape-wide count.
pub(crate) fn coverage_from_stamps(
    extent: &TapeExtent,
    fetch: &TimeRange,
    stamps: &[i64],
    stored: Timeframe,
) -> Coverage {
    let bars = stamps.iter().filter(|stamp| fetch.contains(**stamp)).count();
    let runs = Coverage::from_stamps(stamps, stored)
        .map(|coverage| coverage.runs().to_vec())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|run| run.intersect(fetch))
        .collect::<Vec<_>>();
    Coverage::from_extent_and_runs(
        extent.first_ms,
        extent.last_ms,
        bars as u64,
        runs,
    )
}

/// Plan-grade coverage from stamps in the served window: the extent query
/// bounds the tape (an empty tape answers `None`, preserving the empty-tape
/// refusal) and the runs walk the stamps of the fetch window. The margin
/// keeps the cost bounded by the window: at most two extra stamps, still
/// O(window), never O(tape).
///
/// A row that cannot materialize outside the served window can no longer
/// refuse the read: the extent and stamp queries never touch its columns,
/// and the bar fetch never visits its span. Served bars stay
/// double-validated — [`order_and_validate`] before and after the fold —
/// and the audit path ([`CandleStore::coverage`]) keeps whole-tape
/// validation.
pub(crate) fn windowed_coverage(
    conn: &Connection,
    symbol_id: SymbolId,
    stored: Timeframe,
    req: &ReadRequest,
) -> Result<Option<Coverage>, StoreError> {
    let extent = tape_extent(conn, symbol_id)?;
    let Some(extent) = extent else {
        return Ok(None);
    };
    let (fetch, stamp_window) = coverage_windows(stored, &extent, req)?;
    let stamps = fetch_stamps(
        conn,
        symbol_id,
        stamp_window.start_ms,
        stamp_window.end_ms,
    )?;
    Ok(Some(coverage_from_stamps(&extent, &fetch, &stamps, stored)))
}

/// Fetch one symbol's every row: the whole-tape read behind validated
/// coverage. Audits, repair, and health may call this; the served read
/// paths must not — the budget suite pins that rule by name.
pub(crate) fn fetch_all_bars(
    conn: &Connection,
    symbol_id: SymbolId,
) -> Result<Vec<Bar>, fluent_db::error::DbError> {
    fetch_bars(conn, symbol_id, None, None)
}

/// Sort ascending and prove it: every adjacent pair passes the shared
/// monotonicity check. This is the boundary the runner inherits — a tape
/// that was written shuffled still reads back as a valid series.
pub(crate) fn order_and_validate(mut bars: Vec<Bar>) -> Result<Vec<Bar>, StoreError> {
    bars.sort_by_key(|bar| bar.ts);
    let mut last: Option<&Bar> = None;
    for bar in &bars {
        validate_new_bar(last, bar)
            .map_err(|e| StoreError::InvalidBar(format!("stored series out of order: {e}")))?;
        last = Some(bar);
    }
    Ok(bars)
}

/// The SQLite working store: a catalog plus its tape on one connection.
pub struct SqliteTape {
    catalog: MarketCatalog,
}

impl SqliteTape {
    /// Open (or create) the store at `path`, bringing the schema current.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            catalog: MarketCatalog::open(path)?,
        })
    }

    /// Open an ephemeral store backed by a private in-memory database.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Ok(Self {
            catalog: MarketCatalog::open_in_memory()?,
        })
    }

    /// The catalog behind this tape.
    pub fn catalog(&self) -> &MarketCatalog {
        &self.catalog
    }

    /// Register a new market. The catalog gates and refuses; this is the
    /// setup-time spelling of that call.
    pub fn register(
        &self,
        symbol: &str,
        venue: &str,
        asset_class: crate::catalog::AssetClass,
        price_scale: crate::catalog::PriceScale,
        time_zone: &str,
        timeframe: protopyne_types::Timeframe,
    ) -> Result<SymbolId, StoreError> {
        self.catalog.register(
            symbol,
            venue,
            asset_class,
            price_scale,
            time_zone,
            timeframe,
        )
    }

    /// The presence half of the gate, over the catalog's permit: proves
    /// the calling task was granted store access.
    fn gate(&self) -> Result<(), StoreError> {
        check_present(self.catalog.permit())
    }

    /// Resolve `key` to its market row, refusing an unregistered id with
    /// its number attached — the code-level referential check.
    fn market_for(&self, key: TapeKey) -> Result<Market, StoreError> {
        self.catalog.market_by_id(key.symbol_id)?.ok_or_else(|| {
            StoreError::Plan(PlanError::UnknownSymbol(format!(
                "symbol_id {}",
                key.symbol_id.0
            )))
        })
    }

    fn conn<T>(&self, f: impl FnOnce(&Mutex<Connection>) -> T) -> T {
        f(self.catalog.connection())
    }
}

impl CandleStore for SqliteTape {
    fn markets(&self) -> Result<Vec<Market>, StoreError> {
        self.gate()?;
        self.catalog.markets()
    }

    fn resolve(&self, symbol: &str, venue: &str) -> Result<Option<SymbolId>, StoreError> {
        self.catalog.resolve(symbol, venue)
    }

    fn coverage(&self, key: TapeKey) -> Result<Option<Coverage>, StoreError> {
        self.gate()?;
        self.market_for(key)?;
        let bars = self.conn(|conn| {
            let conn = common_core::sync::lock(conn);
            fetch_all_bars(&conn, key.symbol_id)
        })?;
        Ok(Coverage::from_bars(
            &order_and_validate(bars)?,
            key.timeframe,
        ))
    }

    fn append(
        &self,
        key: TapeKey,
        batch: &[Bar],
        policy: WritePolicy,
    ) -> Result<AppendOutcome, StoreError> {
        self.gate()?;
        let market = self.market_for(key)?;
        let (rows, rejected) = plan_rows(&market, key, batch)?;
        let mut outcome = self.conn(|conn| {
            let mut conn = common_core::sync::lock(conn);
            fluent_db::query::transaction(&mut conn, |tx| {
                append_rows(tx, key.symbol_id, &rows, policy)
            })
        })?;
        outcome.rejected = rejected;
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
        check_scale(&market, bar)?;
        let row = encode_row(key.symbol_id, bar)?;
        self.conn(|conn| {
            let conn = common_core::sync::lock(conn);
            let old: Option<Bar> = fluent_db::query::query_row(
                &conn,
                &format!("{SELECT_CANDLE_SQL} WHERE symbol_id = ?1 AND ts_ms = ?2"),
                params![i64::from(key.symbol_id.0), ts_ms],
                bar_of,
            )?;
            let Some(old) = old else {
                return Err(StoreError::InvalidBar(format!(
                    "no committed row at ts_ms {ts_ms} for repair"
                )));
            };
            let changed = conn.execute(
                UPDATE_CANDLE_SQL,
                params![
                    row.open,
                    row.high,
                    row.low,
                    row.close,
                    row.volume,
                    row.adj,
                    row.symbol_id,
                    row.ts_ms,
                ],
            )?;
            if changed != 1 {
                return Err(StoreError::InvalidBar(format!(
                    "repair touched {changed} rows at ts_ms {ts_ms}, expected exactly one"
                )));
            }
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
        })
    }

    fn read(&self, req: &ReadRequest) -> Result<CandleSeries, StoreError> {
        self.gate()?;
        // The tape lookup is pure (in-memory index, no I/O): unknown
        // refuses before any database touch.
        let (symbol_id, stored) = self
            .catalog
            .tape_of(&req.symbol, &req.venue)
            .ok_or_else(|| StoreError::Plan(PlanError::UnknownSymbol(req.symbol.clone())))?;
        // Stamp-planned coverage over the served window — extent plus
        // windowed runs — never a whole-tape bar fetch. The plan, the
        // per-span fetches, and the double validation below are unchanged.
        let coverage = self.conn(|conn| {
            let conn = common_core::sync::lock(conn);
            windowed_coverage(&conn, symbol_id, stored, req)
        })?;
        let plan = ReadPlan::resolve(&self.catalog, coverage, req)?;
        let served = wanted_window(req, Some(plan.covered()))?;
        let mut bars = Vec::new();
        for span in plan.spans() {
            let fetched = self.conn(|conn| {
                let conn = common_core::sync::lock(conn);
                fetch_bars(&conn, symbol_id, span.span().start_ms, span.span().end_ms)
            })?;
            bars.extend(fetched);
        }
        // Ascending before the fold: buckets assume order, and the database
        // never promised any.
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

impl std::fmt::Debug for SqliteTape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTape")
            .field("catalog", &self.catalog)
            .finish_non_exhaustive()
    }
}

/// Run one append inside an open transaction, on either the synchronous
/// core's connection or a pooled one: the statement shapes and the
/// conflict accounting are one implementation, not one per layer.
pub(crate) fn append_rows(
    tx: &mut rusqlite::Transaction<'_>,
    symbol_id: SymbolId,
    rows: &[EncodedRow],
    policy: WritePolicy,
) -> Result<AppendOutcome, fluent_db::error::DbError> {
    let mut outcome = AppendOutcome::default();
    match policy {
        WritePolicy::InsertIfAbsent => {
            for row in rows {
                match tx.execute(INSERT_CANDLE_SQL, row_params(row)) {
                    Ok(_) => outcome.inserted += 1,
                    Err(e) if fluent_db::sqlite::is_unique_violation(&e) => {
                        outcome.conflicts += 1;
                    }
                    Err(e) => return Err(fluent_db::error::DbError::from(e)),
                }
            }
        }
        WritePolicy::UpsertBySeq { .. } => {
            let mut existing: HashSet<i64> = HashSet::new();
            let mut probe = tx.prepare("SELECT ts_ms FROM candle WHERE symbol_id = ?1")?;
            let mut probe_rows = probe.query(params![i64::from(symbol_id.0)])?;
            while let Some(probe_row) = probe_rows.next()? {
                existing.insert(probe_row.get(0)?);
            }
            for row in rows {
                tx.execute(UPSERT_CANDLE_SQL, row_params(row))?;
                if existing.contains(&row.ts_ms) {
                    outcome.conflicts += 1;
                } else {
                    outcome.inserted += 1;
                }
            }
        }
    }
    Ok(outcome)
}

/// One row ready to bind: the request-independent half of an append.
pub(crate) struct EncodedRow {
    pub(crate) symbol_id: i64,
    pub(crate) ts_ms: i64,
    pub(crate) open: String,
    pub(crate) high: String,
    pub(crate) low: String,
    pub(crate) close: String,
    pub(crate) volume: i64,
    pub(crate) adj: Option<String>,
}

/// Encode one bar for the row shape. Volume past `i64 :: MAX` refuses:
/// the column cannot hold it, so the batch must not pretend it did.
pub(crate) fn encode_row(symbol_id: SymbolId, bar: &Bar) -> Result<EncodedRow, StoreError> {
    let volume = i64::try_from(bar.volume).map_err(|_| {
        StoreError::InvalidBar(format!("volume {} exceeds the stored range", bar.volume))
    })?;
    let adj = bar
        .adj
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| StoreError::InvalidBar(format!("adjustment does not serialize: {e}")))?;
    Ok(EncodedRow {
        symbol_id: i64::from(symbol_id.0),
        ts_ms: bar.ts.timestamp_millis(),
        open: encode_price(bar.o),
        high: encode_price(bar.h),
        low: encode_price(bar.l),
        close: encode_price(bar.c),
        volume,
        adj,
    })
}

/// Bind one encoded row. A free function over a value, not a method, so the
/// insert and the upsert bind identically — two statements, one binding.
pub(crate) fn row_params(row: &EncodedRow) -> [&dyn rusqlite::ToSql; 8] {
    [
        &row.symbol_id,
        &row.ts_ms,
        &row.open,
        &row.high,
        &row.low,
        &row.close,
        &row.volume,
        &row.adj,
    ]
}

#[cfg(test)]
#[path = "../tests/common/setup.rs"]
pub(crate) mod test_setup;

#[cfg(test)]
#[path = "../tests/common/oracle.rs"]
pub(crate) mod test_oracle;

#[cfg(test)]
#[path = "../tests/common/scale.rs"]
pub(crate) mod test_scale;

#[cfg(test)]
#[path = "../tests/append_idempotent.rs"]
mod tests_append;

#[cfg(test)]
#[path = "../tests/store_ordering.rs"]
mod tests_ordering;

#[cfg(test)]
#[path = "../tests/store_coverage.rs"]
mod tests_coverage;

#[cfg(test)]
#[path = "../tests/price_scale_calibration.rs"]
mod tests_price_scale;

#[cfg(test)]
#[path = "../tests/exactness_versus_f64.rs"]
mod tests_exactness;

#[cfg(test)]
#[path = "../tests/rejected_is_counted.rs"]
mod tests_rejected;

#[cfg(test)]
#[path = "../tests/plan_read_end_to_end.rs"]
mod tests_plan_read;

#[cfg(test)]
#[path = "../tests/read_windowed_oracle.rs"]
mod tests_read_oracle;

#[cfg(test)]
#[path = "../tests/read_budget.rs"]
mod tests_read_budget;
