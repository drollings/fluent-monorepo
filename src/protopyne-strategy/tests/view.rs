//! Point-in-time view guards: the future cannot be borrowed through a
//! `ChainSlice`, and the economics-only proposal path stays one body.

use super::*;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use protopyne_types::{ChainQuote, Money, OptionContract, OptionKind};
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;
use std::sync::Arc;

use crate::proposal::{ProposalSide, StructureKind};
use crate::{Direction, EconomicsPolicy, FixedSignal, ProposeInput, Signal, SizingCaps, Strategy};

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn strike(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn at(offset_secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 0).unwrap() + Duration::seconds(offset_secs)
}

fn quote(stamp: DateTime<Utc>) -> ChainQuote {
    ChainQuote::new()
        .contract(
            OptionContract::new()
                .underlying("SPY".to_owned())
                .expiry(NaiveDate::from_ymd_opt(2026, 7, 17).unwrap())
                .strike(strike(600))
                .kind(OptionKind::Call)
                .build(),
        )
        .bid(money(2))
        .ask(money(3))
        .timestamp(stamp)
        .build()
}

fn input(closes: &[f64]) -> ProposeInput<'_> {
    ProposeInput {
        strategy: "ribbon",
        underlying: "SPY",
        side: ProposalSide::Entry,
        closes,
        limit_price: money(250),
        max_loss: money(300),
        notional: money(1_000),
        dte_days: 30,
        max_dte_days: None,
        economics: EconomicsPolicy::ConfigSupplied,
        structure: StructureKind::Vertical,
        credit: false,
        fee_per_contract: Money::zero(),
        caps: SizingCaps::default(),
        created_at: at(0),
    }
}

#[test]
fn a_quote_after_the_view_instant_is_refused() {
    let quotes = [quote(at(-5)), quote(at(1))];
    let refusal = ChainSlice::new(&quotes, at(0), "SPY").expect_err("future quote refuses");
    assert!(
        refusal.to_string().contains("after the view instant"),
        "the refusal names the time bound: {refusal}"
    );
}

#[test]
fn quotes_at_or_before_filters_to_the_requested_instant() {
    let quotes = [quote(at(-10)), quote(at(-5)), quote(at(0))];
    let slice = ChainSlice::new(&quotes, at(0), "SPY").expect("present quotes bind");
    assert_eq!(slice.underlying(), "SPY");
    assert_eq!(slice.as_of(), at(0));
    assert_eq!(slice.quotes().len(), 3);
    assert_eq!(slice.quotes_at_or_before(at(-10)).count(), 1);
    assert_eq!(slice.quotes_at_or_before(at(-5)).count(), 2);
    assert_eq!(slice.quotes_at_or_before(at(0)).count(), 3);
}

#[test]
fn an_empty_chain_is_no_chain_not_an_error() {
    let slice = ChainSlice::new(&[], at(0), "SPY").expect("an empty chain binds");
    assert!(slice.is_empty());
    assert_eq!(slice.quotes_at_or_before(at(0)).count(), 0);
    assert!(MarketData::empty(30).chain.is_none());
}

#[test]
fn the_economics_only_path_agrees_with_a_minimal_view() {
    let strategy = Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    ));
    let closes = [100.0, 101.0, 102.0];
    let input = input(&closes);
    let through_config = strategy.propose("p1", input).expect("proposal");

    let data = MarketData::empty(input.dte_days);
    let view = MarketView {
        data: &data,
        closes: &closes,
        as_of: input.created_at,
    };
    let mut signal = strategy
        .bind(&crate::SignalConfig::new(data.timeframe))
        .expect("fixed signal binds");
    let through_view = strategy
        .propose_with("p1", input, &view, signal.as_mut())
        .expect("proposal");
    assert_eq!(through_config, through_view);
}

