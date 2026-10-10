//! Generator and rebalancer calibration: RSRS-gated vertical goldens,
//! conviction-proportional utility, sorted ranking with re-scores,
//! mid-curve portfolio carry-over, and reconcile-halt narrowing.
//!
//! The ladder is score thresholds only — no candle pattern is seeded into
//! the expectations. Breakout bars must emit, range-bound bars must veto,
//! and a reconcile mismatch must hold entries while exits stay open.

use super::*;
use chrono::{TimeZone, Utc};
use rust_decimal::Decimal;

use protopyne_types::{Bar, Money};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn decimal(value: f64) -> Decimal {
    Decimal::from_f64_retain(value).expect("finite price")
}

fn bar(ts_min: u32, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar::new()
        .ts(Utc.with_ymd_and_hms(2026, 6, 1, 12, ts_min, 0).unwrap())
        .o(decimal(open))
        .h(decimal(high))
        .l(decimal(low))
        .c(decimal(close))
        .volume(1_000)
        .maybe_adj(None)
        .build()
}

/// Breakout fuel: linear lows with a quadratically expanding range. The
/// latest high-on-low slope stands ~1.4 std above its window mean — the
/// RSRS shape of a range expansion, not a candle pattern.
fn breakout_bars() -> Vec<Bar> {
    let mut bars = Vec::new();
    let mut close = 600.0;
    for index in 0..12_u32 {
        let i = f64::from(index);
        let open = close;
        close = 600.0 + 0.5 * i;
        let high = 600.0 + 0.5 * i + 0.1 * i * i;
        let low = 600.0 + 0.5 * i - 0.5;
        bars.push(bar(index, open, high, low, close.max(open)));
    }
    bars
}

/// Mid-ladder fuel: the range expands in steps then holds its width, so the
/// latest slope sits inside one std of its window mean. Step increments
/// `[0 × 6, 0.2 × 2, 0.3 × 4]` shape the slope path; the score lands near
/// 0.75 by construction of the slope/fraction arithmetic, not by pattern.
fn steady_bars() -> Vec<Bar> {
    let increments = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.2, 0.2, 0.3, 0.3, 0.3, 0.3];
    let mut bars = Vec::new();
    let mut spread = 0.0;
    let mut close = 600.0;
    for (index, step) in increments.into_iter().enumerate() {
        spread += step;
        let i = index as f64;
        let open = close;
        close = 600.0 + 0.5 * i;
        bars.push(bar(
            index as u32,
            open,
            600.0 + 0.5 * i + spread,
            600.0 + 0.5 * i - 0.5,
            close.max(open),
        ));
    }
    bars
}

/// Flat market: no slope variance, nothing to rank.
fn range_bars() -> Vec<Bar> {
    (0..12_u32)
        .map(|index| bar(index, 600.0, 601.0, 600.0, 600.5))
        .collect()
}

fn generator() -> Generator {
    Generator::builder()
        .slope_lookback(3)
        .score_window(5)
        .entry_threshold(1.0)
        .hold_threshold(0.0)
        .build()
}

fn input<'a>(underlying: &'a str, bars: &'a [Bar]) -> GenerateInput<'a> {
    GenerateInput {
        strategy: "ribbon",
        underlying,
        bars,
        quantity: 2,
        limit_price: money("1.60"),
        max_loss: money("40"),
        notional: money("320"),
        dte_days: 45,
        activity_score: 0.5,
        at: Utc.with_ymd_and_hms(2026, 6, 1, 12, 30, 0).unwrap(),
    }
}

#[test]
fn ladder_rungs_are_hand_checked() {
    assert_eq!(ladder_rung(Some(1.5), 1.0, 0.0), Rung::Full);
    assert_eq!(ladder_rung(Some(1.0), 1.0, 0.0), Rung::Full);
    assert_eq!(ladder_rung(Some(0.5), 1.0, 0.0), Rung::Half);
    assert_eq!(ladder_rung(Some(0.0), 1.0, 0.0), Rung::Half);
    assert_eq!(ladder_rung(Some(-0.1), 1.0, 0.0), Rung::Veto);
    assert_eq!(ladder_rung(None, 1.0, 0.0), Rung::Veto);
}

#[test]
fn utility_scales_with_conviction() {
    let high = utility(0.8, 0.5);
    let low = utility(0.4, 0.5);
    assert!((high - 2.0 * low).abs() < 1e-12, "utility is proportional to conviction");
    assert!(utility(0.0, 0.5).abs() < 1e-12, "zero conviction is zero utility");
}

#[test]
fn breakout_emits_full_vertical() {
    let bars = breakout_bars();
    let score = rsrs_score(&bars, 3, 5).expect("breakout must score");
    assert!(score >= 1.0, "breakout clears the entry rung, got {score}");
    let mut generator = generator();
    let proposal = generator.propose(input("SPY", &bars)).expect("must emit");
    assert_eq!(proposal.structure, StructureKind::Vertical);
    assert_eq!(proposal.side, ProposalSide::Entry);
    assert_eq!(proposal.direction, Direction::Long);
    assert_eq!(proposal.quantity, Qty::new(Decimal::from(2)));
}

