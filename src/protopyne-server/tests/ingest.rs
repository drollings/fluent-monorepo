//! Ingest service fault-injection suite: boot repair, duplicate/out-of-order,
//! gap suspension, and one-pass recovery.

use crate::common_fixtures::money;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::{CapabilitySet, CURRENT_CAPS};
use protopyne_store::{
    AssetClass, BoxFuture, CandleStore, FetchPage, FetchRequest, HistoryError, HistoryProvider,
    MarketTape, PriceScale, StoreStorage, TapeKey, TimeRange,
};
use protopyne_types::session::SessionCalendar;
use protopyne_types::{Bar, Timeframe};
use protopyne_venue::{
    AdapterOutcome, FeedCapabilities, FeedError, FeedFuture, FeedSession, FeedStats,
    FeedSubscription, MarketDataFeed, MarketEvent, TradeBarAdapter, TradePrint,
};
use protopyne_view::TerminalGateway as _;
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use crate::ingest::{IngestConfig, IngestOutcome, IngestService};
use crate::serve_with_tape;

const BASE_MS: i64 = 1_700_060_400_000;

fn stamp(offset_ms: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_millis_opt(BASE_MS + offset_ms)
        .single()
        .expect("stamp")
}

fn bar(offset_ms: i64, close: &str) -> Bar {
    let price: Decimal = close.parse().expect("decimal");
    Bar::new()
        .ts(stamp(offset_ms))
        .o(price)
        .h(price)
        .l(price)
        .c(price)
        .volume(10)
        .build()
}

struct FeedPump {
    outboxes: Mutex<Vec<mpsc::Sender<MarketEvent>>>,
}

impl FeedPump {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            outboxes: Mutex::new(Vec::new()),
        })
    }

    fn push(&self, event: &MarketEvent) {
        let mut outboxes = common_core::sync::lock(&self.outboxes);
        outboxes.retain(|out| out.try_send(event.clone()).is_ok());
    }
}

impl MarketDataFeed for FeedPump {
    fn capabilities(&self) -> FeedCapabilities {
        FeedCapabilities {
            bars: true,
            ..FeedCapabilities::default()
        }
    }

    fn subscribe<'a>(
        &'a self,
        _subscriptions: &'a [FeedSubscription],
    ) -> FeedFuture<'a, Result<FeedSession, FeedError>> {
        Box::pin(async move {
            let (out, inbox) = mpsc::channel(64);
            common_core::sync::lock(&self.outboxes).push(out);
            Ok(FeedSession::new(
                inbox,
                StreamAbort::new(),
                Arc::new(FeedStats::default()),
            ))
        })
    }
}

#[derive(Debug)]
struct Provider {
    key: TapeKey,
    bars: Vec<Bar>,
}

impl HistoryProvider for Provider {
    fn horizon(&self) -> Option<TimeRange> {
        let first = self.bars.first()?.ts.timestamp_millis();
        let last = self.bars.last()?.ts.timestamp_millis();
        TimeRange::bounded(first, last.saturating_add(self.key.timeframe.duration_ms())).ok()
    }

    fn fetch(&self, req: FetchRequest<'_>) -> BoxFuture<'_, Result<FetchPage, HistoryError>> {
        let key = req.key;
        let (Some(start), Some(end)) = (req.span.start_ms, req.span.end_ms) else {
            return Box::pin(std::future::ready(Err(HistoryError::Unavailable(
                "unbounded".to_owned(),
            ))));
        };
        let bars: Vec<Bar> = self
            .bars
            .iter()
            .filter(|bar| {
                let ts = bar.ts.timestamp_millis();
                key == self.key && ts >= start && ts < end
            })
            .cloned()
            .collect();
        Box::pin(std::future::ready(Ok(FetchPage { bars, next: None })))
    }
}

fn config() -> IngestConfig {
    IngestConfig::new()
        .tape_root(":memory:".to_owned())
        .venue("test".to_owned())
        .symbols(vec!["SPY".to_owned()])
        .timeframe("1M".to_owned())
        .bootstrap_bars(10)
        .build()
}

fn event(bar: Bar, closed: bool) -> MarketEvent {
    MarketEvent::BarUpdate {
        update: protopyne_venue::BarUpdate {
            underlying: "SPY".to_owned(),
            bar,
            closed,
        },
    }
}

#[tokio::test]
async fn duplicate_out_of_order_and_forming_are_counted_not_written() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps, async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let feed = FeedPump::new();
            let service = IngestService::new(
                config(),
                Arc::clone(&tape),
                permit.clone(),
                feed,
                vec![(
                    symbol,
                    Arc::new(Provider {
                        key,
                        bars: Vec::new(),
                    }),
                )],
                SessionCalendar::us_equities(),
            )
            .expect("service");

            assert_eq!(
                service.handle_event(&event(bar(0, "100"), false)),
                IngestOutcome::Ignored
            );
            assert_eq!(
                service.handle_event(&event(bar(0, "100"), true)),
                IngestOutcome::Appended
            );
            assert_eq!(
                service.handle_event(&event(bar(0, "100"), true)),
                IngestOutcome::Duplicate
            );
            assert_eq!(
                service.handle_event(&event(bar(-60_000, "99"), true)),
                IngestOutcome::OutOfOrder
            );
            assert_eq!(service.health().conflicts, 1);
            assert_eq!(service.health().out_of_order, 1);
            let coverage = tape.coverage(key).expect("coverage").expect("some");
            assert_eq!(coverage.bars, 1);
        })
        .await;
}

