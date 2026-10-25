//! Equity management contract: sizing math hand-checked, TP normalization,
//! ratchet-only stops, partial-once exhaustion, and gate must-fire /
//! must-not-fire pairs. All values are pure — no venue, no clock.

use super::*;
use rust_decimal::Decimal;

use crate::elements::Sizing;
use crate::proposal::Direction;
use crate::risk::PortfolioFacts;
use protopyne_types::{MarkBasis, Money, Qty};

fn money(value: &str) -> Money {
    Money::new(value.parse::<Decimal>().expect("decimal"))
}

fn unobserved() -> PortfolioFacts {
    PortfolioFacts::unobserved()
}

fn sizer() -> EquitySizer {
    EquitySizer::new("Risk Percent", 0.5, 25.0, 1.0, 100.0).expect("dials validate")
}

#[test]
fn risk_percent_sizes_from_stop_distance_by_hand() {
    // $100k equity, 0.5% risk = $500 over a $2 stop → 250 shares.
    let trade = sizer().for_trade(100_000.0, 100.0, 2.0);
    assert_eq!(trade.shares(&unobserved()), 250);
    // No stop distance where the mode needs one sizes zero, never a guess.
    let missing = sizer().for_trade(100_000.0, 100.0, f64::NAN);
    assert_eq!(missing.shares(&unobserved()), 0);
}

#[test]
fn percent_of_equity_and_fixed_modes_by_hand() {
    // 25% of $100k = $25k notional at $100 → 250 shares.
    let pct = EquitySizer::new("Percent of Equity", 0.5, 25.0, 1.0, 100.0)
        .expect("validates")
        .for_trade(100_000.0, 100.0, 2.0);
    assert_eq!(pct.shares(&unobserved()), 250);
    // Fixed mode ignores price and stop: 7 shares, capped by max notional.
    let fixed = EquitySizer::new("Fixed Quantity", 0.5, 25.0, 7.0, 100.0)
        .expect("validates")
        .for_trade(100_000.0, 10_000.0, 2.0);
    assert_eq!(fixed.shares(&unobserved()), 7);
    // Max-notional cap binds every mode: $100k cap at $200 asks 500, not
    // the 1000 the risk math allows.
    let capped = EquitySizer::new("Risk Percent", 2.0, 25.0, 1.0, 100.0)
        .expect("validates")
        .for_trade(100_000.0, 200.0, 2.0);
    assert_eq!(capped.shares(&unobserved()), 500);
    // Observed facts win over carried numbers: net liq $50k halves the
    // percent-of-equity answer.
    let facts = PortfolioFacts {
        net_liq: Some(money("50000")),
        ..unobserved()
    };
    assert_eq!(pct.shares(&facts), 125);
}

