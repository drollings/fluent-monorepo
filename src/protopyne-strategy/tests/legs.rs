//! Leg selection fixture matrix: exact legs per structure over a pinned
//! two-expiry ladder, plus the refusals that keep selection honest.
//!
//! The selector is pure over its inputs: every case names its snapshot, so a
//! failure prints the policy breach, never a mystery fill.

use super::*;
use chrono::{NaiveDate, TimeZone};
use protopyne_types::{ChainQuote, LegSide, Money, OptionContract, OptionKind};
use rust_decimal::Decimal;

fn strike(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal strike"))
}

fn expiry(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid expiry")
}

fn front_expiry() -> NaiveDate {
    expiry(2026, 7, 17)
}

fn back_expiry() -> NaiveDate {
    expiry(2026, 8, 21)
}

fn as_of() -> NaiveDate {
    expiry(2026, 6, 15)
}

fn contract(strike_value: &str, on: NaiveDate, kind: OptionKind) -> OptionContract {
    OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(on)
        .strike(strike(strike_value))
        .kind(kind)
        .build()
}

fn quote_for(contract: OptionContract, bid: &str, ask: &str) -> ChainQuote {
    ChainQuote::new()
        .contract(contract)
        .bid(strike(bid))
        .ask(strike(ask))
        .timestamp(chrono::Utc::now())
        .build()
}

/// Two-expiry ladder, strikes 590–610 in 5s, liquid on both expiries.
fn ladder() -> Vec<ChainQuote> {
    let mut quotes = Vec::new();
    for on in [front_expiry(), back_expiry()] {
        for strike_value in ["590", "595", "600", "605", "610"] {
            quotes.push(quote_for(
                contract(strike_value, on, OptionKind::Call),
                "2.50",
                "2.70",
            ));
            quotes.push(quote_for(
                contract(strike_value, on, OptionKind::Put),
                "2.40",
                "2.60",
            ));
        }
    }
    quotes
}

fn selector() -> LegSelector {
    LegSelector::new().build()
}

fn touch() -> Money {
    strike("600")
}

#[test]
fn single_needs_no_legs() {
    let refusal = selector()
        .select_legs(
            StructureKind::Single,
            30,
            "SPY",
            touch(),
            as_of(),
            &ladder(),
        )
        .expect_err("single-leg proposals select no legs");
    assert_eq!(refusal, LegSelectRefusal::NotApplicable);
}

#[test]
fn vertical_picks_front_expiry_atm_pair() {
    let legs = selector()
        .select_legs(
            StructureKind::Vertical,
            30,
            "SPY",
            touch(),
            as_of(),
            &ladder(),
        )
        .expect("vertical selects");
    let legs = legs.legs();
    assert_eq!(legs.len(), 2);
    assert_eq!(
        legs[0].contract,
        contract("600", front_expiry(), OptionKind::Call)
    );
    assert_eq!(legs[0].side, LegSide::Long);
    assert_eq!(
        legs[1].contract,
        contract("605", front_expiry(), OptionKind::Call)
    );
    assert_eq!(legs[1].side, LegSide::Short);
}

#[test]
fn calendar_spans_front_and_back_at_one_strike() {
    let legs = selector()
        .select_legs(
            StructureKind::Calendar,
            30,
            "SPY",
            touch(),
            as_of(),
            &ladder(),
        )
        .expect("calendar selects");
    let legs = legs.legs();
    assert_eq!(legs.len(), 2);
    assert_eq!(
        legs[0].contract,
        contract("600", back_expiry(), OptionKind::Call)
    );
    assert_eq!(legs[0].side, LegSide::Long);
    assert_eq!(
        legs[1].contract,
        contract("600", front_expiry(), OptionKind::Call)
    );
    assert_eq!(legs[1].side, LegSide::Short);
}