#[tokio::test]
async fn gap_suspends_then_one_repair_pass_resumes() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps, async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let provider = Arc::new(Provider {
                key,
                bars: vec![bar(0, "100"), bar(60_000, "101"), bar(120_000, "102")],
            });
            let feed = FeedPump::new();
            let service = IngestService::new(
                config(),
                Arc::clone(&tape),
                permit.clone(),
                feed,
                vec![(symbol, provider)],
                SessionCalendar::us_equities(),
            )
            .expect("service");
            assert_eq!(
                service.handle_event(&event(bar(0, "100"), true)),
                IngestOutcome::Appended
            );
            let gap = service.handle_event(&event(bar(180_000, "103"), true));
            assert!(matches!(gap, IngestOutcome::Gap { .. }));
            assert!(service.backfill_handle().entries_suspended());
            assert!(service.health().open_gaps > 0);

            let complete = service
                .repair_gap(
                    "SPY",
                    BASE_MS + 60_000,
                    BASE_MS + 180_000,
                    BASE_MS + 200_000,
                )
                .await
                .expect("repair");
            assert!(complete, "provider fills the hole");
            assert!(!service.backfill_handle().entries_suspended());
            let coverage = tape.coverage(key).expect("coverage").expect("some");
            assert_eq!(coverage.bars, 4);
        })
        .await;
}

#[tokio::test]
async fn bootstrap_repairs_a_partial_tape_and_seeds_from_the_tape() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps, async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            tape.append(
                key,
                &[bar(0, "100")],
                protopyne_store::WritePolicy::InsertIfAbsent,
            )
            .expect("seed");
            let provider = Arc::new(Provider {
                key,
                bars: vec![bar(0, "100"), bar(60_000, "101"), bar(120_000, "102")],
            });
            let feed = FeedPump::new();
            let service = IngestService::new(
                config(),
                Arc::clone(&tape),
                permit.clone(),
                feed,
                vec![(symbol, provider)],
                SessionCalendar::us_equities(),
            )
            .expect("service");
            let reports = service
                .bootstrap(BASE_MS + 180_000)
                .await
                .expect("bootstrap");
            assert!(reports.iter().any(|report| report.inserted > 0));
            let coverage = tape.coverage(key).expect("coverage").expect("some");
            assert_eq!(coverage.bars, 3);
            assert!(service.health().watermark_age_ms.is_some());
        })
        .await;
}

#[tokio::test]
async fn feed_session_run_consumes_a_closed_bar() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps, async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let feed = FeedPump::new();
            let service = IngestService::new(
                config(),
                Arc::clone(&tape),
                permit.clone(),
                Arc::clone(&feed) as Arc<dyn MarketDataFeed>,
                vec![(
                    symbol,
                    Arc::new(Provider {
                        key,
                        bars: Vec::new(),
                    }),
                )],
                SessionCalendar::us_equities(),
            )
            .expect("service");
            let session = service.subscribe().await.expect("session");
            feed.push(&event(bar(0, "100"), true));
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(120), service.run(session))
                    .await;
            let coverage = tape.coverage(key).expect("coverage").expect("some");
            assert_eq!(coverage.bars, 1);
        })
        .await;
}

#[test]
fn freshness_defaults_are_frozen() {
    let policy = config().freshness();
    assert_eq!(policy.max_lag_bars, 1);
    assert_eq!(policy.max_gap_bars, 0);
    let _ = money("1.00");
}

/// Dropped-tick recovery end to end: prints cross a missing minute bucket,
/// the adapter closes around the hole, ingest suspends on the gap, one
/// repair pass fills it from the provider, and the tape ends contiguous.
/// The window is the latest five fully in-session minutes, so the verdict
/// is deterministic at any wall-clock hour; the repair carries a
/// controlled clock while the supervised loop observes every event (its own
/// real-`now` attempt degrades to a harmless tail probe). The per-step rules
/// are pinned by the smoke tests; this soak proves the composed arc over the
/// real print-to-bar path.
#[ignore = "soak: dropped-tick print stream through the adapter into ingest repair; per-step rules are pinned by smoke tests every run"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soak_dropped_tick_prints_recover_contiguous_through_repair() {
    // Latest five fully in-session minutes: weekends and overnights scan
    // back to Friday afternoon, so the hole always skips a due bar.
    let calendar = SessionCalendar::us_equities();
    let mut base_ms = live_base_ms(5);
    for _ in 0..10_000 {
        if (0..5).all(|step| calendar.contains(base_ms + step * 60_000)) {
            break;
        }
        base_ms -= 60_000;
    }
    assert!(
        (0..5).all(|step| calendar.contains(base_ms + step * 60_000)),
        "an in-session five-minute window always exists"
    );
    let stamp_at = |offset_ms: i64| {
        Utc.timestamp_millis_opt(base_ms + offset_ms)
            .single()
            .expect("stamp")
    };
    let live_bar = |offset_ms: i64, close: &str| {
        let price: Decimal = close.parse().expect("decimal");
        Bar::new()
            .ts(stamp_at(offset_ms))
            .o(price)
            .h(price)
            .l(price)
            .c(price)
            .volume(10)
            .build()
    };
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps.clone(), async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let provider = Arc::new(Provider {
                key,
                bars: vec![
                    live_bar(0, "100"),
                    live_bar(60_000, "101"),
                    live_bar(120_000, "102"),
                    live_bar(180_000, "103"),
                    live_bar(240_000, "104"),
                ],
            });
            let feed = FeedPump::new();
            let service = IngestService::new(
                config(),
                Arc::clone(&tape),
                permit.clone(),
                Arc::clone(&feed) as Arc<dyn MarketDataFeed>,
                vec![(symbol, provider)],
                SessionCalendar::us_equities(),
            )
            .expect("service");
            let session = service.subscribe().await.expect("session");
            let abort = session.abort.clone();
            let running = Arc::new(service);
            let runner = Arc::clone(&running);
            let run = tokio::spawn(
                CURRENT_CAPS.scope(caps.clone(), async move { runner.run(session).await }),
            );
            // One print per bucket, skipping the 120_000 bucket entirely.
            let mut adapter = TradeBarAdapter::new(Timeframe::M1).expect("adapter");
            let mut closed = 0;
            for (offset_ms, price) in [
                (0, "100"),
                (60_000, "101"),
                (180_000, "103"),
                (240_000, "104"),
            ] {
                let print = TradePrint {
                    ts: stamp_at(offset_ms),
                    price: money(price),
                    size: 1,
                    aggressor: None,
                };
                match adapter.ingest(print).expect("tick") {
                    AdapterOutcome::Closed { bar, .. } => {
                        feed.push(&MarketEvent::Candle {
                            underlying: "SPY".to_owned(),
                            bar,
                        });
                        closed += 1;
                    }
                    AdapterOutcome::Forming(_) => {}
                    outcome => panic!("no refused prints in the soak stream, got {outcome:?}"),
                }
            }
            assert_eq!(closed, 3, "three boundaries close around the hole");
            poll_until("the hole is observed", 5_000, || {
                running.backfill_handle().entries_suspended()
            })
            .await;
            running
                .repair_gap(
                    "SPY",
                    base_ms + 120_000,
                    base_ms + 180_000,
                    base_ms + 240_000,
                )
                .await
                .expect("controlled repair");
            poll_until("the tape turns contiguous", 5_000, || {
                tape.coverage(key)
                    .ok()
                    .flatten()
                    .is_some_and(|coverage| coverage.bars == 4)
                    && !running.backfill_handle().entries_suspended()
            })
            .await;
            abort.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("the run stops")
                .expect("the run joins")
                .expect("abort is a clean exit");
            let coverage = tape.coverage(key).expect("coverage").expect("some");
            assert_eq!(coverage.bars, 4, "the repaired tape holds every bucket");
            assert_eq!(coverage.first.ts_ms, base_ms);
            assert_eq!(coverage.last.ts_ms, base_ms + 180_000);
            assert!(
                !running.backfill_handle().entries_suspended(),
                "entries resume once the hole is spliced"
            );
            assert!(
                running
                    .health()
                    .last_repair
                    .is_some_and(|report| report.contains("complete=true")),
                "the dropped bucket was observed as a gap and repaired shut"
            );
        })
        .await;
}

