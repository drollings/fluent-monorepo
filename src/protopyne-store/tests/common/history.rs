//! row parser behind the test adapter, a fixture CSV writer, scripted
//! providers, and handle helpers.
//!
//! Included once from `history.rs`; consumer suites address it as
//! `crate :: history :: test_history`. The adapter below is the one-line
//! bridge the file provider is designed around: the store never names the
//! names the server's parser, and tests inject it here — the same function
//! the server already uses, mapped field-for-field onto the provider error
//! taxonomy, so the suite proves reuse rather than reimplementation.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use fluent_concurrency::stream::StreamAbort;
use protopyne_types::{Bar, Timeframe};

use crate::catalog::AssetClass;
use crate::store::CandleStore;
use crate::tape::MarketTape;
use crate::history::{BoxFuture, FetchPage, FetchRequest, HistoryError, HistoryProvider};
use crate::key::TapeKey;
use crate::range::TimeRange;

/// The venue every history-test market prints on.
pub const VENUE: &str = "test-venue";

/// Parse CSV content exactly the way the server does: the same function,
/// mapped field-for-field onto the provider taxonomy. A bad row stays a bad
/// row with its line, a bad header stays a bad header, I/O stays I/O — the
/// ladder's stop/skip meaning survives the boundary because nothing is
/// collapsed.
pub fn server_parse(content: &str) -> Result<Vec<Bar>, HistoryError> {
    protopyne_server::csv::parse_bars_csv(content).map_err(|err| match err {
        protopyne_server::error::ServerError::BadRow { line, reason } => {
            HistoryError::BadRow { line, reason }
        }
        protopyne_server::error::ServerError::BadHeader(reason) => HistoryError::BadHeader(reason),
        protopyne_server::error::ServerError::Io(io) => HistoryError::Io(io),
        other => HistoryError::Unavailable(other.to_string()),
    })
}

/// Write `bars` as a history CSV `dir/name`: the header the ingest parser
/// requires, epoch-second stamps, decimal prices, integer volumes. A test
/// data writer, not a parser — parsing happens only through
/// [`server_parse`].
pub fn write_tape_csv(dir: &tempfile::TempDir, name: &str, bars: &[Bar]) -> PathBuf {
    let mut content = String::from("time,open,high,low,close,volume\n");
    for bar in bars {
        use std::fmt::Write as _;
        writeln!(
            content,
            "{},{},{},{},{},{}",
            bar.ts.timestamp(),
            bar.o,
            bar.h,
            bar.l,
            bar.c,
            bar.volume
        )
        .expect("writing to a String never fails");
    }
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("fixture CSV writes");
    path
}

/// What a scripted fetch answers.
#[derive(Debug, Clone)]
pub enum ScriptedFetch {
    /// Terminal: the provider has no data.
    Unavailable(String),
    /// Retryable: the fetch times out.
    Timeout(String),
    /// Serve these bars sliced to the requested span, one page.
    Bars(Vec<Bar>),
    /// Answer scripted responses in order, sticking on the last: the
    /// deterministic fail-then-heal driver — no clocks, no races.
    Sequence(Vec<ScriptedFetch>),
}

/// A scripted [`HistoryProvider`]: every reachability check and fetch is
/// logged in order, reachability flips on demand, and the fetch behavior is
/// fixed at construction. The log is the assertion surface for ordering
/// tests: each entry is `"check"`, `"horizon"`, or `"fetch [start, end)"`.
pub struct ScriptedProvider {
    horizon: Option<TimeRange>,
    reachable: AtomicBool,
    behavior: Mutex<ScriptedFetch>,
    calls: Mutex<Vec<String>>,
    abort_on_fetch: Mutex<Option<(usize, StreamAbort)>>,
    fetch_count: Mutex<usize>,
}

impl std::fmt::Debug for ScriptedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedProvider")
            .field("horizon", &self.horizon)
            .field("reachable", &self.reachable.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl ScriptedProvider {
    /// Script `behavior` with `horizon`, reachable.
    pub fn new(horizon: Option<TimeRange>, behavior: ScriptedFetch) -> Self {
        Self {
            horizon,
            reachable: AtomicBool::new(true),
            behavior: Mutex::new(behavior),
            calls: Mutex::new(Vec::new()),
            abort_on_fetch: Mutex::new(None),
            fetch_count: Mutex::new(0),
        }
    }

    /// Flip reachability mid-test: the connectivity re-check observes it on
    /// the next iteration.
    pub fn set_reachable(&self, reachable: bool) {
        self.reachable.store(reachable, Ordering::SeqCst);
    }

    /// Fire `abort` when the `n`th fetch starts (1-based): the deterministic
    /// cancellation driver — no sleeps, no timing.
    pub fn abort_on_fetch(&self, n: usize, abort: StreamAbort) {
        *self.abort_on_fetch.lock().expect("unpoisoned") = Some((n, abort));
    }

    /// The ordered call log.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("unpoisoned").clone()
    }

    /// How many fetches started.
    pub fn fetch_count(&self) -> usize {
        *self.fetch_count.lock().expect("unpoisoned")
    }

    fn log(&self, entry: String) {
        self.calls.lock().expect("unpoisoned").push(entry);
    }
}