#[test]
fn tp_weights_normalize_past_100_and_hold_below() {
    for (got, want) in [
        (normalize_tp_weights([30.0, 30.0, 60.0]), [25.0, 25.0, 50.0]),
        (normalize_tp_weights([30.0, 30.0, 0.0]), [30.0, 30.0, 0.0]),
        (normalize_tp_weights([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]),
    ] {
        for (g, w) in got.iter().zip(want.iter()) {
            assert!(
                (g - w).abs() < 1e-12,
                "weights normalize to {want:?}, got {got:?}"
            );
        }
    }
}

#[test]
fn stop_distances_match_hand_math() {
    assert!((stop_distance_atr(2.0, 1.5).expect("valid") - 3.0).abs() < 1e-12);
    assert!((stop_distance_pct(100.0, 1.0).expect("valid") - 1.0).abs() < 1e-12);
    // Long: 100 entry over a 97 extreme plus 0.25 × 2 buffer → 3.5.
    assert!(
        (stop_distance_swing(true, 100.0, 97.0, 0.25, 2.0).expect("valid") - 3.5).abs() < 1e-12
    );
    // Short mirrors: extreme above the price.
    assert!(
        (stop_distance_swing(false, 100.0, 103.0, 0.25, 2.0).expect("valid") - 3.5).abs() < 1e-12
    );
    assert!(stop_distance_atr(0.0, 1.5).is_err(), "no ATR refuses");
    assert!(stop_distance_atr(2.0, 0.0).is_err(), "no multiple refuses");
    // Clamp keeps dust off the spread and far stops honest.
    assert!((clamp_stop(0.3, 2.0, 0.5, 4.0) - 1.0).abs() < 1e-12);
    assert!((clamp_stop(10.0, 2.0, 0.5, 4.0) - 8.0).abs() < 1e-12);
    assert!((clamp_stop(3.0, 2.0, 0.5, 4.0) - 3.0).abs() < 1e-12);
}

#[test]
fn stops_never_loosen_and_breakeven_covers_fees() {
    // Long ratchets up only; short down only.
    assert!((ratchet_stop(true, 98.0, 99.0) - 99.0).abs() < 1e-12);
    assert!((ratchet_stop(true, 98.0, 97.0) - 98.0).abs() < 1e-12);
    assert!((ratchet_stop(false, 102.0, 101.0) - 101.0).abs() < 1e-12);
    assert!((ratchet_stop(false, 102.0, 103.0) - 102.0).abs() < 1e-12);
    // Breakeven sits past entry by the offset, covering fees and slippage.
    assert!((breakeven_price(true, 100.0, 0.1, 2.0) - 100.2).abs() < 1e-9);
    assert!((breakeven_price(false, 100.0, 0.1, 2.0) - 99.8).abs() < 1e-9);
}

#[test]
fn trailing_candidates_match_hand_math() {
    assert!((trail_chandelier(true, 110.0, 2.0, 2.5) - 105.0).abs() < 1e-12);
    assert!((trail_chandelier(false, 90.0, 2.0, 2.5) - 95.0).abs() < 1e-12);
    assert!((trail_percent(true, 100.0, 1.0) - 99.0).abs() < 1e-12);
    assert!((trail_percent(false, 100.0, 1.0) - 101.0).abs() < 1e-12);
    assert!((trail_ribbon_edge(true, 105.0, 0.5, 2.0) - 104.0).abs() < 1e-12);
    assert!((trail_ribbon_edge(false, 95.0, 0.5, 2.0) - 96.0).abs() < 1e-12);
}

#[test]
fn exhaustion_partial_fires_exactly_once() {
    let mut once = PartialOnce::default();
    assert!(
        once.may_fire(true, true, true),
        "armed and profitable fires"
    );
    once.latch();
    assert!(
        !once.may_fire(true, true, true),
        "a later exhaustion cannot re-fire"
    );
    let fresh = PartialOnce::default();
    assert!(!fresh.may_fire(false, true, true), "disarmed never fires");
    assert!(
        !fresh.may_fire(true, false, true),
        "only-profit skips losers"
    );
    assert!(
        fresh.may_fire(true, false, false),
        "unrestricted fires into a loss"
    );
}

#[test]
fn cooldown_blocks_then_releases() {
    let open = CooldownGate {
        bars: 3,
        loss_bars: 10,
        last_exit: None,
    };
    assert!(open.may_enter(0), "no exit on record passes");
    let flat = CooldownGate {
        bars: 3,
        loss_bars: 10,
        last_exit: Some((10, false)),
    };
    assert!(!flat.may_enter(13), "3 bars after a flat exit still cools");
    assert!(flat.may_enter(14), "the fourth bar re-arms");
    let loss = CooldownGate {
        bars: 3,
        loss_bars: 10,
        last_exit: Some((10, true)),
    };
    assert!(!loss.may_enter(20), "losses add the extra cooldown");
    assert!(loss.may_enter(24), "3 + 10 bars later re-arms");
}

#[test]
fn daily_limits_halt_and_flatten() {
    let capped = DailyLimits {
        max_trades: 8,
        max_consec_losses: 0,
        max_daily_loss_pct: 0.0,
        trades_today: 8,
        consec_losses: 0,
        day_open_equity: None,
        equity_now: None,
        halted: false,
    };
    assert!(!capped.may_enter(), "max trades halts entries");
    let streak = DailyLimits {
        max_trades: 0,
        max_consec_losses: 3,
        max_daily_loss_pct: 0.0,
        trades_today: 0,
        consec_losses: 3,
        day_open_equity: None,
        equity_now: None,
        halted: false,
    };
    assert!(!streak.may_enter(), "loss streak halts entries");
    let breach = DailyLimits {
        max_trades: 0,
        max_consec_losses: 0,
        max_daily_loss_pct: 2.0,
        trades_today: 0,
        consec_losses: 0,
        day_open_equity: Some(100_000.0),
        equity_now: Some(97_500.0),
        halted: false,
    };
    assert!(breach.must_flatten(), "-2.5% flattens");
    let unmeasured = DailyLimits {
        day_open_equity: None,
        equity_now: Some(97_500.0),
        ..breach
    };
    assert!(
        !unmeasured.must_flatten(),
        "unmeasurable equity never flattens"
    );
    let off = DailyLimits {
        max_daily_loss_pct: 0.0,
        ..breach
    };
    assert!(!off.must_flatten(), "disabled cap never flattens");
}

#[test]
fn session_gate_flattens_and_blocks_by_the_clock() {
    // 16:00 ET close = minute 960.
    let gate = SessionGate {
        flatten_min_before: 0,
        noentry_min: 5,
        close_min: 960,
    };
    assert!(gate.must_flatten(960), "the close bar flattens");
    assert!(!gate.must_flatten(959), "before the close holds");
    assert!(!gate.may_enter(955), "final five minutes block entries");
    assert!(gate.may_enter(954), "outside the window entries arm");
}

#[test]
fn sizer_names_itself_and_refuses_bad_dials() {
    assert_eq!(sizer().name(), "risk-percent");
    assert!(EquitySizer::new("Martingale", 0.5, 25.0, 1.0, 100.0).is_err());
    assert!(EquitySizer::new("Risk Percent", 0.0, 25.0, 1.0, 100.0).is_err());
    assert!(EquitySizer::new("Risk Percent", 0.5, 25.0, -1.0, 100.0).is_err());
}

fn position(side: Direction, qty: u32, entry: &str) -> crate::elements::PositionView<'static> {
    crate::elements::PositionView {
        underlying: "SPY".to_owned(),
        side,
        quantity: qty,
        entry_avg: money(entry),
        max_loss: money("1000"),
        dte_days: None,
        legs: None,
        events: None,
    }
}