#[test]
fn condor_pairs_put_and_call_verticals() {
    let legs = selector()
        .select_legs(
            StructureKind::Condor,
            30,
            "SPY",
            touch(),
            as_of(),
            &ladder(),
        )
        .expect("condor selects");
    let legs = legs.legs();
    assert_eq!(legs.len(), 4);
    assert_eq!(
        legs[0].contract,
        contract("590", front_expiry(), OptionKind::Put)
    );
    assert_eq!(legs[0].side, LegSide::Long);
    assert_eq!(
        legs[1].contract,
        contract("595", front_expiry(), OptionKind::Put)
    );
    assert_eq!(legs[1].side, LegSide::Short);
    assert_eq!(
        legs[2].contract,
        contract("600", front_expiry(), OptionKind::Call)
    );
    assert_eq!(legs[2].side, LegSide::Short);
    assert_eq!(
        legs[3].contract,
        contract("605", front_expiry(), OptionKind::Call)
    );
    assert_eq!(legs[3].side, LegSide::Long);
}

/// The nearest listed expiry can clear the requested minimum while missing
/// the request's window: nothing listed fits, so selection refuses instead of
/// trading the nearest date the exchange happens to list.
#[test]
fn an_expiry_past_the_request_maximum_refuses() {
    let refusal = selector()
        .select_directional(
            &LegRequest {
                structure: StructureKind::Vertical,
                direction: Direction::Long,
                credit: false,
                strike_policy: StrikePolicy::NearestToTouch,
                dte_days: 7,
                max_dte_days: Some(20),
                underlying: "SPY",
                touch: touch(),
                as_of: as_of(),
            },
            &ladder(),
        )
        .expect_err("the 32-day front expiry is past the window");
    assert_eq!(
        refusal,
        LegSelectRefusal::ExpiryBeyondMaxDte {
            nearest_dte: 32,
            max_dte_days: 20,
        }
    );
}

#[test]
fn no_expiry_at_or_after_dte_refuses() {
    // Both ladder expiries sit inside 90 days of `as_of`.
    let refusal = selector()
        .select_legs(
            StructureKind::Vertical,
            90,
            "SPY",
            touch(),
            as_of(),
            &ladder(),
        )
        .expect_err("no expiry clears 90 DTE");
    assert_eq!(refusal, LegSelectRefusal::NoExpiryForDte { dte_days: 90 });
}

#[test]
fn empty_snapshot_refuses() {
    let refusal = selector()
        .select_legs(StructureKind::Vertical, 30, "SPY", touch(), as_of(), &[])
        .expect_err("empty snapshot selects nothing");
    assert_eq!(refusal, LegSelectRefusal::EmptySnapshot);
}

#[test]
fn wrong_underlying_refuses() {
    let refusal = selector()
        .select_legs(
            StructureKind::Vertical,
            30,
            "QQQ",
            touch(),
            as_of(),
            &ladder(),
        )
        .expect_err("another root's chain is not ours");
    assert_eq!(refusal, LegSelectRefusal::EmptySnapshot);
}

#[test]
fn crossed_quotes_still_select() {
    // Crossing is the fill policy's refusal, not selection's: the selector
    // reads presence, never prices.
    let crossed = ladder()
        .into_iter()
        .map(|quote| {
            ChainQuote::new()
                .contract(quote.contract.clone())
                .bid(strike("5.00"))
                .ask(strike("4.00"))
                .timestamp(quote.timestamp)
                .build()
        })
        .collect::<Vec<_>>();
    let legs = selector()
        .select_legs(
            StructureKind::Vertical,
            30,
            "SPY",
            touch(),
            as_of(),
            &crossed,
        )
        .expect("crossed books still select legs");
    assert_eq!(legs.len(), 2);
}

#[test]
fn naked_short_refuses_through_spread_validation() {
    let refusal = selector()
        .select_legs(
            StructureKind::NakedShort,
            30,
            "SPY",
            touch(),
            as_of(),
            &ladder(),
        )
        .expect_err("naked shorts never construct");
    assert!(
        matches!(refusal, LegSelectRefusal::InvalidSpread(_)),
        "the refusal comes from spread validation, not a second naked-short check: {refusal}"
    );
}