fn bar(close: i64) -> protopyne_types::Bar {
    protopyne_types::Bar::new()
        .ts(at(0))
        .o(Decimal::from(close))
        .h(Decimal::from(close))
        .l(Decimal::from(close))
        .c(Decimal::from(close))
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

#[test]
fn a_fixed_signal_ignores_the_bar_and_chain_window() {
    let bars = [bar(100), bar(101), bar(102)];
    let closes = [100.0, 101.0, 102.0];
    let full = MarketData {
        bars: &bars,
        chain: None,
        dte_days: 30,
        timeframe: protopyne_types::Timeframe::M15,
    };
    let populated = MarketView {
        data: &full,
        closes: &closes,
        as_of: at(0),
    };
    let empty_data = MarketData::empty(30);
    let empty = MarketView {
        data: &empty_data,
        closes: &[],
        as_of: at(0),
    };
    let signal = FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores");
    assert_eq!(
        signal.evaluate(&populated).expect("scores"),
        signal.evaluate(&empty).expect("scores"),
        "a fixed signal must not read the view"
    );
}

/// A visible trend/noise fixture whose RSRS slope variance stays positive.
fn wobble_bars(count: usize) -> Vec<protopyne_types::Bar> {
    (0..count)
        .map(|i| {
            let high = Decimal::from_f64_retain(100.0 + i as f64 * 0.7 + (i as f64 * 1.3).sin())
                .expect("finite high");
            let low = Decimal::from_f64_retain(99.0 + i as f64 * 0.3 + (i as f64 * 0.7).cos())
                .expect("finite low");
            protopyne_types::Bar::new()
                .ts(at(i as i64 * 60))
                .o(low)
                .h(high)
                .l(low)
                .c(high)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

#[test]
fn the_generator_is_fed_from_the_view_window() {
    use crate::generator::{GenerateInput, Generator};
    let bars = wobble_bars(40);
    let closes: Vec<f64> = bars.iter().map(|bar| bar.c.to_f64().unwrap()).collect();
    let data = MarketData {
        bars: &bars,
        chain: None,
        dte_days: 30,
        timeframe: protopyne_types::Timeframe::M1,
    };
    let view = MarketView {
        data: &data,
        closes: &closes,
        as_of: at(0),
    };
    let mut generator = Generator::builder()
        .slope_lookback(2)
        .score_window(3)
        .entry_threshold(10.0)
        .hold_threshold(-1_000.0)
        .build();
    let proposal = generator.propose(GenerateInput {
        strategy: "ribbon",
        underlying: "SPY",
        bars: view.data.bars,
        quantity: 2,
        limit_price: money(100),
        max_loss: money(50),
        notional: money(100),
        dte_days: 30,
        activity_score: 1.0,
        at: view.as_of,
    });
    assert!(
        proposal.is_some(),
        "the view's bars drive the generator ladder"
    );
}

#[test]
fn a_shared_strategy_evaluates_identically_across_threads() {
    // The signal is shared behind an `Arc` across pool workers. Each worker
    // binds its own instance, so even a stateful signal cannot leak through
    // the shared handle: every worker decides like the baseline.
    let strategy = Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    ));
    let closes = [100.0, 101.0, 102.0];
    let data = MarketData::empty(30);
    let view = MarketView {
        data: &data,
        closes: &closes,
        as_of: at(0),
    };
    let config = crate::SignalConfig::new(data.timeframe);
    let mut baseline_signal = strategy.bind(&config).expect("binds");
    let baseline = strategy
        .propose_with("p1", input(&closes), &view, baseline_signal.as_mut())
        .expect("baseline proposal");
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let strategy = &strategy;
            let baseline = &baseline;
            let config = &config;
            scope.spawn(move || {
                let data = MarketData::empty(30);
                let view = MarketView {
                    data: &data,
                    closes: &closes,
                    as_of: at(0),
                };
                let mut signal = strategy.bind(config).expect("binds");
                for _ in 0..64 {
                    let proposal = strategy
                        .propose_with("p1", input(&closes), &view, signal.as_mut())
                        .expect("worker proposal");
                    assert_eq!(proposal, *baseline, "workers must not diverge");
                }
            });
        }
    });
}
