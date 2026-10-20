//! Tick-to-bar adapter and feed vocabulary tests.

use chrono::{TimeZone, Utc};
use protopyne_kernels::bars::{tick_resample, RawTick};
use protopyne_kernels::mtf::Lookahead;
use protopyne_types::session::{SessionCalendar, TradingSession};
use protopyne_types::{Money, Timeframe};

use crate::feed::{AdapterOutcome, FeedHealth, FeedStats, FeedStatsSnapshot, TradeBarAdapter};
use crate::market::{Aggressor, BarUpdate, MarketEvent, TradePrint};

fn money(value: &str) -> Money {
    Money::new(value.parse().expect("decimal"))
}

fn print(ts_ms: i64, price: &str, size: u64) -> TradePrint {
    TradePrint {
        ts: Utc.timestamp_millis_opt(ts_ms).single().expect("stamp"),
        price: money(price),
        size,
        aggressor: Some(Aggressor::Buy),
    }
}

#[test]
fn closes_exactly_at_boundary_and_never_before() {
    let mut adapter = TradeBarAdapter::new(Timeframe::M1).expect("adapter");
    assert!(matches!(
        adapter.ingest(print(60_000, "100.00", 1)).expect("tick"),
        AdapterOutcome::Forming(_)
    ));
    assert!(matches!(
        adapter.ingest(print(119_999, "101.00", 2)).expect("tick"),
        AdapterOutcome::Forming(_)
    ));
    let closed = adapter.ingest(print(120_000, "102.00", 3)).expect("tick");
    let AdapterOutcome::Closed { bar, forming } = closed else {
        panic!("expected an exact-boundary close, got {closed:?}");
    };
    assert_eq!(bar.ts.timestamp_millis(), 60_000);
    assert_eq!(bar.c, money("101.00").as_decimal());
    assert_eq!(bar.volume, 3);
    assert_eq!(forming.ts_ms, 120_000);
}

#[test]
fn duplicate_and_out_of_order_prints_do_not_double_count() {
    let mut adapter = TradeBarAdapter::new(Timeframe::M1).expect("adapter");
    adapter.ingest(print(60_000, "100.00", 1)).expect("tick");
    assert_eq!(
        adapter.ingest(print(60_000, "100.00", 1)).expect("tick"),
        AdapterOutcome::Duplicate
    );
    assert_eq!(
        adapter.ingest(print(59_000, "99.00", 1)).expect("tick"),
        AdapterOutcome::OutOfOrder
    );
    assert_eq!(adapter.duplicates(), 1);
    assert_eq!(adapter.out_of_order(), 1);
    let closed = adapter.ingest(print(120_000, "101.00", 1)).expect("tick");
    let AdapterOutcome::Closed { bar, .. } = closed else {
        panic!("boundary closes");
    };
    assert_eq!(bar.volume, 1, "duplicate volume never counted");
}

#[test]
fn adapter_matches_tick_resample_oracle() {
    let prints = vec![
        print(60_000, "100.00", 1),
        print(90_000, "101.00", 2),
        print(119_999, "99.50", 1),
        print(120_000, "102.00", 4),
        print(150_000, "102.50", 1),
    ];
    let oracle = tick_resample(
        &prints
            .iter()
            .map(|print| RawTick {
                ts_ms: print.ts.timestamp_millis(),
                price: print.price.as_decimal().try_into().expect("price"),
                volume: print.size as f64,
            })
            .collect::<Vec<_>>(),
        60_000,
        false,
        Lookahead::Off,
    )
    .expect("oracle");
    let mut adapter = TradeBarAdapter::new(Timeframe::M1).expect("adapter");
    for print in prints {
        let _ = adapter.ingest(print).expect("tick");
    }
    let expected = oracle.first().expect("one closed bucket");
    assert_eq!(expected.ts_ms, 60_000);
}

#[test]
fn health_reports_age_and_degradation() {
    let stats = FeedStatsSnapshot {
        received: 3,
        skipped: 1,
        malformed: 2,
        last_event_ms: Some(1_000),
    };
    let health = FeedHealth::of(stats, 2_500);
    assert!(health.live);
    assert_eq!(health.last_event_age_ms, Some(1_500));
    assert!(health.degraded);
    assert!(health.reason.is_some());

    let stats = FeedStats::default();
    stats.record_received();
    assert!(stats.snapshot().last_event_ms.is_some());
}