/// M2 calibration freeze: the golden matrix that pins the selection policy.
///
/// Two axes, tested separately:
///
/// * Producer-confidence (validity): the `None` rows must NOT select —
///   sub-DTE expiries, one-sided or single-strike grids, expiry-mismatched
///   calendars, empty snapshots, and non-spread structures. These controls
///   prove the selector only builds from adequate data; relaxing a rule to
///   force a selection would be confident about the wrong thing.
/// * Task-value (economics): the hard-coded rows below are the freeze —
///   every row runs the live `select_legs` and must match exactly. No ratio
///   is derived from the rows: a count of matches over the same comparison
///   the rows already assert would pass exactly when the rows pass, so the
///   rows themselves are the check.
fn touch_at(value: &str) -> Money {
    strike(value)
}

fn quotes_for(expiries: &[NaiveDate], strikes: &[&str]) -> Vec<ChainQuote> {
    let mut quotes = Vec::new();
    for on in expiries {
        for strike_value in strikes {
            quotes.push(quote_for(
                contract(strike_value, *on, OptionKind::Call),
                "2.50",
                "2.70",
            ));
            quotes.push(quote_for(
                contract(strike_value, *on, OptionKind::Put),
                "2.40",
                "2.60",
            ));
        }
    }
    quotes
}

struct SelectCase {
    name: &'static str,
    structure: StructureKind,
    dte_days: u32,
    touch: Money,
    quotes: Vec<ChainQuote>,
    expected: Option<Vec<(OptionContract, LegSide)>>,
}

fn long_leg(strike_value: &str, on: NaiveDate, kind: OptionKind) -> (OptionContract, LegSide) {
    (contract(strike_value, on, kind), LegSide::Long)
}

fn short_leg(strike_value: &str, on: NaiveDate, kind: OptionKind) -> (OptionContract, LegSide) {
    (contract(strike_value, on, kind), LegSide::Short)
}

