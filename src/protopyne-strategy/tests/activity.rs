//! Options-activity seam: the chain proxy is bounded, quiet on unreadable
//! chains, and never replaces the signal's score with a guess.

use super::*;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use protopyne_types::{ChainQuote, Money, OptionContract, OptionKind, Timeframe};
use rust_decimal::Decimal;
use std::sync::Arc;

use crate::elements::{EconomicsPolicy, SignalConfig, Strategy};
use crate::proposal::{ProposalSide, StructureKind};
use crate::view::{ChainSlice, MarketData, MarketView};
use crate::{Direction, FixedSignal, ProposeInput, SizingCaps};

fn money(value: f64) -> Money {
    Money::new(Decimal::from_f64_retain(value).expect("finite money"))
}

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 0).unwrap()
}

#[allow(clippy::too_many_arguments)]
fn quote(
    strike: f64,
    kind: OptionKind,
    bid: f64,
    ask: f64,
    bid_size: u32,
    ask_size: u32,
    open_interest: u64,
) -> ChainQuote {
    ChainQuote::new()
        .contract(
            OptionContract::new()
                .underlying("SPY".to_owned())
                .expiry(NaiveDate::from_ymd_opt(2026, 7, 17).unwrap())
                .strike(money(strike))
                .kind(kind)
                .build(),
        )
        .bid(money(bid))
        .ask(money(ask))
        .bid_size(bid_size)
        .ask_size(ask_size)
        .open_interest(open_interest)
        .timestamp(at())
        .build()
}

fn view<'a>(closes: &'a [f64], data: &'a MarketData<'a>) -> MarketView<'a> {
    MarketView {
        data,
        closes,
        as_of: at(),
    }
}

fn with_chain(quotes: &[ChainQuote]) -> ChainSlice<'_> {
    ChainSlice::new(quotes, at(), "SPY").expect("quotes bind")
}

fn signal() -> FixedSignal {
    FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores")
}

fn input(closes: &[f64]) -> ProposeInput<'_> {
    ProposeInput {
        strategy: "ribbon",
        underlying: "SPY",
        side: ProposalSide::Entry,
        closes,
        limit_price: money(250.0),
        max_loss: money(300.0),
        notional: money(1_000.0),
        dte_days: 30,
        max_dte_days: None,
        economics: EconomicsPolicy::ConfigSupplied,
        structure: StructureKind::Vertical,
        credit: false,
        fee_per_contract: Money::zero(),
        caps: SizingCaps::default(),
        created_at: at(),
    }
}

#[test]
fn a_single_strike_is_not_a_reading() {
    let quotes = [
        quote(100.0, OptionKind::Call, 2.0, 2.2, 10, 10, 50),
        quote(100.0, OptionKind::Put, 2.0, 2.2, 10, 10, 50),
    ];
    let closes = [100.0];
    let chain = with_chain(&quotes);
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let score = ChainActivityScore::new()
        .score(&view(&closes, &data))
        .expect("the proxy runs");
    assert_eq!(score, None, "one listed strike cannot rank activity");
}

#[test]
fn a_crossed_chain_is_not_a_reading() {
    let quotes = [
        quote(100.0, OptionKind::Call, 2.0, 2.2, 10, 10, 50),
        quote(105.0, OptionKind::Call, 2.2, 2.0, 10, 10, 50),
    ];
    let closes = [100.0];
    let chain = with_chain(&quotes);
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let score = ChainActivityScore::new()
        .score(&view(&closes, &data))
        .expect("the proxy runs");
    assert_eq!(score, None, "a crossed book has no reading");
}

#[test]
fn zero_size_is_not_a_reading() {
    let quotes = [
        quote(100.0, OptionKind::Call, 2.0, 2.2, 0, 0, 50),
        quote(105.0, OptionKind::Call, 2.2, 2.4, 0, 0, 50),
    ];
    let closes = [100.0];
    let chain = with_chain(&quotes);
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let score = ChainActivityScore::new()
        .score(&view(&closes, &data))
        .expect("the proxy runs");
    assert_eq!(score, None, "zero size cannot rank activity");
}

#[test]
fn an_empty_view_is_not_a_reading() {
    let quotes: [ChainQuote; 0] = [];
    let closes = [100.0];
    let chain = with_chain(&quotes);
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    assert_eq!(
        ChainActivityScore::new()
            .score(&view(&closes, &data))
            .expect("the proxy runs"),
        None
    );
}