#[test]
fn range_bound_vetoes() {
    let bars = range_bars();
    assert_eq!(rsrs_score(&bars, 3, 5), None, "flat market has nothing to rank");
    let mut generator = generator();
    assert!(generator.propose(input("SPY", &bars)).is_none(), "must veto");
}

#[test]
fn mid_trend_emits_half_quantity() {
    let bars = steady_bars();
    let score = rsrs_score(&bars, 3, 5).expect("steady trend still scores");
    assert!(score < 1.0, "constant slope carries no surprise, got {score}");
    let mut generator = generator();
    let proposal = generator.propose(input("SPY", &bars)).expect("must emit at half");
    assert_eq!(proposal.quantity, Qty::new(Decimal::from(1)));
}

#[test]
fn short_history_vetoes_fail_closed() {
    let bars = breakout_bars();
    let short = &bars[..3];
    assert_eq!(rsrs_score(short, 3, 5), None);
    let mut generator = generator();
    assert!(generator.propose(input("SPY", short)).is_none());
}

#[test]
fn proposals_stay_sorted_by_utility_with_rescores() {
    let breakout = breakout_bars();
    let steady = steady_bars();
    let mut generator = generator();
    generator.propose(input("SPY", &steady)).expect("half");
    generator.propose(input("QQQ", &breakout)).expect("full");
    let ranked = generator.ranked();
    assert_eq!(ranked.len(), 2);
    assert!(
        utility(ranked[0].conviction, ranked[0].activity_score)
            >= utility(ranked[1].conviction, ranked[1].activity_score),
        "best utility first"
    );
    assert_eq!(generator.scores.len(), 2, "re-score map tracks both keys");

    let low_id = ranked[1].id.clone();
    let high_id = ranked[0].id.clone();
    generator.rescore(&high_id, 0.1);
    let reranked = generator.ranked();
    assert_eq!(reranked[0].id, low_id, "re-score re-sorts");
    assert_eq!(reranked[1].id, high_id);
    assert_eq!(generator.scores.len(), 2, "re-score map is the only per-key state");
}

#[test]
fn portfolio_carry_over_equality() {
    let step_one = vec![
        Exposure { underlying: "SPY".to_owned(), strategy: "ribbon".to_owned() },
        Exposure { underlying: "SPY".to_owned(), strategy: "ribbon".to_owned() },
    ];
    let step_two = vec![
        Exposure { underlying: "SPY".to_owned(), strategy: "ribbon".to_owned() },
        Exposure { underlying: "SPY".to_owned(), strategy: "ribbon".to_owned() },
    ];
    let carried = portfolio_for("SPY", &step_one);
    let derived = portfolio_for("SPY", &step_two);
    assert_eq!(carried, derived, "mid-curve derivation equals the carried portfolio");
    carried.validate().expect("derived portfolios validate");
    let other = portfolio_for("QQQ", &step_two);
    assert_ne!(carried.id, other.id);
}

#[test]
fn reconcile_halt_holds_entries_keeps_exits() {
    let entry = input_proposal("entry-1", ProposalSide::Entry);
    let exit = input_proposal("exit-1", ProposalSide::Exit);

    let open = rebalance(vec![entry.clone(), exit.clone()], true, "paper");
    assert_eq!(open.mode, RebalanceMode::Full);
    assert_eq!(open.entries.len(), 1);
    assert_eq!(open.exits.len(), 1);
    assert!(open.alerts.is_empty());

    let halted = rebalance(vec![entry, exit], false, "paper");
    assert_eq!(halted.mode, RebalanceMode::NoNewTrades);
    assert!(halted.entries.is_empty(), "mismatch holds every entry");
    assert_eq!(halted.exits.len(), 1, "exits stay open under halt");
    assert_eq!(halted.alerts.len(), 1);
    assert_eq!(halted.alerts[0].portfolio, "paper");
    assert!(
        halted.alerts[0].message.contains("reconciliation"),
        "alert says why, got: {}",
        halted.alerts[0].message
    );
}

fn input_proposal(id: &str, side: ProposalSide) -> Proposal {
    Proposal::new()
        .id(id.to_owned())
        .strategy("ribbon".to_owned())
        .underlying("SPY".to_owned())
        .direction(Direction::Long)
        .side(side)
        .structure(StructureKind::Vertical)
        .quantity(Qty::new(Decimal::from(2)))
        .limit_price(money("1.60"))
        .max_loss(money("40"))
        .notional(money("320"))
        .dte_days(45)
        .conviction(0.8)
        .activity_score(0.5)
        .created_at(Utc.with_ymd_and_hms(2026, 6, 1, 12, 30, 0).unwrap())
        .build()
}