/// Minute-aligned base `minutes_back` minutes behind real now: stamps the
/// tape reads as live, so classification exercises the interior path rather
/// than the stale tail.
fn live_base_ms(minutes_back: i64) -> i64 {
    let now_ms = Utc::now().timestamp_millis();
    now_ms.div_euclid(60_000) * 60_000 - minutes_back * 60_000
}

fn bar_at(ts_ms: i64, close: &str) -> Bar {
    let price: Decimal = close.parse().expect("decimal");
    Bar::new()
        .ts(Utc.timestamp_millis_opt(ts_ms).single().expect("stamp"))
        .o(price)
        .h(price)
        .l(price)
        .c(price)
        .volume(10)
        .build()
}

fn closed_candle(bar: Bar) -> MarketEvent {
    MarketEvent::Candle {
        underlying: "SPY".to_owned(),
        bar,
    }
}

/// Spin until `ready` or the deadline passes. The supervised paths publish
/// on their own cadence; polling with a deadline keeps the suite fast
/// without a fixed sleep that flakes under load.
async fn poll_until(label: &str, timeout_ms: u64, mut ready: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(timeout_ms);
    while !ready() {
        assert!(
            start.elapsed() <= deadline,
            "{label} never became ready within {timeout_ms}ms"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Kill mid-write, reopen over the same tape, bootstrap: the torn tail
/// repairs from the provider and the new service seeds its watermarks from
/// the tape rather than memory. In-memory stamps die with the killed
/// service; only the tape survives.
#[tokio::test]
async fn torn_tail_kill_reopen_repairs_through_bootstrap() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps, async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let provider = Arc::new(Provider {
                key,
                bars: vec![
                    bar(0, "100"),
                    bar(60_000, "101"),
                    bar(120_000, "102"),
                    bar(180_000, "103"),
                    bar(240_000, "104"),
                ],
            });
            let first = IngestService::new(
                config(),
                Arc::clone(&tape),
                permit.clone(),
                FeedPump::new(),
                vec![(symbol, Arc::clone(&provider) as Arc<dyn HistoryProvider>)],
                SessionCalendar::us_equities(),
            )
            .expect("service");
            assert_eq!(
                first.handle_event(&event(bar(0, "100"), true)),
                IngestOutcome::Appended
            );
            assert_eq!(
                first.handle_event(&event(bar(60_000, "101"), true)),
                IngestOutcome::Appended
            );
            drop(first);
            let second = IngestService::new(
                config(),
                Arc::clone(&tape),
                permit.clone(),
                FeedPump::new(),
                vec![(symbol, provider)],
                SessionCalendar::us_equities(),
            )
            .expect("reopen");
            let reports = second
                .bootstrap(BASE_MS + 300_000)
                .await
                .expect("bootstrap");
            let inserted: usize = reports.iter().map(|report| report.inserted).sum();
            assert_eq!(inserted, 3, "the torn tail fills from the provider");
            let coverage = tape.coverage(key).expect("coverage").expect("some");
            assert_eq!(coverage.bars, 5);
            assert!(second.health().watermark_age_ms.is_some());
            assert_eq!(
                second.handle_event(&event(bar(300_000, "105"), true)),
                IngestOutcome::Appended,
                "the reopened service seeded its watermark from the tape"
            );
        })
        .await;
}

/// A weekend span the provider cannot fill repairs nothing and fabricates
/// nothing: no phantom bars appear, the tape keeps exactly its two edge
/// bars, and the gap stays honestly open. No session verdict is asserted
/// here — the exchange-session interpretation belongs to the freshness
/// calibration milestone, not to repair.
#[tokio::test]
async fn weekend_span_repair_fills_nothing_and_fabricates_nothing() {
    // Friday 2026-06-05 15:59 ET (EDT) to Monday 2026-06-08 09:30 ET.
    const FRI_MS: i64 = 1780689540000;
    const MON_MS: i64 = 1780925400000;
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps, async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let edges = vec![bar_at(FRI_MS, "100"), bar_at(MON_MS, "101")];
            tape.append(key, &edges, protopyne_store::WritePolicy::InsertIfAbsent)
                .expect("edge bars land");
            let provider = Arc::new(Provider { key, bars: edges });
            let service = IngestService::new(
                config(),
                Arc::clone(&tape),
                permit.clone(),
                FeedPump::new(),
                vec![(symbol, provider)],
                SessionCalendar::us_equities(),
            )
            .expect("service");
            let complete = service
                .repair_gap("SPY", FRI_MS + 60_000, MON_MS, MON_MS + 60_000)
                .await
                .expect("repair");
            assert!(!complete, "an unfillable weekend span stays honestly open");
            let coverage = tape.coverage(key).expect("coverage").expect("some");
            assert_eq!(coverage.bars, 2, "repair invents no bars");
            assert_eq!(coverage.first.ts_ms, FRI_MS);
            assert_eq!(coverage.last.ts_ms, MON_MS);
            assert!(
                service
                    .health()
                    .last_repair
                    .is_some_and(|report| report.contains("complete=false")),
                "the open span is recorded, not swallowed"
            );
        })
        .await;
}