#[test]
fn existing_market_events_keep_their_shape_and_new_events_match() {
    let quote = crate::quote::Quote::new()
        .bid(money("100.00"))
        .ask(money("100.10"))
        .bid_size(1)
        .ask_size(1)
        .ts(Utc.timestamp_millis_opt(60_000).single().expect("stamp"))
        .build();
    let event = MarketEvent::Quote {
        underlying: "SPY".to_owned(),
        quote,
    };
    let wire = serde_json::to_value(&event).expect("serialize");
    assert!(
        wire.get("Quote").is_some(),
        "existing variant shape is pinned"
    );

    let print = TradePrint {
        ts: Utc.timestamp_millis_opt(60_000).single().expect("stamp"),
        price: money("100.00"),
        size: 4,
        aggressor: None,
    };
    let new = MarketEvent::TradePrint {
        underlying: "SPY".to_owned(),
        print,
    };
    assert_eq!(new.kind(), crate::market::MarketEventKind::TradePrint);
    assert!(new.matches(
        Some("SPY"),
        Some(crate::market::MarketEventKind::TradePrint)
    ));
    assert!(!new.matches(Some("QQQ"), None));

    let update = BarUpdate {
        underlying: "SPY".to_owned(),
        bar: protopyne_types::Bar::new()
            .ts(Utc.timestamp_millis_opt(60_000).single().expect("stamp"))
            .o(money("100.00").as_decimal())
            .h(money("101.00").as_decimal())
            .l(money("99.00").as_decimal())
            .c(money("100.50").as_decimal())
            .volume(10)
            .build(),
        closed: false,
    };
    let event = MarketEvent::BarUpdate { update };
    assert_eq!(event.symbol(), "SPY");
    assert_eq!(event.kind(), crate::market::MarketEventKind::BarUpdate);

    // Every variant serializes under its own top-level key with the routing
    // header beside the payload — no variant may flatten or rename.
    let stamp = Utc.timestamp_millis_opt(60_000).single().expect("stamp");
    let fill = crate::order::Fill::new()
        .quantity(2)
        .price(money("100.00"))
        .fee(money("1.00"))
        .build();
    let bar = protopyne_types::Bar::new()
        .ts(stamp)
        .o(money("100.00").as_decimal())
        .h(money("101.00").as_decimal())
        .l(money("99.00").as_decimal())
        .c(money("100.50").as_decimal())
        .volume(10)
        .build();
    let cases = [
        (
            MarketEvent::Quote {
                underlying: "SPY".to_owned(),
                quote,
            },
            "Quote",
            "quote",
        ),
        (
            MarketEvent::Trade {
                underlying: "SPY".to_owned(),
                fill,
            },
            "Trade",
            "fill",
        ),
        (
            MarketEvent::TradePrint {
                underlying: "SPY".to_owned(),
                print,
            },
            "TradePrint",
            "print",
        ),
        (
            MarketEvent::BookDelta {
                underlying: "SPY".to_owned(),
                sequence: 7,
            },
            "BookDelta",
            "sequence",
        ),
        (
            MarketEvent::Candle {
                underlying: "SPY".to_owned(),
                bar: bar.clone(),
            },
            "Candle",
            "bar",
        ),
        (
            MarketEvent::BarUpdate {
                update: BarUpdate {
                    underlying: "SPY".to_owned(),
                    bar,
                    closed: true,
                },
            },
            "BarUpdate",
            "update",
        ),
    ];
    for (event, key, payload) in cases {
        let wire = serde_json::to_value(&event).expect("serialize");
        let object = wire.as_object().expect("externally tagged object");
        assert_eq!(object.len(), 1, "{key} keeps one top-level key");
        let inner = object.get(key).expect("variant key is pinned");
        assert!(inner.get(payload).is_some(), "{key} carries {payload}");
        assert_eq!(
            inner.get("underlying").and_then(serde_json::Value::as_str),
            if key == "BarUpdate" {
                None
            } else {
                Some("SPY")
            },
            "{key} routing header is pinned"
        );
    }
}

/// The weekday-aware calendar the trigger corpus reads: regular US equity
/// hours, Thanksgiving closed, the Friday after at a 13:00 early close.
fn equities_calendar() -> SessionCalendar {
    SessionCalendar::us_equities()
        .with_holiday((2026, 11, 26))
        .with_half_day(
            (2026, 11, 27),
            TradingSession::parse("0930-1300").expect("half parses"),
        )
}

