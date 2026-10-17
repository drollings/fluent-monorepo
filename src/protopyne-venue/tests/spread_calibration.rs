//! Spread fill-policy calibration: the golden set that freezes the per-leg
//! policy constants and the atomicity rule.
//!
//! Two axes, tested separately:
//!
//! * Producer-confidence (validity): crossed, wide, sub-minimum-size, stale,
//!   and missing leg quotes must-NOT-fill. These controls prove the venue
//!   only prices valid markets; widening a cap to force a fill would be
//!   confident about the wrong thing.
//! * Task-value (economics): precision (filled ⇒ should-fill) and recall
//!   (should-fill ⇒ filled) over the labeled set below must both be `1.0`
//!   before any production constant moves. Per-leg slippage stays inside the
//!   frozen bps tolerance, and the net package gates on the intent limit.
//!
//! No verdict is cached anywhere in this module: every case submits through
//! the live `decide_spread` fold, so the freeze measures current behavior,
//! never a stored answer. Refusals must name the offending leg (`leg {index}
//! ({OCC})`) and commit nothing — atomicity means a refused spread leaves no
//! leg behind.

use super::*;
use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;

use crate::venue::{LegQuoteSource, Venue};
use protopyne_store::StoreHealth;
use protopyne_types::{LegSide, Money, OptionContract, OptionKind, SpreadLeg, SpreadLegs};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 5).unwrap()
}

fn leg_quote(bid: &str, ask: &str, size: u64, ts: DateTime<Utc>) -> Quote {
    Quote::new()
        .bid(money(bid))
        .ask(money(ask))
        .bid_size(size)
        .ask_size(size)
        .ts(ts)
        .build()
}

fn fresh(bid: &str, ask: &str) -> Quote {
    leg_quote(bid, ask, 50, now())
}

fn contract(strike: &str, kind: OptionKind) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(chrono::NaiveDate::from_ymd_opt(2026, 1, 16).expect("valid date"))
        .strike(money(strike))
        .kind(kind)
        .build()
}

fn vertical() -> SpreadLegs {
    SpreadLegs::try_new(vec![
        SpreadLeg::new()
            .contract(contract("600", OptionKind::Call))
            .side(LegSide::Long)
            .ratio(1)
            .build(),
        SpreadLeg::new()
            .contract(contract("610", OptionKind::Call))
            .side(LegSide::Short)
            .ratio(1)
            .build(),
    ])
    .expect("vertical must build")
}

/// Deterministic per-leg market data: OCC symbol → quote.
struct MapLegQuotes {
    map: HashMap<String, Quote>,
}

impl LegQuoteSource for MapLegQuotes {
    fn leg_quote(&self, contract: &OptionContract) -> Option<Quote> {
        self.map.get(&contract.occ_symbol()).copied()
    }
}

fn liquid_book() -> MapLegQuotes {
    let legs = vertical();
    let mut map = HashMap::new();
    map.insert(legs.legs()[0].contract.occ_symbol(), fresh("2.50", "2.70"));
    map.insert(legs.legs()[1].contract.occ_symbol(), fresh("1.20", "1.40"));
    MapLegQuotes { map }
}

fn venue_with(source: MapLegQuotes) -> PaperVenue {
    let mut venue = PaperVenue::new(PaperConfig::new().build());
    venue.set_leg_quotes(Arc::new(source));
    venue
}

