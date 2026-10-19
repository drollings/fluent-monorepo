//! Package-price tolerance calibration: a must-fill treatment against a
//! must-NOT-fill control set, sweeping the signed-package tolerance before
//! the default ships.
//!
//! Protocol: the treatment proves a liquid package trades once the limit is
//! widened; the controls prove crossed, sizeless, stale, and far-from-mid
//! packages never fill however the tolerance moves. The frozen tolerance is
//! the smallest sweep step that fills both treatments while every control
//! stays unfilled. Slippage and the package tolerance are fill-model
//! assumptions, never refusal reasons about merit or health.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rust_decimal::Decimal;

use crate::*;
use protopyne_strategy::{spread_economics, LegSelector, PackageSide, SpreadEconomics};
use protopyne_types::{ChainQuote, Money, OptionContract, OptionKind, Qty, SpreadLegs};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn as_of() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 0).unwrap()
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 5).unwrap()
}

fn expiry() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 7, 17).expect("expiry")
}

fn contract(strike: &str) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(expiry())
        .strike(money(strike))
        .kind(OptionKind::Call)
        .build()
}

fn chain_quote(strike: &str, bid: &str, ask: &str) -> ChainQuote {
    ChainQuote::new()
        .contract(contract(strike))
        .bid(money(bid))
        .ask(money(ask))
        .timestamp(as_of())
        .build()
}

/// Canonical liquid selection chain: 600/610 calls, symmetric one-tenth
/// spreads around 3.50 and 2.50.
fn selection_chain() -> Vec<ChainQuote> {
    vec![
        chain_quote("600", "3.499", "3.501"),
        chain_quote("610", "2.499", "2.501"),
    ]
}

/// Price the 600/610 call vertical from the canonical chain.
fn priced_vertical() -> (SpreadLegs, SpreadEconomics) {
    let quotes = selection_chain();
    let legs = LegSelector::new()
        .build()
        .select_legs(
            protopyne_strategy::StructureKind::Vertical,
            30,
            "SPY",
            money("600"),
            as_of().date_naive(),
            &quotes,
        )
        .expect("vertical selects");
    let economics = spread_economics(&legs, &quotes, Qty::new(Decimal::ONE), money("0.65"))
        .expect("package prices");
    (legs, economics)
}

fn quote(bid: &str, ask: &str, size: u64, ts: DateTime<Utc>) -> Quote {
    Quote::new()
        .bid(money(bid))
        .ask(money(ask))
        .bid_size(size)
        .ask_size(size)
        .ts(ts)
        .build()
}

/// Per-leg fill quotes for one package.
type FillSource = Vec<(OptionContract, Quote)>;

/// Deterministic per-leg fill data keyed by OCC symbol.
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

fn venue_with_fills(fills: &[(OptionContract, Quote)]) -> PaperVenue {
    let mut map = HashMap::new();
    for (contract, quote) in fills {
        map.insert(contract.occ_symbol(), *quote);
    }
    let mut venue = PaperVenue::new(PaperConfig::new().build());
    venue.set_leg_quotes(Arc::new(MapLegQuotes { map }));
    venue
}