fn selection_matrix() -> Vec<SelectCase> {
    let full = quotes_for(
        &[front_expiry(), back_expiry()],
        &["590", "595", "600", "605", "610"],
    );
    vec![
        SelectCase {
            name: "vertical straddles a between-strike touch",
            structure: StructureKind::Vertical,
            dte_days: 30,
            touch: touch_at("597.50"),
            quotes: full.clone(),
            expected: Some(vec![
                long_leg("595", front_expiry(), OptionKind::Call),
                short_leg("600", front_expiry(), OptionKind::Call),
            ]),
        },
        SelectCase {
            name: "vertical straddles upward",
            structure: StructureKind::Vertical,
            dte_days: 30,
            touch: touch_at("602.50"),
            quotes: full.clone(),
            expected: Some(vec![
                long_leg("600", front_expiry(), OptionKind::Call),
                short_leg("605", front_expiry(), OptionKind::Call),
            ]),
        },
        SelectCase {
            name: "vertical below every strike clamps to the lowest pair",
            structure: StructureKind::Vertical,
            dte_days: 30,
            touch: touch_at("580"),
            quotes: full.clone(),
            expected: Some(vec![
                long_leg("590", front_expiry(), OptionKind::Call),
                short_leg("595", front_expiry(), OptionKind::Call),
            ]),
        },
        SelectCase {
            name: "higher DTE skips the front expiry",
            structure: StructureKind::Vertical,
            dte_days: 33,
            touch: touch(),
            quotes: full.clone(),
            expected: Some(vec![
                long_leg("600", back_expiry(), OptionKind::Call),
                short_leg("605", back_expiry(), OptionKind::Call),
            ]),
        },
        SelectCase {
            name: "calendar breaks strike ties downward",
            structure: StructureKind::Calendar,
            dte_days: 30,
            touch: touch_at("597.50"),
            quotes: full.clone(),
            expected: Some(vec![
                long_leg("595", back_expiry(), OptionKind::Call),
                short_leg("595", front_expiry(), OptionKind::Call),
            ]),
        },
        SelectCase {
            name: "condor pins the iron shape",
            structure: StructureKind::Condor,
            dte_days: 30,
            touch: touch(),
            quotes: full.clone(),
            expected: Some(vec![
                long_leg("590", front_expiry(), OptionKind::Put),
                short_leg("595", front_expiry(), OptionKind::Put),
                short_leg("600", front_expiry(), OptionKind::Call),
                long_leg("605", front_expiry(), OptionKind::Call),
            ]),
        },
        // Control group: every row below must refuse.
        SelectCase {
            name: "touch above every strike leaves no short leg",
            structure: StructureKind::Vertical,
            dte_days: 30,
            touch: touch_at("620"),
            quotes: full.clone(),
            expected: None,
        },
        SelectCase {
            name: "single expiry below DTE",
            structure: StructureKind::Vertical,
            dte_days: 40,
            touch: touch(),
            quotes: quotes_for(&[front_expiry()], &["590", "595", "600", "605", "610"]),
            expected: None,
        },
        SelectCase {
            name: "single listed strike pairs with nothing",
            structure: StructureKind::Vertical,
            dte_days: 30,
            touch: touch(),
            quotes: quotes_for(&[front_expiry(), back_expiry()], &["600"]),
            expected: None,
        },
        SelectCase {
            name: "calendar with no back-expiry calls",
            structure: StructureKind::Calendar,
            dte_days: 30,
            touch: touch(),
            quotes: {
                let mut quotes =
                    quotes_for(&[front_expiry()], &["590", "595", "600", "605", "610"]);
                for strike_value in ["590", "595", "600", "605", "610"] {
                    quotes.push(quote_for(
                        contract(strike_value, back_expiry(), OptionKind::Put),
                        "2.40",
                        "2.60",
                    ));
                }
                quotes
            },
            expected: None,
        },
        SelectCase {
            name: "condor one-sided above the touch",
            structure: StructureKind::Condor,
            dte_days: 30,
            touch: touch(),
            quotes: quotes_for(&[front_expiry(), back_expiry()], &["600", "605", "610"]),
            expected: None,
        },
        SelectCase {
            name: "empty snapshot",
            structure: StructureKind::Vertical,
            dte_days: 30,
            touch: touch(),
            quotes: Vec::new(),
            expected: None,
        },
        SelectCase {
            name: "single never selects",
            structure: StructureKind::Single,
            dte_days: 30,
            touch: touch(),
            quotes: full.clone(),
            expected: None,
        },
        SelectCase {
            name: "naked short never constructs",
            structure: StructureKind::NakedShort,
            dte_days: 30,
            touch: touch(),
            quotes: full,
            expected: None,
        },
    ]
}

fn run_case(case: &SelectCase) -> Option<Vec<(OptionContract, LegSide)>> {
    selector()
        .select_legs(
            case.structure,
            case.dte_days,
            "SPY",
            case.touch,
            as_of(),
            &case.quotes,
        )
        .map(|legs| {
            legs.legs()
                .iter()
                .map(|leg| (leg.contract.clone(), leg.side))
                .collect()
        })
        .ok()
}

#[test]
fn golden_matrix_rows_are_the_freeze() {
    let mut selected = 0_usize;
    let mut refused = 0_usize;
    for case in selection_matrix() {
        let got = run_case(&case);
        assert_eq!(got, case.expected, "golden row diverged: {}", case.name);
        if got.is_some() {
            selected += 1;
        } else {
            refused += 1;
        }
    }
    assert!(
        selected > 0 && refused > 0,
        "the matrix must exercise both verdicts"
    );
}