/// Reconnect splice: the feed drops mid-stream, a fresh session resumes on
/// the same service, and the tape stays contiguous — no duplicate writes,
/// no missed gap. The service-level watermarks outlive the session, so the
/// splice is a continuation rather than a restart. Fixed in-session stamps
/// keep the verdicts deterministic at any wall-clock hour; the hole-repair
/// arc is soaked separately with a controlled clock.
#[tokio::test]
async fn reconnect_splice_continues_without_duplicates_or_gaps() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps.clone(), async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let feed = FeedPump::new();
            let service = Arc::new(
                IngestService::new(
                    config(),
                    Arc::clone(&tape),
                    permit.clone(),
                    Arc::clone(&feed) as Arc<dyn MarketDataFeed>,
                    vec![(
                        symbol,
                        Arc::new(Provider {
                            key,
                            bars: Vec::new(),
                        }) as Arc<dyn HistoryProvider>,
                    )],
                    SessionCalendar::us_equities(),
                )
                .expect("service"),
            );
            // First session: two contiguous bars, then the feed drops.
            let session = service.subscribe().await.expect("session");
            let abort = session.abort.clone();
            let running = Arc::clone(&service);
            let run = tokio::spawn(
                CURRENT_CAPS.scope(caps.clone(), async move { running.run(session).await }),
            );
            feed.push(&closed_candle(bar_at(WED_10_00, "100")));
            feed.push(&closed_candle(bar_at(WED_10_01, "101")));
            poll_until("first session lands", 2_000, || {
                tape.coverage(key)
                    .ok()
                    .flatten()
                    .is_some_and(|coverage| coverage.bars == 2)
            })
            .await;
            abort.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("the dropped run stops")
                .expect("the run joins")
                .expect("abort is a clean exit");
            // Second session: a contiguous splice, then a redelivered
            // duplicate. Neither suspends nor opens a gap.
            let session = service.subscribe().await.expect("resubscribe");
            let abort = session.abort.clone();
            let running = Arc::clone(&service);
            let run = tokio::spawn(
                CURRENT_CAPS.scope(caps.clone(), async move { running.run(session).await }),
            );
            // The next due bucket: a contiguous splice, not a hole.
            feed.push(&closed_candle(bar_at(WED_10_01 + 60_000, "102")));
            poll_until("splice continues", 2_000, || {
                tape.coverage(key)
                    .ok()
                    .flatten()
                    .is_some_and(|coverage| coverage.bars == 3)
            })
            .await;
            assert!(
                !service.backfill_handle().entries_suspended(),
                "a contiguous splice suspends nothing"
            );
            feed.push(&closed_candle(bar_at(WED_10_01 + 60_000, "102")));
            poll_until("duplicate counts", 2_000, || {
                service.health().conflicts == 1
            })
            .await;
            let coverage = tape.coverage(key).expect("coverage").expect("some");
            assert_eq!(coverage.bars, 3, "the duplicate wrote no row");
            assert_eq!(coverage.first.ts_ms, WED_10_00);
            assert_eq!(coverage.last.ts_ms, WED_10_01 + 60_000);
            assert_eq!(
                service.health().feed.received,
                2,
                "the second session consumed exactly its two events"
            );
            abort.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("the second run stops")
                .expect("the run joins")
                .expect("abort is a clean exit");
        })
        .await;
}

/// The tape reader a served boot test needs: the store, the permit-bound
/// caps it gates on, and the venue the ingest config names.
fn boot_reader(tape: &Arc<MarketTape>, permit: &StoreStorage) -> crate::boot::TapeReader {
    crate::boot::TapeReader {
        store: Arc::clone(tape),
        caps: CapabilitySet::new().with(permit.clone()),
        permit: permit.clone(),
        venue: "test".to_owned(),
        limit: crate::DEFAULT_TAPE_LIMIT,
    }
}

fn history_bars() -> Vec<Bar> {
    vec![bar(0, "100"), bar(60_000, "101")]
}

/// Without ingest, the served boot publishes no ingest health and `/health`
/// carries no ingest keys — the disabled path is byte-identical.
#[tokio::test]
async fn ingest_disabled_boot_leaves_health_absent() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps, async {
            tape.register(
                "SPY",
                "test",
                AssetClass::Stock,
                PriceScale::new().build(),
                "America/New_York",
                Timeframe::M1,
            )
            .expect("registered");
            let reader = boot_reader(&tape, &permit);
            let config = crate::boot::ServerConfig::new()
                .bind("127.0.0.1:0".to_owned())
                .build();
            let handle = serve_with_tape(config, history_bars(), Some(reader), None)
                .await
                .expect("server boots without ingest");
            assert!(
                handle.state.ingest_health().is_none(),
                "no ingest reading is ever published"
            );
            let body = reqwest::Client::new()
                .get(format!("http://{}/health", handle.addr))
                .send()
                .await
                .expect("route reachable")
                .text()
                .await
                .expect("body reads");
            assert!(
                !body.contains("ingest"),
                "the disabled path carries no ingest keys"
            );
            handle.shutdown().await;
        })
        .await;
}