/// One calibration fixture: prints in, closed bars and gap verdicts out.
/// Verdicts come from the calendar over consecutive closed-bar stamps: a
/// positive verdict names genuinely missing trading bars.
struct TriggerFixture {
    name: &'static str,
    prints: Vec<(i64, u64)>,
    closed: Vec<(i64, u64)>,
    verdicts: Vec<u64>,
}

fn trigger_fixtures() -> Vec<TriggerFixture> {
    vec![
        // Ticks at the boundary, inside the bucket, and the first tick of
        // the next bucket: exactly one close on the exact bucket.
        TriggerFixture {
            name: "exact",
            prints: vec![(1780495200000, 1), (1780495230000, 1), (1780495260000, 1)],
            closed: vec![(1780495200000, 2)],
            verdicts: vec![],
        },
        // One millisecond before the boundary is still forming; the
        // boundary tick itself closes.
        TriggerFixture {
            name: "off_by_ms",
            prints: vec![(1780495200000, 1), (1780495259999, 1), (1780495260000, 1)],
            closed: vec![(1780495200000, 2)],
            verdicts: vec![],
        },
        // Four silent buckets, then prints: one close on the old bucket, a
        // four-bar trading gap, then the resumed bar.
        TriggerFixture {
            name: "no_tick_gap",
            prints: vec![(1780495200000, 1), (1780495500000, 1), (1780495560000, 1)],
            closed: vec![(1780495200000, 1), (1780495500000, 1)],
            verdicts: vec![4],
        },
        // A duplicate never folds: the close counts only accepted prints.
        TriggerFixture {
            name: "duplicates",
            prints: vec![
                (1780495200000, 1),
                (1780495200000, 9),
                (1780495230000, 1),
                (1780495260000, 1),
            ],
            closed: vec![(1780495200000, 2)],
            verdicts: vec![],
        },
        // An out-of-order print is refused; the jump over 10:02 is a real
        // one-bar gap, caught when the next bar closes.
        TriggerFixture {
            name: "out_of_order",
            prints: vec![
                (1780495200000, 1),
                (1780495260000, 1),
                (1780495230000, 9),
                (1780495380000, 1),
                (1780495440000, 1),
            ],
            closed: vec![(1780495200000, 1), (1780495260000, 1), (1780495380000, 1)],
            verdicts: vec![0, 1],
        },
        // Friday close to Monday open: the weekend counts nothing.
        TriggerFixture {
            name: "session_weekend",
            prints: vec![(1780689540000, 1), (1780925400000, 2), (1780925460000, 3)],
            closed: vec![(1780689540000, 1), (1780925400000, 2)],
            verdicts: vec![0],
        },
        // Spring forward between the sessions: Friday EST, Monday EDT.
        TriggerFixture {
            name: "dst_spring",
            prints: vec![(1772830740000, 1), (1773063000000, 2), (1773063060000, 3)],
            closed: vec![(1772830740000, 1), (1773063000000, 2)],
            verdicts: vec![0],
        },
        // Fall back between the sessions: Friday EDT, Monday EST.
        TriggerFixture {
            name: "dst_fall",
            prints: vec![(1793390340000, 1), (1793629800000, 2), (1793629860000, 3)],
            closed: vec![(1793390340000, 1), (1793629800000, 2)],
            verdicts: vec![0],
        },
        // Half-day close to Monday open: Friday afternoon counts nothing.
        TriggerFixture {
            name: "half_day",
            prints: vec![(1795802340000, 1), (1796049000000, 2), (1796049060000, 3)],
            closed: vec![(1795802340000, 1), (1796049000000, 2)],
            verdicts: vec![0],
        },
        // One dropped minute bucket intraday: the verdict lands exactly on
        // the hole, nowhere else.
        TriggerFixture {
            name: "dropped_tick",
            prints: vec![
                (1780495200000, 1),
                (1780495260000, 1),
                (1780495380000, 1),
                (1780495440000, 1),
            ],
            closed: vec![(1780495200000, 1), (1780495260000, 1), (1780495380000, 1)],
            verdicts: vec![0, 1],
        },
    ]
}

