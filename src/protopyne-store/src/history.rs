//! History providers: where repair bars come from.
//!
//! A [`HistoryProvider`] serves committed bars for a tape over a span, one
//! page at a time. The repair loop consumes providers through this trait and
//! never touches files or sockets itself, so the loop is testable against
//! scripted fakes and the only production-shaped provider — the file reader
//! — stays a thin shell around bytes the caller already trusts.
//!
//! Exactly two implementations ship, and no multi-provider machinery:
//!
//! - [`FileHistory`]: one CSV file for one tape, parsed by the caller-supplied
//!   row parser. The crate never names the server's parser — the store is a
//!   substrate and takes no dependency on a pipeline layer — so the parser
//!   arrives injected: production and test wiring pass the same
//!   `parse_bars_csv` the server already uses, through the one-line adapter in
//!   `tests/common/history.rs`. There is exactly one CSV parser in the
//!   workspace; this module holds none.
//! - [`NetworkStub`]: the trait-shaped placeholder for a wired market-data
//!   producer. There is none — no transport implements the fetch, so every
//!   call reports [`HistoryError::Unavailable`] rather than pretending to
//!   dial.
//!
//! The error taxonomy preserves the ingest ladder's three-way distinction
//! instead of paralleling it: a bad header stops, a bad row or an I/O
//! failure skips to the next candidate, and the adapter maps each server-side
//! variant onto the same-named case here, field for field. Collapsing the
//! three into one "fetch failed" would lose the stop/skip decision the
//! ladder already made.

use std::fmt::Debug;
use std::path::PathBuf;

use common_core::error::IoError;
use protopyne_types::Bar;
use thiserror::Error;

use crate::key::TapeKey;
use crate::range::TimeRange;

/// A boxed sendable future: the provider trait's fetch shape without an
/// `async_trait` macro hiding a per-call box.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// What to fetch: one tape over one span, borrowed so the request itself
/// stays `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What to fetch: one tape over one span, borrowed so the request itself
/// stays `Copy`.
pub struct FetchRequest<'a> {
    /// The tape to serve, at its stored spacing.
    pub key: TapeKey,
    /// The span to serve, half-open `[start, end)` like every other span.
    pub span: &'a TimeRange,
}

/// One page of history plus the cursor for the rest. Paging stops here: no
/// page sizes, prefetch depths, or parallel cursors — just the still-unserved
/// remainder, or nothing when the span is fully served.
#[derive(Debug, Clone, PartialEq)]
/// One page of history plus the cursor for the rest. Paging stops here: no
/// page sizes, prefetch depths, or parallel cursors — just the still-unserved
/// remainder, or nothing when the span is fully served.
pub struct FetchPage {
    /// Committed bars for the leading part of the requested span, ascending.
    pub bars: Vec<Bar>,
    /// The still-unserved remainder of the requested span. `None` means the
    /// span is fully served — including when it served zero bars, which is
    /// an answered empty page, distinctly from a refusal.
    pub next: Option<TimeRange>,
}

/// Why a fetch could not serve. Terminal and retryable failures stay
/// distinct — "no data exists" and "try again" are both facts about the
/// world, and the repair loop treats them oppositely — and the row/header
/// cases keep the ingest ladder's stop/skip meaning across the boundary.
#[derive(Error, Debug)]
/// Why a fetch could not serve. Terminal and retryable failures stay
/// distinct — "no data exists" and "try again" are both facts about the
/// world, and the repair loop treats them oppositely — and the row/header
/// cases keep the ingest ladder's stop/skip meaning across the boundary.
pub enum HistoryError {
    /// The provider has no data for this span: a listing date, a delisting,
    /// an unknown symbol, the unwired network stub. Terminal — backing off
    /// cannot grow data that does not exist.
    #[error("history unavailable: {0}")]
    Unavailable(String),
    /// The fetch did not answer in time: a timeout, a reset connection.
    /// Retryable under backoff.
    #[error("history fetch timed out: {0}")]
    Timeout(String),
    /// A history row could not be converted to a bar. Mapped field-for-field
    /// from the ingest taxonomy's row case, so a corrupt file reads exactly
    /// like the ladder already classifies it.
    #[error("bad history row at line {line}: {reason}")]
    BadRow {
        /// 1-based line number including the header line.
        line: usize,
        /// What failed to parse and why.
        reason: String,
    },
    /// The history header lacks a required column. Mapped field-for-field
    /// from the ingest taxonomy's header case.
    #[error("bad history header: {0}")]
    BadHeader(String),
    /// The transport or file system refused the operation. Retryable-shaped:
    /// a transient failure heals, a permanent one stays loud per attempt.
    #[error("history I/O failure: {0}")]
    Io(#[from] IoError),
}

common_core::impl_from_io_error!(HistoryError);

impl HistoryError {
    /// Whether trying again may help. Only contention with the world —
    /// timeouts and transport failures — is transient. An absent span and
    /// unusable bytes are facts a retry reproduces exactly, so the repair
    /// loop treats them as terminal. This is the provider-side mirror of
    /// the store's own retry taxonomy, consulted the same way.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Io(_))
    }
}