#[test]
fn a_one_sided_book_scores_balanced_open_interest_at_half() {
    let quotes = [
        quote(100.0, OptionKind::Call, 2.0, 2.2, 40, 0, 50),
        quote(105.0, OptionKind::Call, 2.2, 2.4, 30, 0, 50),
    ];
    let closes = [100.0];
    let chain = with_chain(&quotes);
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let score = ChainActivityScore::new()
        .score(&view(&closes, &data))
        .expect("the proxy runs")
        .expect("a readable chain scores");
    assert!(
        (score - 0.5).abs() < 1e-12,
        "one-sided imbalance and even OI average to 0.5, got {score}"
    );
}

#[test]
fn the_proxy_stays_inside_the_unit_interval() {
    let quotes = [
        quote(100.0, OptionKind::Call, 2.0, 2.2, 30, 10, 60),
        quote(105.0, OptionKind::Call, 2.2, 2.4, 10, 30, 40),
    ];
    let closes = [100.0];
    let chain = with_chain(&quotes);
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let score = ChainActivityScore::new()
        .score(&view(&closes, &data))
        .expect("the proxy runs")
        .expect("a readable chain scores");
    assert!((0.0..=1.0).contains(&score), "score {score} is bounded");
}

#[test]
fn a_non_positive_band_refuses() {
    assert!(ChainActivityScore::with_band(0.0).is_err());
    assert!(ChainActivityScore::with_band(1.5).is_err());
    assert!(ChainActivityScore::with_band(f64::NAN).is_err());
    let band = ChainActivityScore::with_band(0.2)
        .expect("a valid band")
        .near_money_band();
    assert!((band - 0.2).abs() < 1e-12);
}

#[test]
fn an_absent_source_keeps_the_signal_score() {
    let strategy = Strategy::with_signal(Arc::new(signal()));
    let closes = [100.0];
    let proposal = strategy
        .propose("p", input(&closes))
        .expect("proposal with the signal's score");
    assert!((proposal.activity_score - 0.7).abs() < 1e-12);
}

#[test]
fn an_installed_source_replaces_the_signal_score() {
    let quotes = [
        quote(100.0, OptionKind::Call, 2.0, 2.2, 40, 0, 50),
        quote(105.0, OptionKind::Call, 2.2, 2.4, 30, 0, 50),
    ];
    let closes = [100.0];
    let chain = with_chain(&quotes);
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let strategy = Strategy::with_signal(Arc::new(signal()))
        .with_options_activity(Arc::new(ChainActivityScore::new()));
    let mut instance = strategy
        .bind(&SignalConfig::new(Timeframe::D1))
        .expect("binds");
    let proposal = strategy
        .propose_with(
            "p",
            input(&closes),
            &view(&closes, &data),
            instance.as_mut(),
        )
        .expect("proposal with the chain score");
    assert!((proposal.activity_score - 0.5).abs() < 1e-12);
}

#[test]
fn an_unreadable_chain_keeps_the_signal_score() {
    let closes = [100.0];
    let data = MarketData {
        bars: &[],
        chain: None,
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let strategy = Strategy::with_signal(Arc::new(signal()))
        .with_options_activity(Arc::new(ChainActivityScore::new()));
    let mut instance = strategy
        .bind(&SignalConfig::new(Timeframe::D1))
        .expect("binds");
    let proposal = strategy
        .propose_with(
            "p",
            input(&closes),
            &view(&closes, &data),
            instance.as_mut(),
        )
        .expect("proposal keeps its own score");
    assert!((proposal.activity_score - 0.7).abs() < 1e-12);
}

struct WildSource;

impl OptionsActivity for WildSource {
    fn score(&self, _view: &MarketView<'_>) -> Result<Option<f64>, StrategyError> {
        Ok(Some(1.5))
    }
}

#[test]
fn a_source_outside_the_unit_interval_refuses() {
    let closes = [100.0];
    let data = MarketData {
        bars: &[],
        chain: None,
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let strategy =
        Strategy::with_signal(Arc::new(signal())).with_options_activity(Arc::new(WildSource));
    let mut instance = strategy
        .bind(&SignalConfig::new(Timeframe::D1))
        .expect("binds");
    let err = strategy
        .propose_with(
            "p",
            input(&closes),
            &view(&closes, &data),
            instance.as_mut(),
        )
        .expect_err("an out-of-range score refuses");
    assert!(err.to_string().contains("outside [0, 1]"), "{err}");
}