fn spread_intent(id: &str, side: OrderSide, limit: &str) -> OrderIntent {
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("cal".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(side)
        .quantity(1)
        .limit_price(money(limit))
        .maybe_legs(Some(vertical()))
        .build()
}

/// The single-leg quote `submit` still requires. The spread fold prices
/// legs off the installed source only; this quote must never leak into a
/// leg verdict.
fn dummy_quote() -> Quote {
    fresh("600.00", "600.10")
}

/// One labeled golden case: the spread, its leg book, the intent side and
/// limit, and whether the policy must fill it.
struct Case {
    name: &'static str,
    book: MapLegQuotes,
    side: OrderSide,
    limit: &'static str,
    should_fill: bool,
}

fn must_fill_buy() -> Case {
    // Net 1.5039 debit (2.7027 ask leg over 1.1988 bid leg) within 5.00.
    Case { name: "liquid vertical buys within limit", book: liquid_book(), side: OrderSide::Buy, limit: "5.00", should_fill: true }
}

fn must_fill_tight_limit() -> Case {
    // Same book, limit 1.60 still covers the 1.5039 net: the gate is the
    // net, not per-leg headroom.
    Case { name: "tight but covering limit still fills", book: liquid_book(), side: OrderSide::Buy, limit: "1.60", should_fill: true }
}

fn must_fill_sell() -> Case {
    // Same legs sold: 1.5039 credit clears a 1.00 floor.
    Case { name: "liquid vertical sells above credit floor", book: liquid_book(), side: OrderSide::Sell, limit: "1.00", should_fill: true }
}

fn wide_leg_case() -> Case {
    let legs = vertical();
    let mut map = HashMap::new();
    map.insert(legs.legs()[0].contract.occ_symbol(), fresh("2.50", "2.70"));
    map.insert(legs.legs()[1].contract.occ_symbol(), fresh("1.20", "60.00"));
    Case { name: "wide short leg refuses", book: MapLegQuotes { map }, side: OrderSide::Buy, limit: "5.00", should_fill: false }
}

fn crossed_leg_case() -> Case {
    let legs = vertical();
    let mut map = HashMap::new();
    map.insert(legs.legs()[0].contract.occ_symbol(), fresh("2.70", "2.50"));
    map.insert(legs.legs()[1].contract.occ_symbol(), fresh("1.20", "1.40"));
    Case { name: "crossed long leg refuses", book: MapLegQuotes { map }, side: OrderSide::Buy, limit: "5.00", should_fill: false }
}

fn tiny_size_case() -> Case {
    let legs = vertical();
    let mut map = HashMap::new();
    map.insert(
        legs.legs()[0].contract.occ_symbol(),
        leg_quote("2.50", "2.70", 0, now()),
    );
    map.insert(legs.legs()[1].contract.occ_symbol(), fresh("1.20", "1.40"));
    Case { name: "sub-minimum-size long leg refuses", book: MapLegQuotes { map }, side: OrderSide::Buy, limit: "5.00", should_fill: false }
}

fn stale_leg_case() -> Case {
    let legs = vertical();
    let stale = now() - chrono::Duration::seconds(120);
    let mut map = HashMap::new();
    map.insert(legs.legs()[0].contract.occ_symbol(), fresh("2.50", "2.70"));
    map.insert(
        legs.legs()[1].contract.occ_symbol(),
        leg_quote("1.20", "1.40", 50, stale),
    );
    Case { name: "stale short leg refuses", book: MapLegQuotes { map }, side: OrderSide::Buy, limit: "5.00", should_fill: false }
}

fn missing_leg_case() -> Case {
    let legs = vertical();
    let mut map = HashMap::new();
    map.insert(legs.legs()[0].contract.occ_symbol(), fresh("2.50", "2.70"));
    Case { name: "missing short-leg quote refuses", book: MapLegQuotes { map }, side: OrderSide::Buy, limit: "5.00", should_fill: false }
}

fn net_outside_limit_case() -> Case {
    // Both legs valid, but the 1.5039 net debit exceeds a 1.00 cap: the
    // package refuses rather than repricing a leg to fit.
    Case { name: "net above buy cap refuses", book: liquid_book(), side: OrderSide::Buy, limit: "1.00", should_fill: false }
}

fn golden_set() -> Vec<Case> {
    vec![
        must_fill_buy(),
        must_fill_tight_limit(),
        must_fill_sell(),
        wide_leg_case(),
        crossed_leg_case(),
        tiny_size_case(),
        stale_leg_case(),
        missing_leg_case(),
        net_outside_limit_case(),
    ]
}

async fn submit_case(case: Case, client_id: &str) -> (PaperVenue, Result<OrderRecord, VenueError>) {
    let venue = venue_with(case.book);
    let intent = spread_intent(client_id, case.side, case.limit);
    let outcome = venue.submit(intent, &dummy_quote(), now()).await;
    (venue, outcome)
}

#[test]
fn policy_defaults_are_frozen() {
    // The per-leg economics this golden set measures. Any production move
    // of these constants must first update this freeze and re-earn 1.0/1.0.
    let config = PaperConfig::new().build();
    assert_eq!(
        config.filter.max_spread_width,
        Money::new(Decimal::new(50, 0)),
        "per-leg width cap moves only with a green recalibration"
    );
    assert_eq!(config.filter.min_size, 1, "per-leg size floor moves only with a green recalibration");
    assert_eq!(
        config.filter.max_age_secs, 60,
        "leg staleness bound moves only with a green recalibration"
    );
    assert_eq!(
        config.slippage.bps, DEFAULT_SLIPPAGE_BPS,
        "per-leg slippage tolerance moves only with a green recalibration"
    );
    assert_eq!(DEFAULT_SLIPPAGE_BPS, 10, "bps default itself is pinned");
}

#[tokio::test]
async fn golden_set_precision_and_recall_are_one() {
    let mut filled_and_should = 0_usize;
    let mut filled = 0_usize;
    let mut should = 0_usize;
    for (index, case) in golden_set().into_iter().enumerate() {
        let expect = case.should_fill;
        let name = case.name;
        let (_venue, outcome) = submit_case(case, &format!("cal-{index}")).await;
        let is_filled = outcome.is_ok();
        assert_eq!(is_filled, expect, "golden case diverged: {name}");
        if is_filled {
            filled += 1;
            if expect {
                filled_and_should += 1;
            }
        }
        if expect {
            should += 1;
        }
    }
    // Precision and recall as exact integer fractions: strict float
    // comparison would hide the counts, and these ratios are 1.0 exactly
    // when every verdict matches its label.
    assert_eq!(filled_and_should, filled, "precision: filled implies should-fill");
    assert_eq!(filled_and_should, should, "recall: should-fill implies filled");
    assert!(should > 0 && filled > 0, "the golden set must exercise both verdicts");
}

#[tokio::test]
async fn wide_leg_refusal_names_the_leg_and_commits_nothing() {
    let legs = vertical();
    let occ = legs.legs()[1].contract.occ_symbol();
    let (venue, outcome) = submit_case(wide_leg_case(), "cal-wide").await;
    let reason = match outcome {
        Ok(record) => panic!("wide leg must not fill, got {record:?}"),
        Err(VenueError::Refused(reason)) => reason,
        Err(other) => panic!("expected a refusal with reasons, got {other:?}"),
    };
    assert!(reason.contains("leg 1"), "names the offending leg, got: {reason}");
    assert!(reason.contains(&occ), "names the contract, got: {reason}");
    assert!(venue.query("cal-wide").await.is_none(), "refused spread records nothing");
    assert!(venue.executed_legs("cal-wide").is_empty(), "refused spread banks no legs");
    assert!(venue.spread_positions().is_empty(), "refused spread opens no leg position");
}

#[tokio::test]
async fn crossed_leg_refusal_names_the_leg() {
    let legs = vertical();
    let occ = legs.legs()[0].contract.occ_symbol();
    let (_venue, outcome) = submit_case(crossed_leg_case(), "cal-crossed").await;
    let reason = match outcome {
        Ok(record) => panic!("crossed leg must not fill, got {record:?}"),
        Err(VenueError::Refused(reason)) => reason,
        Err(other) => panic!("expected a refusal with reasons, got {other:?}"),
    };
    assert!(reason.contains("leg 0"), "names the offending leg, got: {reason}");
    assert!(reason.contains(&occ), "names the contract, got: {reason}");
}

#[tokio::test]
async fn tiny_size_leg_refusal_names_the_leg() {
    let (_venue, outcome) = submit_case(tiny_size_case(), "cal-tiny").await;
    let reason = match outcome {
        Ok(record) => panic!("sub-minimum-size leg must not fill, got {record:?}"),
        Err(VenueError::Refused(reason)) => reason,
        Err(other) => panic!("expected a refusal with reasons, got {other:?}"),
    };
    assert!(reason.contains("leg 0"), "names the offending leg, got: {reason}");
}

#[tokio::test]
async fn stale_leg_refusal_agrees_with_the_health_gate() {
    let (venue, outcome) = submit_case(stale_leg_case(), "cal-stale").await;
    let reason = match outcome {
        Ok(record) => panic!("stale leg must not fill, got {record:?}"),
        Err(VenueError::Refused(reason)) => reason,
        Err(other) => panic!("expected a refusal with reasons, got {other:?}"),
    };
    assert!(reason.contains("leg 1"), "names the offending leg, got: {reason}");
    assert!(venue.query("cal-stale").await.is_none(), "stale spread records nothing");
    // The same 120s age trips the platform health gate: the fill policy and
    // `may_trade` refuse the same stale world, neither covering for the other.
    let state = HealthState { quote_age_secs: 120, clock_skew_secs: 0, stream_connected: true, store: StoreHealth::default(), reconciled: true, backfilled: true };
    assert!(
        !may_trade(&state, &HealthLimits::new().build()).may_trade(),
        "health gate halts on the same staleness the leg filter refused"
    );
}

#[tokio::test]
async fn single_marketable_leg_never_fills_alone() {
    // The long leg is liquid and marketable; the short leg has no quote at
    // all. Atomicity means the spread refuses instead of filling one leg.
    let legs = vertical();
    let occ = legs.legs()[1].contract.occ_symbol();
    let (venue, outcome) = submit_case(missing_leg_case(), "cal-partial").await;
    let reason = match outcome {
        Ok(record) => panic!("half-quoted spread must not partially fill, got {record:?}"),
        Err(VenueError::Refused(reason)) => reason,
        Err(other) => panic!("expected a refusal with reasons, got {other:?}"),
    };
    assert!(reason.contains(&occ), "names the unquoted contract, got: {reason}");
    assert!(venue.query("cal-partial").await.is_none(), "no partial record");
    assert!(venue.spread_positions().is_empty(), "no partial leg position");
}

#[tokio::test]
async fn net_outside_limit_refuses_without_repricing() {
    let (venue, outcome) = submit_case(net_outside_limit_case(), "cal-net").await;
    let reason = match outcome {
        Ok(record) => panic!("over-cap net must not fill, got {record:?}"),
        Err(VenueError::Refused(reason)) => reason,
        Err(other) => panic!("expected a refusal with reasons, got {other:?}"),
    };
    assert!(reason.contains("outside limit"), "net gate explains itself, got: {reason}");
    assert!(venue.query("cal-net").await.is_none(), "over-cap spread records nothing");
}