/// Where repair bars come from: one tape, one span, paged.
pub trait HistoryProvider: Send + Sync + Debug {
    /// The span this provider can serve, or `None` when it cannot say
    /// without fetching. Repair spans are bounded by this horizon: a span no
    /// horizon covers is refused with its reason named, never truncated
    /// into a servable-looking prefix.
    fn horizon(&self) -> Option<TimeRange>;

    /// Whether a fetch is worth attempting right now. The repair loop
    /// re-checks this at the top of every retry iteration, so a long sleep
    /// never commits to a doomed fetch. Cheap and side-effect free: a file
    /// presence probe, a socket flag — never a fetch.
    fn is_reachable(&self) -> bool {
        true
    }

    /// Serve the leading page of `req.span`. Implementations slice their
    /// source and name the remainder in [`FetchPage::next`]; callers loop
    /// until it is `None`.
    fn fetch(&self, req: FetchRequest<'_>) -> BoxFuture<'_, Result<FetchPage, HistoryError>>;
}

/// The row parser a [`FileHistory`] reads through. A plain function pointer:
/// `Send + Sync + Copy + Debug` with no captures, so the provider stays
/// cloneable and the parser stays visibly injected rather than smuggled in
/// through a dependency edge the substrate must not have.
pub type ParseFn = fn(&str) -> Result<Vec<Bar>, HistoryError>;

/// How many bars one file page serves by default. Small enough that a cold
/// backfill pages; large enough that paging is not the bottleneck.
pub const DEFAULT_PAGE_BARS: usize = 1_024;

/// One CSV file for one tape, read eagerly and served from memory.
///
/// The file is read and parsed once, at construction — fixtures and one-off
/// backfills do not change under a repair pass, so a snapshot is the honest
/// shape, and serving from memory keeps `fetch` free of blocking I/O on any
/// runtime. This provider is ingest for tests and one-off runs, not a store:
/// for live or large histories the wired producer replaces it, and the unit
/// tests covering this provider are unchanged by that promotion.
#[derive(Debug, Clone)]
/// One CSV file for one tape, read eagerly and served from memory.
/// 
/// The file is read and parsed once, at construction — fixtures and one-off
/// backfills do not change under a repair pass, so a snapshot is the honest
/// shape, and serving from memory keeps `fetch` free of blocking I/O on any
/// runtime. This provider is ingest for tests and one-off runs, not a store:
/// for live or large histories the wired producer replaces it, and the unit
/// tests covering this provider are unchanged by that promotion.
pub struct FileHistory {
    path: PathBuf,
    key: TapeKey,
    bars: Vec<Bar>,
    horizon: Option<TimeRange>,
    page_size: usize,
}

impl FileHistory {
    /// Read `path` through `parse` and bind the result to `key`. Fails fast
    /// when the file is missing or unusable — a repair loop must never
    /// discover at fetch time what construction already knew.
    pub fn new(path: PathBuf, key: TapeKey, parse: ParseFn) -> Result<Self, HistoryError> {
        let content = common_core::io::read_to_string_err(&path)?;
        let bars = parse(&content)?;
        let horizon = horizon_of(&bars, key);
        Ok(Self {
            path,
            key,
            bars,
            horizon,
            page_size: DEFAULT_PAGE_BARS,
        })
    }

    /// Serve smaller pages, for tests that must observe multi-page fills.
    /// Clamped at one: a zero page serves nothing forever.
    #[must_use]
    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size.max(1);
        self
    }

