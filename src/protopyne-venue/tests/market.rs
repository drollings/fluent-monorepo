use crate::journal::VenueStorage;
use crate::market::{MarketEvent, MarketEventKind};
use crate::order::{Fill, OrderSide};
use crate::paper::{PaperConfig, PaperVenue};
use crate::quote::Quote;
use crate::registry::VenueRegistry;
use crate::session::{SessionConfig, TradingSession};
use crate::venue::{QuoteSource, Venue};
use chrono::{TimeZone, Utc};
use protopyne_types::{Bar, Money, SymbolIndex};
use rust_decimal::Decimal;
use std::sync::Arc;

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn quote() -> Quote {
    Quote::new()
        .bid(money("600.00"))
        .ask(money("600.10"))
        .bid_size(50)
        .ask_size(50)
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap())
        .build()
}

fn bar() -> Bar {
    Bar::new()
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap())
        .o("600.00".parse().expect("decimal"))
        .h("601.00".parse().expect("decimal"))
        .l("599.00".parse().expect("decimal"))
        .c("600.50".parse().expect("decimal"))
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

fn fill() -> Fill {
    Fill::new()
        .quantity(2)
        .price(money("600.05"))
        .fee(money("1.30"))
        .build()
}

fn events() -> Vec<MarketEvent> {
    vec![
        MarketEvent::Quote { underlying: "SPY".to_owned(), quote: quote() },
        MarketEvent::Trade { underlying: "SPY".to_owned(), fill: fill() },
        MarketEvent::BookDelta { underlying: "AAPL".to_owned(), sequence: 7 },
        MarketEvent::Candle { underlying: "SPY".to_owned(), bar: bar() },
        MarketEvent::Quote { underlying: "AAPL".to_owned(), quote: quote() },
    ]
}

fn session() -> TradingSession {
    let venue: Arc<dyn Venue> = Arc::new(PaperVenue::new(PaperConfig::new().build()));
    TradingSession::boot(
        &SessionConfig::new().build(),
        Arc::new(VenueRegistry::single(venue)),
        VenueStorage::new("test.db".into()),
    )
    .expect("boot")
}

#[test]
fn event_routing_header() {
    let quote = MarketEvent::Quote { underlying: "SPY".to_owned(), quote: quote() };
    assert_eq!(quote.symbol(), "SPY");
    assert_eq!(quote.kind(), MarketEventKind::Quote);
    assert!(quote.matches(Some("SPY"), Some(MarketEventKind::Quote)));
    assert!(!quote.matches(Some("AAPL"), None));
    assert!(!quote.matches(None, Some(MarketEventKind::Candle)));
    assert!(quote.matches(None, None));
    // Receiver-side indexed filtering: one interning per symbol, then
    // `Copy`-key comparison.
    let mut index = SymbolIndex::new();
    let spy = index.intern("SPY").expect("intern SPY");
    let key = quote.symbol_key(&mut index).expect("key SPY");
    assert_eq!(key, spy);
}

#[test]
fn payloads_reuse_existing_vocabulary() {
    // No new price/fill/candle types: the payloads are the venue/types
    // vocabulary by construction.
    let trade = MarketEvent::Trade { underlying: "SPY".to_owned(), fill: fill() };
    match trade {
        MarketEvent::Trade { fill, .. } => assert_eq!(fill.quantity, 2),
        _ => panic!("wrong variant"),
    }
    let candle = MarketEvent::Candle { underlying: "SPY".to_owned(), bar: bar() };
    match candle {
        MarketEvent::Candle { bar, .. } => assert_eq!(bar.volume, 1_000),
        _ => panic!("wrong variant"),
    }
}