fn package_intent(
    id: &str,
    legs: &SpreadLegs,
    economics: &SpreadEconomics,
    tolerance_bps: u32,
) -> OrderIntent {
    let side = match economics.side {
        PackageSide::Buy => OrderSide::Buy,
        PackageSide::Sell => OrderSide::Sell,
    };
    OrderIntent::new()
        .client_id(id.to_owned())
        .proposal_id("cal".to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .side(side)
        .quantity(1)
        .limit_price(economics.limit_with_tolerance(tolerance_bps))
        .maybe_legs(Some(legs.clone()))
        .maybe_capital_requirement(Some(economics.capital_requirement))
        .build()
}

async fn fills(
    legs: &SpreadLegs,
    economics: &SpreadEconomics,
    tolerance_bps: u32,
    sources: &[(OptionContract, Quote)],
    id: &str,
) -> bool {
    let venue = venue_with_fills(sources);
    venue
        .submit(
            package_intent(id, legs, economics, tolerance_bps),
            &quote("5", "6", 50, now()),
            now(),
        )
        .await
        .is_ok()
}

#[tokio::test]
async fn treatment_packages_fill_at_the_frozen_tolerance() {
    let (legs, economics) = priced_vertical();
    // One-penny and five-penny half-spreads: mid and one tick away.
    let tight = vec![
        (contract("600"), quote("3.499", "3.501", 50, now())),
        (contract("610"), quote("2.499", "2.501", 50, now())),
    ];
    let one_tick = vec![
        (contract("600"), quote("3.495", "3.505", 50, now())),
        (contract("610"), quote("2.495", "2.505", 50, now())),
    ];
    assert!(
        fills(&legs, &economics, PACKAGE_TOLERANCE_BPS, &tight, "a#0").await,
        "the mid-priced package must fill at the frozen tolerance"
    );
    assert!(
        fills(&legs, &economics, PACKAGE_TOLERANCE_BPS, &one_tick, "b#0").await,
        "the one-tick package must fill at the frozen tolerance"
    );
}

#[tokio::test]
async fn control_packages_never_fill() {
    let (legs, economics) = priced_vertical();
    let controls: Vec<(&str, FillSource)> = vec![
        (
            "crossed leg",
            vec![
                (contract("600"), quote("3.60", "3.50", 50, now())),
                (contract("610"), quote("2.499", "2.501", 50, now())),
            ],
        ),
        (
            "zero size",
            vec![
                (contract("600"), quote("3.499", "3.501", 0, now())),
                (contract("610"), quote("2.499", "2.501", 50, now())),
            ],
        ),
        (
            "stale quotes",
            vec![
                (contract("600"), quote("3.499", "3.501", 50, now())),
                (
                    contract("610"),
                    quote("2.499", "2.501", 50, now() - chrono::Duration::hours(1)),
                ),
            ],
        ),
        (
            "three tolerances from mid",
            vec![
                (contract("600"), quote("3.485", "3.515", 50, now())),
                (contract("610"), quote("2.485", "2.515", 50, now())),
            ],
        ),
    ];
    let mut false_fills = 0_usize;
    for (label, fills_source) in controls {
        if fills(
            &legs,
            &economics,
            PACKAGE_TOLERANCE_BPS,
            &fills_source,
            label,
        )
        .await
        {
            false_fills += 1;
            eprintln!("control falsely filled: {label}");
        }
    }
    assert_eq!(false_fills, 0, "controls must never fill");
}

#[tokio::test]
async fn the_tolerance_sweep_freezes_the_smallest_filling_step() {
    let (legs, economics) = priced_vertical();
    let tight = vec![
        (contract("600"), quote("3.499", "3.501", 50, now())),
        (contract("610"), quote("2.499", "2.501", 50, now())),
    ];
    let one_tick = vec![
        (contract("600"), quote("3.495", "3.505", 50, now())),
        (contract("610"), quote("2.495", "2.505", 50, now())),
    ];
    let mut smallest = None;
    for bps in [0_u32, 25, 50, 100, 150, 200, 250] {
        let tight_fills = fills(&legs, &economics, bps, &tight, "s-tight").await;
        let tick_fills = fills(&legs, &economics, bps, &one_tick, "s-tick").await;
        if tight_fills && tick_fills && smallest.is_none() {
            smallest = Some(bps);
        }
    }
    assert_eq!(
        smallest,
        Some(PACKAGE_TOLERANCE_BPS),
        "the frozen tolerance is the smallest step that fills both treatments"
    );
    assert_eq!(
        PACKAGE_TOLERANCE_BPS, 200,
        "tolerance re-froze; update reviewers"
    );
}