    /// The file this provider was snapshot from.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Slice the snapshot to `req`, one page. The shared core behind both
    /// the async trait fetch and any synchronous driver: one implementation,
    /// no blocking I/O, safe on any thread.
    fn fetch_sync(&self, req: FetchRequest<'_>) -> Result<FetchPage, HistoryError> {
        if req.key != self.key {
            return Err(HistoryError::Unavailable(format!(
                "no such symbol: provider serves {:?} only",
                self.key
            )));
        }
        let (Some(start), Some(end)) = (req.span.start_ms, req.span.end_ms) else {
            return Err(HistoryError::Unavailable(format!(
                "unbounded fetch span {:?}: refusing rather than serving the whole file",
                req.span
            )));
        };
        // The snapshot parses ascending — the ingest parser enforces strict
        // bar order — so two binary searches bound the window. An inverted
        // span clamps to an empty window and serves an empty page, which the
        // repair loop reads as "no data", never as fetched bars.
        let hi = self
            .bars
            .partition_point(|bar| bar.ts.timestamp_millis() < end);
        let lo = self
            .bars
            .partition_point(|bar| bar.ts.timestamp_millis() < start)
            .min(hi);
        let window = &self.bars[lo..hi];
        let take = window.len().min(self.page_size.max(1));
        let bars = window[..take].to_vec();
        let next = if lo + take < hi {
            bars.last().map(|last| TimeRange {
                start_ms: Some(last.ts.timestamp_millis().saturating_add(1)),
                end_ms: Some(end),
            })
        } else {
            None
        };
        Ok(FetchPage { bars, next })
    }
}

/// The covered span of snapshot `bars` in bar-span convention
/// (`[first, last + step)`), or `None` for an empty snapshot.
fn horizon_of(bars: &[Bar], key: TapeKey) -> Option<TimeRange> {
    let first = bars.first()?;
    let last = bars.last()?;
    Some(TimeRange {
        start_ms: Some(first.ts.timestamp_millis()),
        end_ms: Some(
            last.ts
                .timestamp_millis()
                .saturating_add(key.timeframe.duration_ms()),
        ),
    })
}

impl HistoryProvider for FileHistory {
    fn horizon(&self) -> Option<TimeRange> {
        self.horizon
    }

    fn is_reachable(&self) -> bool {
        self.path.exists()
    }

    fn fetch(&self, req: FetchRequest<'_>) -> BoxFuture<'_, Result<FetchPage, HistoryError>> {
        let page = self.fetch_sync(req);
        Box::pin(async move { page })
    }
}

/// The unwired network reader: trait-shaped, data-free.
///
/// Every fetch reports [`HistoryError::Unavailable`] naming the missing
/// producer, so a repair pass over this provider terminates instead of
/// backing off on a connection that will never exist. Reachability stays at
/// its default: the fetch itself is the honest signal, and it always says
/// the producer is absent.
#[derive(Debug, Clone, Copy, Default)]
/// The unwired network reader: trait-shaped, data-free.
/// 
/// Every fetch reports [`HistoryError::Unavailable`] naming the missing
/// producer, so a repair pass over this provider terminates instead of
/// backing off on a connection that will never exist. Reachability stays at
/// its default: the fetch itself is the honest signal, and it always says
/// the producer is absent.
pub struct NetworkStub;

impl NetworkStub {
    /// The placeholder, carrying no configuration because there is nothing
    /// to configure yet.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl HistoryProvider for NetworkStub {
    fn horizon(&self) -> Option<TimeRange> {
        None
    }

    fn fetch(&self, _req: FetchRequest<'_>) -> BoxFuture<'_, Result<FetchPage, HistoryError>> {
        Box::pin(async move {
            Err(HistoryError::Unavailable(
                "network history provider is unwired: no market-data producer exists".to_owned(),
            ))
        })
    }
}

#[cfg(test)]
#[path = "../tests/common/history.rs"]
pub(crate) mod test_history;

#[cfg(test)]
#[path = "../tests/repair_classification.rs"]
mod tests_repair_classification;

#[cfg(test)]
#[path = "../tests/repair_single_flight.rs"]
mod tests_repair_single_flight;

#[cfg(test)]
#[path = "../tests/repair_idempotent.rs"]
mod tests_repair_idempotent;

#[cfg(test)]
#[path = "../tests/repair_backoff.rs"]
mod tests_repair_backoff;

#[cfg(test)]
#[path = "../tests/demand_fill.rs"]
mod tests_demand_fill;

#[cfg(test)]
#[path = "../tests/demand_fill_bounded.rs"]
mod tests_demand_fill_bounded;