#[tokio::test]
async fn one_pump_fans_out_to_filtered_receivers() {
    let mut session = session();
    let feed = session.spawn_event_feed(events(), 16);
    let mut spy_quotes = feed.subscribe();
    let mut candles = feed.subscribe();
    drop(feed);

    let mut got_quotes = Vec::new();
    while let Ok(event) = spy_quotes.recv().await {
        if event.matches(Some("SPY"), Some(MarketEventKind::Quote)) {
            got_quotes.push(event);
        }
        if got_quotes.len() == 1 {
            break;
        }
    }
    assert_eq!(got_quotes.len(), 1);
    assert_eq!(got_quotes[0].symbol(), "SPY");

    let mut got_candles = Vec::new();
    while let Ok(event) = candles.recv().await {
        if event.matches(None, Some(MarketEventKind::Candle)) {
            got_candles.push(event);
        }
        if got_candles.len() == 1 {
            break;
        }
    }
    assert_eq!(got_candles.len(), 1);
    session.shutdown().await;
}

#[tokio::test]
async fn shutdown_drains_the_pump() {
    let mut session = session();
    let feed = session.spawn_event_feed(events(), 16);
    let mut rx = feed.subscribe();
    let mut count = 0;
    // Five events published; the pump then ends and the sender is dropped
    // with the session, so the stream closes instead of hanging.
    loop {
        match rx.recv().await {
            Ok(_) => count += 1,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
        }
        if count >= 5 {
            break;
        }
    }
    assert_eq!(count, 5);
    session.shutdown().await;
}

struct FixedSource {
    name: String,
    quote: Option<Quote>,
}

impl QuoteSource for FixedSource {
    fn latest(&self, _underlying: &str) -> Option<Quote> {
        self.quote
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[tokio::test]
async fn ladder_falls_back_and_fails_closed() {
    let dark: Arc<dyn QuoteSource> = Arc::new(FixedSource {
        name: "primary".to_owned(),
        quote: None,
    });
    let lit: Arc<dyn QuoteSource> = Arc::new(FixedSource {
        name: "fallback".to_owned(),
        quote: Some(quote()),
    });
    let (served, rung) =
        TradingSession::resolve_quote(vec![dark.clone(), lit.clone()], "SPY")
            .await
            .expect("fallback serves");
    assert_eq!(served, quote());
    assert_eq!(rung, "fallback");
    let lit_primary: Arc<dyn QuoteSource> = Arc::new(FixedSource {
        name: "primary".to_owned(),
        quote: Some(quote()),
    });
    let first_wins =
        TradingSession::resolve_quote(vec![lit_primary.clone(), dark.clone()], "SPY").await;
    assert_eq!(
        first_wins.map(|(_, name)| name),
        Some("primary".to_owned()),
        "first rung with a quote wins"
    );
    assert!(
        TradingSession::resolve_quote(vec![dark], "SPY").await.is_none(),
        "every feed dark means no quote"
    );
}

#[test]
fn lagged_receiver_skips_and_continues() {
    // The policy every broadcast fan-out in this crate relies on
    // (rehearsal feed, session event pump): a slow receiver observes
    // `Lagged` with the skipped count and keeps receiving — the pump never
    // blocks and slow consumers never wedge it. Pinned here so a Tokio
    // semantic change fails loudly instead of silently dropping.
    let (tx, mut slow) = tokio::sync::broadcast::channel::<String>(1);
    tx.send("a".to_owned()).expect("buffered");
    tx.send("b".to_owned()).expect("buffered");
    tx.send("c".to_owned()).expect("buffered");
    match slow.try_recv() {
        Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
            assert_eq!(skipped, 2, "two frames overwritten in a capacity-one buffer");
        }
        other => panic!("slow receiver must observe lag, got {other:?}"),
    }
    assert_eq!(slow.try_recv().expect("stream continues"), "c");
}

#[tokio::test]
async fn existing_quote_flow_unchanged() {
    // The venue submit path still prices off direct quotes: the new fan-out
    // is additive and the fill below never touches it.
    let venue = PaperVenue::new(PaperConfig::new().build());
    let record = venue
        .submit(
            crate::order::OrderIntent::new()
                .client_id("m#0".to_owned())
                .proposal_id("prop-1".to_owned())
                .strategy("ribbon".to_owned())
                .underlying("SPY".to_owned())
                .side(OrderSide::Buy)
                .quantity(2)
                .limit_price(money("600.50"))
                .build(),
            &quote(),
            Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap(),
        )
        .await
        .expect("fills without the fan-out");
    assert_eq!(record.filled_quantity, 2);
}