/// With ingest configured, the served boot constructs the service, runs the
/// bootstrap, supervises the run loop and the health publisher under its own
/// scope, and shuts them down with the server. A live candle pushed through
/// the attached feed lands on the tape.
#[tokio::test]
async fn ingest_boot_constructs_supervises_and_publishes() {
    let base_ms = live_base_ms(2);
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps, async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let reader = boot_reader(&tape, &permit);
            let mut server = crate::boot::ServerConfig::new()
                .bind("127.0.0.1:0".to_owned())
                .build();
            server.ingest = Some(config());
            let feed = FeedPump::new();
            let boot = crate::boot::IngestBoot {
                feed: Arc::clone(&feed) as Arc<dyn MarketDataFeed>,
                providers: vec![(
                    symbol,
                    Arc::new(Provider {
                        key,
                        bars: Vec::new(),
                    }) as Arc<dyn HistoryProvider>,
                )],
            };
            let handle = serve_with_tape(server, history_bars(), Some(reader), Some(boot))
                .await
                .expect("server boots with ingest");
            poll_until("the publisher mirrors the service", 2_000, || {
                handle.state.ingest_health().is_some()
            })
            .await;
            feed.push(&closed_candle(bar_at(base_ms, "100")));
            poll_until("the live bar lands on the tape", 2_000, || {
                tape.coverage(key)
                    .ok()
                    .flatten()
                    .is_some_and(|coverage| coverage.bars == 1)
            })
            .await;
            poll_until("the feed is observed", 2_000, || {
                handle
                    .state
                    .ingest_health()
                    .is_some_and(|health| health.feed.received >= 1)
            })
            .await;
            let view = handle.gateway().health().expect("gateway health reads");
            assert_eq!(
                view.ingest_open_gaps,
                Some(0),
                "the supervised health surfaces on the gateway"
            );
            handle.shutdown().await;
        })
        .await;
}

/// Every half-wired combination refuses before binding: config without a
/// feed, a feed without config, ingest without a tape, and an unknown
/// ingest symbol. Nothing listens on a refused boot.
#[tokio::test]
async fn ingest_half_wiring_refuses_to_bind() {
    let boot_one = || {
        let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
        let permit = StoreStorage::new(tape.root().to_path_buf());
        (tape, permit)
    };
    let register = |tape: &Arc<MarketTape>| {
        tape.register(
            "SPY",
            "test",
            AssetClass::Stock,
            PriceScale::new().build(),
            "America/New_York",
            Timeframe::M1,
        )
        .expect("registered")
    };
    // Config without a feed.
    {
        let (tape, permit) = boot_one();
        let caps = CapabilitySet::new().with(permit.clone());
        CURRENT_CAPS
            .scope(caps, async {
                register(&tape);
                let mut server = crate::boot::ServerConfig::new()
                    .bind("127.0.0.1:0".to_owned())
                    .build();
                server.ingest = Some(config());
                let error = serve_with_tape(
                    server,
                    history_bars(),
                    Some(boot_reader(&tape, &permit)),
                    None,
                )
                .await
                .err()
                .expect("config without a feed refuses");
                assert!(
                    format!("{error:?}").contains("no feed"),
                    "names the missing feed: {error:?}"
                );
            })
            .await;
    }
    // Feed without config.
    {
        let (tape, permit) = boot_one();
        let caps = CapabilitySet::new().with(permit.clone());
        CURRENT_CAPS
            .scope(caps, async {
                let symbol = register(&tape);
                let boot = crate::boot::IngestBoot {
                    feed: FeedPump::new() as Arc<dyn MarketDataFeed>,
                    providers: vec![(
                        symbol,
                        Arc::new(Provider {
                            key: TapeKey {
                                symbol_id: symbol,
                                timeframe: Timeframe::M1,
                            },
                            bars: Vec::new(),
                        }) as Arc<dyn HistoryProvider>,
                    )],
                };
                let server = crate::boot::ServerConfig::new()
                    .bind("127.0.0.1:0".to_owned())
                    .build();
                let error = serve_with_tape(
                    server,
                    history_bars(),
                    Some(boot_reader(&tape, &permit)),
                    Some(boot),
                )
                .await
                .err()
                .expect("a feed without config refuses");
                assert!(
                    format!("{error:?}").contains("without ingest config"),
                    "names the missing config: {error:?}"
                );
            })
            .await;
    }
    // Ingest without a tape, and an unknown ingest symbol.
    {
        let (tape, permit) = boot_one();
        let caps = CapabilitySet::new().with(permit.clone());
        CURRENT_CAPS
            .scope(caps, async {
                let symbol = register(&tape);
                let feed = || FeedPump::new() as Arc<dyn MarketDataFeed>;
                let providers = || {
                    vec![(
                        symbol,
                        Arc::new(Provider {
                            key: TapeKey {
                                symbol_id: symbol,
                                timeframe: Timeframe::M1,
                            },
                            bars: Vec::new(),
                        }) as Arc<dyn HistoryProvider>,
                    )]
                };
                let mut server = crate::boot::ServerConfig::new()
                    .bind("127.0.0.1:0".to_owned())
                    .build();
                server.ingest = Some(config());
                let error = serve_with_tape(
                    server,
                    history_bars(),
                    None,
                    Some(crate::boot::IngestBoot {
                        feed: feed(),
                        providers: providers(),
                    }),
                )
                .await
                .err()
                .expect("ingest without a tape refuses");
                assert!(
                    format!("{error:?}").contains("without a durable tape"),
                    "names the missing tape: {error:?}"
                );
                let mut stranger = crate::boot::ServerConfig::new()
                    .bind("127.0.0.1:0".to_owned())
                    .build();
                stranger.ingest = Some(
                    IngestConfig::new()
                        .tape_root(":memory:".to_owned())
                        .venue("test".to_owned())
                        .symbols(vec!["NOPE".to_owned()])
                        .timeframe("1M".to_owned())
                        .build(),
                );
                let error = serve_with_tape(
                    stranger,
                    history_bars(),
                    Some(boot_reader(&tape, &permit)),
                    Some(crate::boot::IngestBoot {
                        feed: feed(),
                        providers: providers(),
                    }),
                )
                .await
                .err()
                .expect("an unknown ingest symbol refuses");
                assert!(
                    format!("{error:?}").contains("NOPE"),
                    "names the unknown symbol: {error:?}"
                );
            })
            .await;
    }
}

/// The weekday-aware calendar the gap corpus reads: regular US equity hours,
/// Thanksgiving closed, the Friday after at a 13:00 early close.
fn equities_calendar() -> SessionCalendar {
    SessionCalendar::us_equities()
        .with_holiday((2026, 11, 26))
        .with_half_day(
            (2026, 11, 27),
            protopyne_types::session::TradingSession::parse("0930-1300").expect("half parses"),
        )
}

