//! Corporate actions against the venue books: expiry at intrinsic,
//! assignment converting an option leg into stock, dividends crediting cash,
//! and splits re-basing option contracts while leaving the book reconcilable.

use super::*;
use std::collections::HashMap;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use crate::order::{CorporateEvent, CorporateEventArgs, OrderIntent, OrderSide, OrderStatus};
use crate::quote::Quote;
use crate::venue::{LegQuoteSource, Venue};
use protopyne_types::{LegSide, Money, OptionContract, OptionKind, SpreadLeg, SpreadLegs};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn at(year: i32, month: u32, day: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 14, 30, 0).unwrap()
}

fn quote(bid: &str, ask: &str) -> Quote {
    Quote::new()
        .bid(money(bid))
        .ask(money(ask))
        .bid_size(50)
        .ask_size(50)
        .ts(now())
        .build()
}

fn contract(strike: &str, kind: OptionKind) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(chrono::NaiveDate::from_ymd_opt(2026, 1, 16).expect("valid date"))
        .strike(money(strike))
        .kind(kind)
        .build()
}

fn vertical(long_strike: &str, short_strike: &str) -> SpreadLegs {
    SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(contract(long_strike, OptionKind::Call))
            .side(LegSide::Long)
            .ratio(1)
            .build(),
        SpreadLeg::new()
            .contract(contract(short_strike, OptionKind::Call))
            .side(LegSide::Short)
            .ratio(1)
            .build(),
    ])
    .expect("vertical must build")
}

fn spread_intent(id: &str, legs: SpreadLegs) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(1)
        .limit_price(money("5.00"))
        .maybe_legs(Some(legs))
        .build()
}

fn stock_intent(id: &str, quantity: u32) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("prop-1".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(OrderSide::Buy)
        .quantity(quantity)
        .limit_price(money("600.50"))
        .build()
}

/// Deterministic per-leg market data: OCC symbol → quote.
struct MapLegQuotes {
    map: HashMap<String, Quote>,
}

impl LegQuoteSource for MapLegQuotes {
    fn leg_quote(&self, contract: &OptionContract) -> Option<Quote> {
        self.map.get(&contract.occ_symbol()).copied()
    }

    /// These stubs model a newest-only feed: the opt-in is explicit, so the
    /// fill prices newest by declaration rather than by a silent default.
    fn newest_ok(&self) -> bool {
        true
    }
}

fn source(quotes: &[(&OptionContract, &str, &str)]) -> MapLegQuotes {
    let mut map = HashMap::new();
    for (contract, bid, ask) in quotes {
        map.insert(contract.occ_symbol(), quote(bid, ask));
    }
    MapLegQuotes { map }
}

fn liquid_vertical() -> MapLegQuotes {
    source(&[
        (&contract("600", OptionKind::Call), "2.50", "2.70"),
        (&contract("610", OptionKind::Call), "1.20", "1.40"),
    ])
}

fn venue_with(source: MapLegQuotes) -> PaperVenue {
    let mut venue = PaperVenue::new(PaperConfig::new().build());
    venue.set_leg_quotes(Arc::new(source));
    venue
}

fn action(event: CorporateEvent) -> CorporateEventArgs {
    CorporateEventArgs::new()
        .underlying("SPY".to_owned())
        .at(at(2026, 6, 1))
        .event(event)
        .build()
}