fn mark(price: Option<&str>) -> crate::elements::MarkContext {
    crate::elements::MarkContext {
        basis: MarkBasis::FillPrice,
        mark: price.map(money),
    }
}

#[test]
fn stop_and_target_fire_past_the_line_hold_on_it() {
    use crate::elements::LossMgmt;
    let stop = EquityStopLoss::new(money("200")).expect("positive validates");
    assert_eq!(stop.name(), "equity-stop");
    assert!(
        EquityStopLoss::new(money("0")).is_err(),
        "dust stop refuses"
    );
    let pos = position(Direction::Long, 100, "100");
    // $2 adverse on 100 shares = -$200: exactly on the line holds.
    assert!(stop.review(&pos, &mark(Some("98"))).is_empty());
    assert_eq!(stop.review(&pos, &mark(Some("97.99"))).len(), 1);
    assert!(
        stop.review(&pos, &mark(None)).is_empty(),
        "absent mark holds"
    );
    let target = EquityTarget::new(money("300")).expect("positive validates");
    assert_eq!(target.name(), "equity-target");
    assert!(target.review(&pos, &mark(Some("103"))).is_empty());
    assert_eq!(target.review(&pos, &mark(Some("103.01"))).len(), 1);
}

#[test]
fn gate_monitor_names_its_breach() {
    use crate::elements::Monitor;
    let blocked = GateMonitor {
        gate: "cooldown",
        blocked: true,
    };
    let alerts = blocked.observe(&unobserved(), &[]);
    assert_eq!(alerts.len(), 1);
    assert!(
        alerts[0].message.contains("cooldown"),
        "alert names the gate"
    );
    let open = GateMonitor {
        gate: "cooldown",
        blocked: false,
    };
    assert!(open.observe(&unobserved(), &[]).is_empty());
}

#[test]
fn sizer_adapter_sizes_through_the_trait() {
    // The composed strategy sizes through `Sizing::size`; the adapter must
    // agree with the hand-checked math above.
    let trade = sizer().for_trade(100_000.0, 100.0, 2.0);
    let qty = trade.size(
        &crate::elements::SignalOutput {
            direction: Direction::Long,
            conviction: 1.0,
            activity_score: 1.0,
            volatility: Some(0.01),
            edge: None,
            odds: None,
        },
        &unobserved(),
    );
    assert_eq!(qty, Qty::new(Decimal::from(250)));
    assert_eq!(qty.as_decimal(), Decimal::from(250));
}