/// One calibration fixture: closed-bar stamps in, verdicts out. A positive
/// gap verdict names genuinely missing trading bars; every control must
/// append quietly.
struct VerdictFixture {
    name: &'static str,
    stamps: Vec<i64>,
    expected: Vec<Verdict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Appended,
    Duplicate,
    OutOfOrder,
    Ignored,
    Unknown,
    Gap,
}

impl Verdict {
    fn of(outcome: &IngestOutcome) -> Self {
        match outcome {
            IngestOutcome::Ignored => Self::Ignored,
            IngestOutcome::Appended => Self::Appended,
            IngestOutcome::Duplicate => Self::Duplicate,
            IngestOutcome::OutOfOrder => Self::OutOfOrder,
            IngestOutcome::Gap { .. } => Self::Gap,
            IngestOutcome::UnknownSymbol(_) => Self::Unknown,
        }
    }
}

// Wednesday 2026-06-03 minute grid (EDT), the corpus workhorse.
const WED_10_00: i64 = 1780495200000;
const WED_10_01: i64 = 1780495260000;
const WED_10_03: i64 = 1780495380000;
const WED_10_05: i64 = 1780495500000;
// Friday 2026-06-05 15:59 EDT to Monday 2026-06-08 09:30 EDT.
const FRI_CLOSE: i64 = 1780689540000;
const MON_OPEN: i64 = 1780925400000;
// Spring forward: Friday EST to Monday EDT.
const DST_SPRING_FRI: i64 = 1772830740000;
const DST_SPRING_MON: i64 = 1773063000000;
// Fall back: Friday EDT to Monday EST.
const DST_FALL_FRI: i64 = 1793390340000;
const DST_FALL_MON: i64 = 1793629800000;
// Half day: Friday 2026-11-27 12:59 EST to Monday 09:30 EST.
const HALF_FRI: i64 = 1795802340000;
const HALF_MON: i64 = 1796049000000;
// Thanksgiving: Wednesday 15:59 EST to Friday 09:30 EST, Thursday closed.
const HOL_WED: i64 = 1795640340000;
const HOL_FRI: i64 = 1795789800000;

fn verdict_fixtures() -> Vec<VerdictFixture> {
    let pair = |last: i64, first: i64| vec![last, first];
    vec![
        VerdictFixture {
            name: "exact",
            stamps: pair(WED_10_00, WED_10_01),
            expected: vec![Verdict::Appended, Verdict::Appended],
        },
        VerdictFixture {
            name: "duplicates",
            stamps: pair(WED_10_00, WED_10_00),
            expected: vec![Verdict::Appended, Verdict::Duplicate],
        },
        VerdictFixture {
            name: "out_of_order",
            stamps: vec![WED_10_00, WED_10_01, WED_10_00],
            expected: vec![Verdict::Appended, Verdict::Appended, Verdict::OutOfOrder],
        },
        VerdictFixture {
            name: "no_tick_gap",
            stamps: pair(WED_10_00, WED_10_05),
            expected: vec![Verdict::Appended, Verdict::Gap],
        },
        VerdictFixture {
            name: "dropped_tick",
            stamps: vec![WED_10_00, WED_10_01, WED_10_03],
            expected: vec![Verdict::Appended, Verdict::Appended, Verdict::Gap],
        },
        VerdictFixture {
            name: "session_weekend",
            stamps: pair(FRI_CLOSE, MON_OPEN),
            expected: vec![Verdict::Appended, Verdict::Appended],
        },
        VerdictFixture {
            name: "dst_spring",
            stamps: pair(DST_SPRING_FRI, DST_SPRING_MON),
            expected: vec![Verdict::Appended, Verdict::Appended],
        },
        VerdictFixture {
            name: "dst_fall",
            stamps: pair(DST_FALL_FRI, DST_FALL_MON),
            expected: vec![Verdict::Appended, Verdict::Appended],
        },
        VerdictFixture {
            name: "half_day",
            stamps: pair(HALF_FRI, HALF_MON),
            expected: vec![Verdict::Appended, Verdict::Appended],
        },
        VerdictFixture {
            name: "holiday",
            stamps: pair(HOL_WED, HOL_FRI),
            expected: vec![Verdict::Appended, Verdict::Appended],
        },
        VerdictFixture {
            name: "genuine_tear",
            stamps: pair(WED_10_00, WED_10_03),
            expected: vec![Verdict::Appended, Verdict::Gap],
        },
    ]
}

fn fixture_service(
    tape: &Arc<MarketTape>,
    permit: &StoreStorage,
    caps: &CapabilitySet,
) -> (TapeKey, IngestService) {
    let symbol = fluent_wvr::CURRENT_CAPS.sync_scope(caps.clone(), || {
        tape.register(
            "SPY",
            "test",
            AssetClass::Stock,
            PriceScale::new().build(),
            "America/New_York",
            Timeframe::M1,
        )
        .expect("registered")
    });
    let key = TapeKey {
        symbol_id: symbol,
        timeframe: Timeframe::M1,
    };
    let service = IngestService::new(
        config(),
        Arc::clone(tape),
        permit.clone(),
        FeedPump::new(),
        vec![(
            symbol,
            Arc::new(Provider {
                key,
                bars: Vec::new(),
            }) as Arc<dyn HistoryProvider>,
        )],
        equities_calendar(),
    )
    .expect("service");
    (key, service)
}