impl HistoryProvider for ScriptedProvider {
    fn horizon(&self) -> Option<TimeRange> {
        self.log("horizon".to_owned());
        self.horizon
    }

    fn is_reachable(&self) -> bool {
        self.log("check".to_owned());
        self.reachable.load(Ordering::SeqCst)
    }

    fn fetch(&self, req: FetchRequest<'_>) -> BoxFuture<'_, Result<FetchPage, HistoryError>> {
        let (start, end) = (
            req.span.start_ms.unwrap_or(i64::MIN),
            req.span.end_ms.unwrap_or(i64::MAX),
        );
        self.log(format!("fetch [{start}, {end})"));
        let mut count = self.fetch_count.lock().expect("unpoisoned");
        *count += 1;
        let fired = self
            .abort_on_fetch
            .lock()
            .expect("unpoisoned")
            .clone()
            .filter(|(n, _)| *n == *count)
            .map(|(_, abort)| abort);
        drop(count);
        if let Some(abort) = fired {
            abort.cancel();
        }
        let page = {
            let mut guard = self.behavior.lock().expect("unpoisoned");
            let current = match &mut *guard {
                ScriptedFetch::Sequence(script) => {
                    if script.len() > 1 {
                        script.remove(0)
                    } else {
                        script
                            .first()
                            .cloned()
                            .unwrap_or(ScriptedFetch::Unavailable("empty script".to_owned()))
                    }
                }
                other => other.clone(),
            };
            drop(guard);
            match current {
                ScriptedFetch::Unavailable(reason) => Err(HistoryError::Unavailable(reason)),
                ScriptedFetch::Timeout(reason) => Err(HistoryError::Timeout(reason)),
                ScriptedFetch::Bars(bars) => {
                    let kept: Vec<Bar> = bars
                        .into_iter()
                        .filter(|bar| {
                            let ts = bar.ts.timestamp_millis();
                            ts >= start && ts < end
                        })
                        .collect();
                    Ok(FetchPage {
                        bars: kept,
                        next: None,
                    })
                }
                ScriptedFetch::Sequence(_) => {
                    Err(HistoryError::Unavailable("nested script".to_owned()))
                }
            }
        };
        Box::pin(async move { page })
    }
}

/// Open a file-backed tape with the root-bound token installed. The caller
/// keeps the permit and installs the returned capability set around each
/// gated effect. Opening itself gates nothing; registration and appends
/// inside the scope succeed, outside they are denied.
pub fn open_file_handle(
    dir: &tempfile::TempDir,
    name: &str,
) -> (
    MarketTape,
    crate::capability::StoreStorage,
    fluent_wvr::CapabilitySet,
) {
    let root = dir.path().join(name);
    let permit = crate::capability::StoreStorage::new(root.clone());
    let caps = fluent_wvr::CapabilitySet::new().with(permit.clone());
    let tape = MarketTape::open(&root).expect("tape opens");
    (tape, permit, caps)
}

/// Register `symbol` at a two-digit scale on the M1 tape. Returns the tape
/// key; the caller holds the capability scope.
pub fn register_m1(
    store: &MarketTape,
    _permit: &crate::capability::StoreStorage,
    symbol: &str,
) -> TapeKey {
    let symbol_id = store
        .register(
            symbol,
            VENUE,
            AssetClass::Stock,
            crate::catalog::PriceScale::of(2).expect("valid scale"),
            "UTC",
            Timeframe::M1,
        )
        .expect("registration succeeds");
    TapeKey {
        symbol_id,
        timeframe: Timeframe::M1,
    }
}

/// Append `bars` under `InsertIfAbsent`, asserting the full batch lands.
/// The caller holds the capability scope.
pub fn seed_append(
    store: &MarketTape,
    _permit: &crate::capability::StoreStorage,
    key: TapeKey,
    bars: &[Bar],
) {
    let outcome = store
        .append(key, bars, crate::store::WritePolicy::InsertIfAbsent)
        .expect("seed appends");
    assert_eq!(outcome.inserted, bars.len());
    assert_eq!(outcome.conflicts, 0);
}

/// Contiguous M1 bars: `n` bars from `first_step`, closing a cent higher
/// each bar so every bar is distinct and order-sensitive. Prices derive from
/// the absolute step, so the same step always carries the same price no
/// matter which window built it.
pub fn ascending_bars(first_step: i64, n: usize) -> Vec<Bar> {
    (0..n as i64)
        .map(|step| {
            let absolute = first_step + step;
            crate::commit::test_tapes::bar(
                absolute,
                &format!("{}.{:02}", 100 + absolute, absolute % 100),
            )
        })
        .collect()
}

/// The default repair bounds for tests that do not calibrate bounds: wide
/// spans, small pool, instant backoff.
pub fn test_config() -> crate::repair::RepairConfig {
    crate::repair::RepairConfig::new()
        .max_repair_span_ms(u64::MAX)
        .repair_concurrency(2)
        .backoff_base_ms(1)
        .backoff_max_shift(2)
        .backoff_ceiling_ms(5)
        .build()
}

/// A never-stale clock for tests: the millisecond after the last bar, so a
/// complete tape classifies fresh without session arithmetic.
pub fn now_after(bars: &[Bar]) -> i64 {
    bars.last()
        .expect("nonempty tape")
        .ts
        .timestamp_millis()
        .saturating_add(60_000)
}