#[tokio::test]
async fn expiry_closes_legs_at_intrinsic_and_books_realized() {
    let venue = venue_with(liquid_vertical());
    venue
        .submit(
            spread_intent("exp#0", vertical("600", "610")),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect("vertical fills");
    let long = contract("600", OptionKind::Call);
    let outcome = venue
        .apply_corporate_event(
            &CorporateEventArgs::new()
                .underlying("SPY".to_owned())
                .contract(long.clone())
                .at(at(2026, 6, 1))
                .event(CorporateEvent::Expiry)
                .amount(money("4.00"))
                .build(),
        )
        .expect("expiry");
    assert_eq!(outcome.legs.len(), 1);
    assert_eq!(outcome.legs[0].contract, long.contract_id());
    // Bought at the slipped ask 2.7027 with a 0.65 entry fee: the 4.00
    // intrinsic books (4.00 - 2.7027) - 0.65 = 0.6473.
    assert_eq!(outcome.realised, Some(money("0.6473")));
    assert!(!venue.spread_positions().contains_key(&long.occ_symbol()));
    assert_eq!(
        venue.spread_positions().len(),
        1,
        "the other leg is not expired"
    );
}

#[tokio::test]
async fn assignment_keeps_both_legs_reconcilable() {
    let venue = venue_with(liquid_vertical());
    venue
        .submit(
            spread_intent("asg#0", vertical("600", "610")),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect("vertical fills");
    let short = contract("610", OptionKind::Call);
    let outcome = venue
        .apply_corporate_event(
            &CorporateEventArgs::new()
                .underlying("SPY".to_owned())
                .contract(short.clone())
                .at(at(2026, 6, 1))
                .event(CorporateEvent::Assignment)
                .amount(money("5.00"))
                .build(),
        )
        .expect("assignment");
    assert!(outcome.reconcile, "assignment leaves the book for review");
    assert_eq!(outcome.legs.len(), 1, "the option leg is journaled closed");
    let stock = outcome.stock_leg.expect("the generated stock leg");
    assert_eq!(stock.side, OrderSide::Sell);
    assert_eq!(stock.quantity, 100, "standard 100-share deliverable");
    assert_eq!(stock.price, money("610"));
    // The assigned leg is gone, the long leg stays, and the stock leg exists:
    // no phantom and no disappearance.
    assert!(!venue.spread_positions().contains_key(&short.occ_symbol()));
    assert_eq!(venue.spread_positions().len(), 1);
    let positions = venue.positions();
    assert_eq!(positions.len(), 1);
    assert_eq!(positions[0].underlying, "SPY");
    assert_eq!(positions[0].quantity, -100);
    assert_eq!(positions[0].avg_price, money("610"));
}

#[tokio::test]
async fn split_adjusts_option_strike_and_contracts_then_prices_adjusted_fills() {
    let mut venue = venue_with(liquid_vertical());
    venue
        .submit(
            spread_intent("spl#0", vertical("600", "610")),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect("vertical fills");
    let outcome = venue
        .apply_corporate_event(&action(CorporateEvent::Split(2)))
        .expect("split");
    let long = contract("300", OptionKind::Call);
    let short = contract("305", OptionKind::Call);
    let adjusted = venue.spread_positions();
    let long_position = adjusted.get(&long.occ_symbol()).expect("re-based long");
    assert_eq!(long_position.quantity_abs, 2);
    assert_eq!(long_position.price_entry_average, money("1.35135"));
    let short_position = adjusted.get(&short.occ_symbol()).expect("re-based short");
    assert_eq!(short_position.quantity_abs, 2);
    assert_eq!(short_position.price_entry_average, money("0.5994"));
    assert!(!adjusted.contains_key(&contract("600", OptionKind::Call).occ_symbol()));
    assert!(!adjusted.contains_key(&contract("610", OptionKind::Call).occ_symbol()));
    assert!(outcome.affected.contains(&long.contract_id()));
    assert!(outcome.affected.contains(&short.contract_id()));

    // A later fill prices off the adjusted contracts' own quotes and averages
    // into the doubled books.
    venue.set_leg_quotes(Arc::new(source(&[
        (&long, "1.20", "1.40"),
        (&short, "0.70", "0.90"),
    ])));
    venue
        .submit(
            spread_intent("aft#0", vertical("300", "305")),
            &quote("300.00", "300.10"),
            now(),
        )
        .await
        .expect("adjusted vertical fills");
    let executed = venue.executed_legs("aft#0");
    assert_eq!(executed.len(), 2);
    assert_eq!(executed[0].contract, long.contract_id());
    assert_eq!(executed[0].price, money("1.4014"));
    let adjusted = venue.spread_positions();
    let long_position = adjusted.get(&long.occ_symbol()).expect("long");
    assert_eq!(long_position.quantity_abs, 3);
    let expected_long = Money::new(
        (money("1.35135").as_decimal() * Decimal::from(2) + money("1.4014").as_decimal())
            / Decimal::from(3),
    );
    assert_eq!(long_position.price_entry_average, expected_long);
    let short_position = adjusted.get(&short.occ_symbol()).expect("short");
    assert_eq!(short_position.quantity_abs, 3);
    let expected_short = Money::new(
        (money("0.5994").as_decimal() * Decimal::from(2) + money("0.6993").as_decimal())
            / Decimal::from(3),
    );
    assert_eq!(short_position.price_entry_average, expected_short);
}

#[tokio::test]
async fn dividend_books_cash_and_records_the_option_legs() {
    let venue = venue_with(liquid_vertical());
    venue
        .submit(
            stock_intent("stock#0", 2),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect("stock fills");
    venue
        .submit(
            spread_intent("div#0", vertical("600", "610")),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect("vertical fills");
    let outcome = venue
        .apply_corporate_event(
            &CorporateEventArgs::new()
                .underlying("SPY".to_owned())
                .at(at(2026, 6, 1))
                .event(CorporateEvent::Dividend)
                .amount(money("1.50"))
                .build(),
        )
        .expect("dividend");
    assert_eq!(outcome.cash, Some(money("3.00")));
    assert_eq!(venue.booked_dividends().get("SPY"), Some(&money("3.00")));
    assert_eq!(outcome.affected.len(), 2, "both option legs are named");
    assert!(outcome.legs.is_empty(), "a dividend closes no leg");
    assert!(
        outcome.note.contains("option leg"),
        "the leg effect is recorded, never string-only: {}",
        outcome.note
    );
}

#[tokio::test]
async fn expiry_before_the_contract_expiry_refuses() {
    let venue = venue_with(liquid_vertical());
    venue
        .submit(
            spread_intent("early#0", vertical("600", "610")),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect("vertical fills");
    let err = venue
        .apply_corporate_event(
            &CorporateEventArgs::new()
                .underlying("SPY".to_owned())
                .contract(contract("600", OptionKind::Call))
                .at(at(2026, 1, 1))
                .event(CorporateEvent::Expiry)
                .build(),
        )
        .expect_err("expiry before the listing expiry must refuse");
    assert!(err.to_string().contains("before contract expiry"), "{err}");
    assert_eq!(venue.spread_positions().len(), 2);
}

#[tokio::test]
async fn assignment_without_an_identified_contract_refuses() {
    let venue = venue_with(liquid_vertical());
    let err = venue
        .apply_corporate_event(&action(CorporateEvent::Assignment))
        .expect_err("assignment needs the identified contract");
    assert!(err.to_string().contains("identified"), "{err}");
    assert!(matches!(err, VenueError::Invalid(_)));
}

#[tokio::test]
async fn split_keeps_a_submitted_fill_status_terminal() {
    // Guard against the split path re-entering order state: the executed
    // package stays filled while its option legs are re-based.
    let venue = venue_with(liquid_vertical());
    let record = venue
        .submit(
            spread_intent("term#0", vertical("600", "610")),
            &quote("600.00", "600.10"),
            now(),
        )
        .await
        .expect("vertical fills");
    assert_eq!(record.status, OrderStatus::Filled);
    venue
        .apply_corporate_event(&action(CorporateEvent::Split(2)))
        .expect("split");
    assert_eq!(venue.orders()[0].status, OrderStatus::Filled);
}