/// The gap-verdict corpus: every consecutive pair carries the
/// session-aware verdict — gap only when trading bars were actually due.
/// Closed controls append quietly with no suspension and no open gaps; the
/// three genuine holes fire exactly once each. Both verdict precision and
/// recall measure 1.0, and controls produce zero repairs.
#[test]
fn session_aware_gap_corpus_measures_perfect_verdict_precision() {
    let mut true_positives = 0u64;
    let mut false_positives = 0u64;
    let mut false_negatives = 0u64;
    let mut gap_positives = 0u64;
    let mut gap_expected = 0u64;
    for fixture in verdict_fixtures() {
        let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
        let permit = StoreStorage::new(tape.root().to_path_buf());
        let caps = CapabilitySet::new().with(permit.clone());
        let (_key, service) = fixture_service(&tape, &permit, &caps);
        let outcomes: Vec<Verdict> = fluent_wvr::CURRENT_CAPS.sync_scope(caps, || {
            fixture
                .stamps
                .iter()
                .map(|ts| service.handle_event(&closed_candle(bar_at(*ts, "100"))))
                .map(|outcome| Verdict::of(&outcome))
                .collect()
        });
        assert_eq!(
            outcomes, fixture.expected,
            "{}: verdicts fire only on genuine holes",
            fixture.name
        );
        let positive = fixture.expected.contains(&Verdict::Gap);
        let fired = outcomes.contains(&Verdict::Gap);
        match (positive, fired) {
            (true, true) => {
                true_positives += 1;
                gap_positives += 1;
                gap_expected += 1;
            }
            (true, false) => false_negatives += 1,
            (false, true) => {
                false_positives += 1;
                gap_positives += 1;
            }
            (false, false) => {
                assert_eq!(
                    service.health().open_gaps,
                    0,
                    "{}: a control leaves no open gap",
                    fixture.name
                );
                assert!(
                    !service.backfill_handle().entries_suspended(),
                    "{}: a control suspends nothing",
                    fixture.name
                );
            }
        }
    }
    // Non-bar events never write: forming updates, raw prints, and book
    // deltas are display-only.
    {
        let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
        let permit = StoreStorage::new(tape.root().to_path_buf());
        let caps = CapabilitySet::new().with(permit.clone());
        let (_key, service) = fixture_service(&tape, &permit, &caps);
        fluent_wvr::CURRENT_CAPS.sync_scope(caps, || {
            assert_eq!(
                service.handle_event(&event(bar_at(WED_10_00, "100"), false)),
                IngestOutcome::Ignored
            );
            assert_eq!(
                service.handle_event(&MarketEvent::TradePrint {
                    underlying: "SPY".to_owned(),
                    print: TradePrint {
                        ts: stamp(0),
                        price: money("100"),
                        size: 1,
                        aggressor: None,
                    },
                }),
                IngestOutcome::Ignored
            );
            assert_eq!(
                service.handle_event(&MarketEvent::BookDelta {
                    underlying: "SPY".to_owned(),
                    sequence: 1,
                }),
                IngestOutcome::Ignored
            );
            assert!(
                matches!(
                    service.handle_event(&MarketEvent::Candle {
                        underlying: "OTHER".to_owned(),
                        bar: bar_at(WED_10_00, "100"),
                    }),
                    IngestOutcome::UnknownSymbol(_)
                ),
                "an unregistered symbol is named, never stored"
            );
        });
    }
    assert!(
        false_positives == 0 && true_positives > 0,
        "verdict precision is 1.0: no false gap, got {false_positives} false positives"
    );
    assert!(
        false_negatives == 0 && true_positives > 0,
        "verdict recall is 1.0: no missed hole, got {false_negatives} false negatives"
    );
    assert_eq!(
        gap_positives, gap_expected,
        "gap verdicts fire exactly on the genuine holes"
    );
    assert!(gap_expected == 3, "the corpus holds three genuine holes");
}

/// The frozen exchange-session interpretation: a skipped span is a gap if
/// and only if trading bars were due inside it. The corpus asserts the full
/// truth table; this pins the exact gap coordinates on a genuine hole and
/// the quiet append on a weekend crossing from its own Friday watermark.
#[test]
fn gap_coordinates_name_the_due_span_exactly() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    let (_key, service) = fixture_service(&tape, &permit, &caps);
    fluent_wvr::CURRENT_CAPS.sync_scope(caps.clone(), || {
        assert_eq!(
            service.handle_event(&closed_candle(bar_at(WED_10_00, "100"))),
            IngestOutcome::Appended
        );
        // Four silent minute buckets: the verdict names exactly the due span.
        assert_eq!(
            service.handle_event(&closed_candle(bar_at(WED_10_05, "101"))),
            IngestOutcome::Gap {
                symbol: "SPY".to_owned(),
                from_ms: WED_10_01,
                to_ms: WED_10_05,
            }
        );
    });
    // A weekend crossing from its own Friday watermark appends quietly: the
    // suspended service above keeps its Wednesday watermark, so the crossing
    // needs a service that actually saw the Friday close.
    let friday = Arc::new(MarketTape::open_in_memory().expect("store"));
    let friday_permit = StoreStorage::new(friday.root().to_path_buf());
    let friday_caps = CapabilitySet::new().with(friday_permit.clone());
    let (_key, friday_service) = fixture_service(&friday, &friday_permit, &friday_caps);
    fluent_wvr::CURRENT_CAPS.sync_scope(friday_caps, || {
        assert_eq!(
            friday_service.handle_event(&closed_candle(bar_at(FRI_CLOSE, "102"))),
            IngestOutcome::Appended
        );
        assert_eq!(
            friday_service.handle_event(&closed_candle(bar_at(MON_OPEN, "103"))),
            IngestOutcome::Appended
        );
        assert!(
            !friday_service.backfill_handle().entries_suspended(),
            "the weekend crossing suspends nothing"
        );
    });
}