#[test]
fn selection_policy_is_frozen() {
    // The rules the matrix measures. Any production move of these must first
    // update the frozen rows above.
    let full = quotes_for(
        &[front_expiry(), back_expiry()],
        &["590", "595", "600", "605", "610"],
    );
    let at = |dte: u32| {
        selector().select_legs(StructureKind::Vertical, dte, "SPY", touch(), as_of(), &full)
    };
    let front = |legs: &protopyne_types::SpreadLegs| legs.legs()[0].contract.expiry;
    // Jul-17 sits 32 days past Jun-15: eligible at 32, skipped at 33.
    assert_eq!(front(&at(32).expect("boundary DTE holds")), front_expiry());
    assert_eq!(front(&at(33).expect("higher DTE rolls")), back_expiry());
    // Ties break to the lower strike, never up.
    let tied = selector()
        .select_legs(
            StructureKind::Vertical,
            30,
            "SPY",
            touch_at("597.50"),
            as_of(),
            &full,
        )
        .expect("tied touch selects");
    assert_eq!(tied.legs()[0].contract.strike, strike("595"));
    // Verticals stay bull-call shaped on one expiry: long below, short above.
    let vertical = selector()
        .select_legs(StructureKind::Vertical, 30, "SPY", touch(), as_of(), &full)
        .expect("vertical selects");
    let legs = vertical.legs();
    assert_eq!(
        (legs[0].side, legs[1].side),
        (LegSide::Long, LegSide::Short)
    );
    assert_eq!(legs[0].contract.expiry, legs[1].contract.expiry);
    assert!(
        legs[0].contract.strike.as_decimal() < legs[1].contract.strike.as_decimal(),
        "long strike sits below short strike"
    );
}

// --- Directional selection, strike policy, and the chain-aware selector ---

use crate::elements::{SignalOutput, StructureRequest, StructureSelector};
use crate::proposal::Direction;
use crate::view::{ChainSlice, MarketData, MarketView};
use protopyne_types::{Bar, Greeks, Qty, Timeframe};

fn request_for(direction: Direction, credit: bool, policy: StrikePolicy) -> LegRequest<'static> {
    LegRequest {
        structure: StructureKind::Vertical,
        direction,
        credit,
        strike_policy: policy,
        dte_days: 30,
        max_dte_days: None,
        underlying: "SPY",
        touch: touch(),
        as_of: as_of(),
    }
}

fn selected_legs(
    request: &LegRequest<'_>,
    quotes: &[ChainQuote],
) -> Vec<(OptionContract, LegSide)> {
    selector()
        .select_directional(request, quotes)
        .expect("selection holds")
        .legs()
        .iter()
        .map(|leg| (leg.contract.clone(), leg.side))
        .collect()
}

#[test]
fn short_direction_selects_a_bear_put_vertical() {
    let legs = selected_legs(
        &request_for(Direction::Short, false, StrikePolicy::NearestToTouch),
        &ladder(),
    );
    assert_eq!(
        legs,
        vec![
            long_leg("600", front_expiry(), OptionKind::Put),
            short_leg("595", front_expiry(), OptionKind::Put),
        ],
        "a short direction must not keep the bull-call shape"
    );
}

#[test]
fn a_credit_intent_sells_the_anchor_and_buys_the_wing() {
    let bull_put = selected_legs(
        &request_for(Direction::Long, true, StrikePolicy::NearestToTouch),
        &ladder(),
    );
    assert_eq!(
        bull_put,
        vec![
            short_leg("600", front_expiry(), OptionKind::Put),
            long_leg("595", front_expiry(), OptionKind::Put),
        ]
    );

    let bear_call = selected_legs(
        &request_for(Direction::Short, true, StrikePolicy::NearestToTouch),
        &ladder(),
    );
    assert_eq!(
        bear_call,
        vec![
            short_leg("600", front_expiry(), OptionKind::Call),
            long_leg("605", front_expiry(), OptionKind::Call),
        ]
    );
}

#[test]
fn nearest_to_touch_reproduces_every_frozen_row() {
    for case in selection_matrix() {
        let got = selector()
            .select_directional(
                &LegRequest {
                    structure: case.structure,
                    direction: Direction::Long,
                    credit: false,
                    strike_policy: StrikePolicy::NearestToTouch,
                    dte_days: case.dte_days,
                    max_dte_days: None,
                    underlying: "SPY",
                    touch: case.touch,
                    as_of: as_of(),
                },
                &case.quotes,
            )
            .ok()
            .map(|legs| {
                legs.legs()
                    .iter()
                    .map(|leg| (leg.contract.clone(), leg.side))
                    .collect::<Vec<_>>()
            });
        assert_eq!(
            got, case.expected,
            "directional NearestToTouch diverged: {}",
            case.name
        );
    }
}