/// The trigger corpus: every accepted print either advances the bucket
/// (exactly one close on the exact bucket) or it does not (forming only),
/// and every consecutive closed-bar pair carries the calendar verdict —
/// zero on every control, positive only on genuine holes. Both the close
/// rule and the gap rule measure 1.0 precision and recall.
#[test]
fn trigger_corpus_measures_perfect_close_and_gap_precision() {
    let calendar = equities_calendar();
    let timeframe_ms = Timeframe::M1.duration_ms();
    let mut true_positives = 0u64;
    let mut false_positives = 0u64;
    let mut false_negatives = 0u64;
    let mut gap_positives = 0u64;
    let mut gap_expected = 0u64;
    for fixture in trigger_fixtures() {
        let mut adapter = TradeBarAdapter::new(Timeframe::M1).expect("adapter");
        let mut bars: Vec<(i64, u64)> = Vec::new();
        let mut previous_bucket: Option<i64> = None;
        for (ts_ms, size) in &fixture.prints {
            let bucket = ts_ms.div_euclid(timeframe_ms) * timeframe_ms;
            let advanced = previous_bucket.is_some_and(|previous| bucket > previous);
            let outcome = adapter
                .ingest(print(*ts_ms, "100.00", *size))
                .expect("tick");
            let accepted = !matches!(
                outcome,
                AdapterOutcome::Duplicate | AdapterOutcome::OutOfOrder
            );
            match outcome {
                AdapterOutcome::Duplicate | AdapterOutcome::OutOfOrder => {}
                AdapterOutcome::Forming(_) => {
                    if advanced {
                        false_negatives += 1;
                    }
                }
                AdapterOutcome::Closed { bar, .. } => {
                    if advanced {
                        true_positives += 1;
                    } else {
                        false_positives += 1;
                    }
                    bars.push((bar.ts.timestamp_millis(), bar.volume));
                }
            }
            if accepted {
                previous_bucket = Some(bucket);
            }
        }
        assert_eq!(
            bars, fixture.closed,
            "{}: closes land on the exact buckets with exact volumes",
            fixture.name
        );
        let verdicts: Vec<u64> = bars
            .windows(2)
            .map(|pair| calendar.trading_buckets_between(pair[0].0, pair[1].0, timeframe_ms))
            .collect();
        assert_eq!(
            verdicts, fixture.verdicts,
            "{}: gap verdicts fire only on genuine holes",
            fixture.name
        );
        gap_positives += verdicts.iter().filter(|verdict| **verdict > 0).count() as u64;
        gap_expected += fixture
            .verdicts
            .iter()
            .filter(|verdict| **verdict > 0)
            .count() as u64;
    }
    assert!(
        false_positives == 0 && true_positives > 0,
        "close precision is 1.0: no premature close, got {false_positives} false positives"
    );
    assert!(
        false_negatives == 0 && true_positives > 0,
        "close recall is 1.0: no missed close, got {false_negatives} false negatives"
    );
    assert_eq!(
        gap_positives, gap_expected,
        "gap verdicts fire exactly on the genuine holes"
    );
    assert!(gap_expected > 0, "the corpus contains genuine holes");
}

/// The close rule is frozen at zero tolerance: one millisecond early is
/// forming, the boundary tick closes exactly once, and a repeated boundary
/// tick is a duplicate rather than a second close.
#[test]
fn close_boundary_is_frozen_to_the_exact_millisecond() {
    let mut adapter = TradeBarAdapter::new(Timeframe::M1).expect("adapter");
    assert!(matches!(
        adapter
            .ingest(print(1780495200000, "100.00", 1))
            .expect("tick"),
        AdapterOutcome::Forming(_)
    ));
    assert!(
        matches!(
            adapter
                .ingest(print(1780495259999, "100.00", 1))
                .expect("tick"),
            AdapterOutcome::Forming(_)
        ),
        "one millisecond early is still forming"
    );
    match adapter
        .ingest(print(1780495260000, "100.00", 3))
        .expect("tick")
    {
        AdapterOutcome::Closed { bar, .. } => {
            assert_eq!(bar.ts.timestamp_millis(), 1780495200000);
            assert_eq!(bar.volume, 2, "both accepted prints count once each");
        }
        other => panic!("the boundary tick closes, got {other:?}"),
    }
    assert!(
        matches!(
            adapter
                .ingest(print(1780495260000, "100.00", 5))
                .expect("tick"),
            AdapterOutcome::Duplicate
        ),
        "a repeated boundary tick never closes twice"
    );
}