/// A weekend crossing through the supervised feed fires no gap, suspends
/// nothing, and survives a reconnect in the middle: the splice is a
/// continuation, not a restart. No repair path runs, so the stamps stay
/// fixed and the test is deterministic.
#[tokio::test]
async fn weekend_crossing_through_supervised_feed_fires_no_gap() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps.clone(), async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let feed = FeedPump::new();
            let service = Arc::new(
                IngestService::new(
                    config(),
                    Arc::clone(&tape),
                    permit.clone(),
                    Arc::clone(&feed) as Arc<dyn MarketDataFeed>,
                    vec![(
                        symbol,
                        Arc::new(Provider {
                            key,
                            bars: Vec::new(),
                        }) as Arc<dyn HistoryProvider>,
                    )],
                    equities_calendar(),
                )
                .expect("service"),
            );
            let session = service.subscribe().await.expect("session");
            let abort = session.abort.clone();
            let running = Arc::clone(&service);
            let run = tokio::spawn(
                CURRENT_CAPS.scope(caps.clone(), async move { running.run(session).await }),
            );
            feed.push(&closed_candle(bar_at(FRI_CLOSE, "100")));
            poll_until("the Friday close lands", 2_000, || {
                tape.coverage(key)
                    .ok()
                    .flatten()
                    .is_some_and(|coverage| coverage.bars == 1)
            })
            .await;
            abort.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("the run stops")
                .expect("the run joins")
                .expect("abort is a clean exit");
            // Reconnect over the weekend: the Monday open appends quietly.
            let session = service.subscribe().await.expect("resubscribe");
            let abort = session.abort.clone();
            let running = Arc::clone(&service);
            let run = tokio::spawn(
                CURRENT_CAPS.scope(caps.clone(), async move { running.run(session).await }),
            );
            feed.push(&closed_candle(bar_at(MON_OPEN, "101")));
            feed.push(&closed_candle(bar_at(MON_OPEN + 60_000, "102")));
            poll_until("the Monday open lands", 2_000, || {
                tape.coverage(key)
                    .ok()
                    .flatten()
                    .is_some_and(|coverage| coverage.bars == 3)
            })
            .await;
            assert!(
                !service.backfill_handle().entries_suspended(),
                "the weekend crossing suspends nothing"
            );
            assert_eq!(
                service.health().open_gaps,
                0,
                "the weekend crossing opens no gap"
            );
            abort.cancel();
            tokio::time::timeout(std::time::Duration::from_secs(2), run)
                .await
                .expect("the second run stops")
                .expect("the run joins")
                .expect("abort is a clean exit");
        })
        .await;
}

/// Repeated reconnect/repair is idempotent: redelivered bars add no rows,
/// re-running repair over a filled span inserts nothing, and no false gap
/// ever fires. Fixed in-session stamps keep the verdicts deterministic at
/// any wall-clock hour; the supervised loop observes every event while the
/// explicit repair step carries a controlled clock (the loop's own
/// real-`now` attempt degrades to a harmless tail probe on an old tape).
#[ignore = "soak: three supervised reconnect rounds plus an explicit re-repair; per-round rules are pinned by smoke tests every run"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn soak_repeated_reconnect_repair_is_idempotent() {
    let tape = Arc::new(MarketTape::open_in_memory().expect("store"));
    let permit = StoreStorage::new(tape.root().to_path_buf());
    let caps = CapabilitySet::new().with(permit.clone());
    CURRENT_CAPS
        .scope(caps.clone(), async {
            let symbol = tape
                .register(
                    "SPY",
                    "test",
                    AssetClass::Stock,
                    PriceScale::new().build(),
                    "America/New_York",
                    Timeframe::M1,
                )
                .expect("registered");
            let key = TapeKey {
                symbol_id: symbol,
                timeframe: Timeframe::M1,
            };
            let provider = Arc::new(Provider {
                key,
                bars: vec![
                    bar_at(WED_10_00, "100"),
                    bar_at(WED_10_01, "101"),
                    bar_at(WED_10_01 + 60_000, "102"),
                    bar_at(WED_10_03, "103"),
                ],
            });
            let feed = FeedPump::new();
            let service = Arc::new(
                IngestService::new(
                    config(),
                    Arc::clone(&tape),
                    permit.clone(),
                    Arc::clone(&feed) as Arc<dyn MarketDataFeed>,
                    vec![(symbol, provider)],
                    equities_calendar(),
                )
                .expect("service"),
            );
            // Three rounds: contiguous pair, redelivered duplicate, then a
            // one-bar hole the supervised loop observes and the controlled
            // repair fills. Every replay after the first round is a refusal,
            // never a row.
            for round in 1..=3u64 {
                let session = service.subscribe().await.expect("session");
                let abort = session.abort.clone();
                let running = Arc::clone(&service);
                let run = tokio::spawn(
                    CURRENT_CAPS.scope(caps.clone(), async move { running.run(session).await }),
                );
                feed.push(&closed_candle(bar_at(WED_10_00, "100")));
                feed.push(&closed_candle(bar_at(WED_10_01, "101")));
                feed.push(&closed_candle(bar_at(WED_10_01, "101")));
                feed.push(&closed_candle(bar_at(WED_10_03, "102")));
                // Only the first round observes a hole: replays are refusals.
                if round == 1 {
                    poll_until("the hole is observed", 5_000, || {
                        service.backfill_handle().entries_suspended()
                    })
                    .await;
                    service
                        .repair_gap("SPY", WED_10_01 + 60_000, WED_10_03, WED_10_03 + 60_000)
                        .await
                        .expect("controlled repair");
                }
                poll_until("the round settles contiguous", 5_000, || {
                    tape.coverage(key)
                        .ok()
                        .flatten()
                        .is_some_and(|coverage| coverage.bars == 4)
                        && service.health().conflicts == round
                        && !service.backfill_handle().entries_suspended()
                })
                .await;
                abort.cancel();
                tokio::time::timeout(std::time::Duration::from_secs(2), run)
                    .await
                    .expect("the round stops")
                    .expect("the run joins")
                    .expect("abort is a clean exit");
            }
            // Re-running repair over the filled span inserts nothing: the
            // verdict is already complete and the tape is untouched.
            let before = tape.coverage(key).expect("coverage").expect("some");
            let complete = service
                .repair_gap("SPY", WED_10_01 + 60_000, WED_10_03, WED_10_03 + 60_000)
                .await
                .expect("re-repair");
            assert!(complete, "the filled span verifies complete");
            let after = tape.coverage(key).expect("coverage").expect("some");
            assert_eq!(before.bars, after.bars, "re-repair writes no rows");
            assert_eq!(after.bars, 4, "replays never double-wrote");
            assert_eq!(service.health().open_gaps, 0, "no false gap survives");
            assert!(
                service
                    .health()
                    .last_repair
                    .is_some_and(|report| report.contains("complete=true")),
                "the re-repair records its clean verdict"
            );
        })
        .await;
}