fn call_quote(contract: OptionContract, delta: f64) -> ChainQuote {
    ChainQuote::new()
        .contract(contract)
        .bid(strike("2.50"))
        .ask(strike("2.70"))
        .greeks(Greeks::new().delta(delta).build())
        .timestamp(chrono::Utc::now())
        .build()
}

fn delta_ladder() -> Vec<ChainQuote> {
    [
        ("590", 0.05),
        ("595", 0.20),
        ("600", 0.42),
        ("605", 0.65),
        ("610", 0.85),
    ]
    .into_iter()
    .map(|(strike_value, delta)| {
        call_quote(
            contract(strike_value, front_expiry(), OptionKind::Call),
            delta,
        )
    })
    .collect()
}

#[test]
fn delta_target_selects_the_closest_absolute_delta() {
    let legs = selected_legs(
        &request_for(
            Direction::Long,
            false,
            StrikePolicy::DeltaTarget {
                delta: "0.40".parse().unwrap(),
            },
        ),
        &delta_ladder(),
    );
    assert_eq!(
        legs,
        vec![
            long_leg("600", front_expiry(), OptionKind::Call),
            short_leg("605", front_expiry(), OptionKind::Call),
        ]
    );
}

#[test]
fn delta_target_refuses_without_greeks() {
    let refusal = selector()
        .select_directional(
            &request_for(
                Direction::Long,
                false,
                StrikePolicy::DeltaTarget {
                    delta: "0.40".parse().unwrap(),
                },
            ),
            &ladder(),
        )
        .expect_err("no greeks means no delta anchor");
    assert_eq!(refusal, LegSelectRefusal::MissingGreeks);
}

#[test]
fn an_out_of_range_delta_target_refuses() {
    let refusal = selector()
        .select_directional(
            &request_for(
                Direction::Long,
                false,
                StrikePolicy::DeltaTarget {
                    delta: "1.50".parse().unwrap(),
                },
            ),
            &delta_ladder(),
        )
        .expect_err("delta sits outside (0, 1]");
    assert!(matches!(refusal, LegSelectRefusal::InvalidDeltaTarget(_)));
}

#[test]
fn the_selector_prices_the_chain_derived_package() {
    let ts = chrono::Utc
        .with_ymd_and_hms(2026, 6, 15, 14, 30, 0)
        .unwrap();
    let quotes: Vec<ChainQuote> = ladder()
        .into_iter()
        .map(|mut quote| {
            quote.timestamp = ts;
            quote
        })
        .collect();
    let dec = |text: &str| text.parse::<Decimal>().expect("decimal");
    let bars = [Bar::new()
        .ts(ts)
        .o(dec("600"))
        .h(dec("601"))
        .l(dec("599"))
        .c(dec("600"))
        .volume(0)
        .build()];
    let chain = ChainSlice::new(&quotes, ts, "SPY").expect("quotes bind");
    let data = MarketData {
        bars: &bars,
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let view = MarketView {
        data: &data,
        closes: &[600.0],
        as_of: ts,
    };
    let signal = SignalOutput {
        direction: Direction::Long,
        conviction: 0.5,
        activity_score: 0.0,
    };
    let request = StructureRequest::new()
        .structure(StructureKind::Vertical)
        .dte_days(30)
        .quantity(Qty::new(Decimal::ONE))
        .fee_per_contract(strike("0.65"))
        .build();
    let selection = selector()
        .select(&view, &signal, &request)
        .expect("the chain supports a vertical");
    assert_eq!(selection.kind, StructureKind::Vertical);
    let legs = selection.legs.expect("legs selected");
    assert_eq!(legs.len(), 2);
    let economics = selection.economics.expect("package priced");
    assert_eq!(economics.package_width, Some(strike("5")));
    assert!(economics.bounded);
}
