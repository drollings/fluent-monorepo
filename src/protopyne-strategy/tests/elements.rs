use super::*;
use crate::proposal::{ProposalSide, ProposalState, StructureKind};
use crate::view::{MarketData, MarketView};
use chrono::Utc;
use protopyne_types::Money;
use rust_decimal::Decimal;
use std::sync::Arc;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn signal() -> Arc<dyn Signal> {
    Arc::new(FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"))
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
        created_at: Utc::now(),
    }
}

#[test]
fn fixed_signal_rejects_bad_scores_and_ignores_the_view() {
    assert!(FixedSignal::new(Direction::Long, 1.5, 0.5).is_err());
    assert!(FixedSignal::new(Direction::Long, 0.5, f64::NAN).is_err());
    let s = signal();
    // A fixed signal carries no data dependency, so any view — even an empty
    // one — yields its scores.
    let data = MarketData::empty(30);
    let view = MarketView {
        data: &data,
        closes: &[],
        as_of: Utc::now(),
    };
    let out = s.evaluate(&view).expect("scores");
    assert_eq!(out.direction, Direction::Long);
}

#[test]
fn strategy_proposes_with_defaults() {
    let strategy = Strategy::with_signal(signal());
    assert_eq!(strategy.signal_name(), "fixed");
    let closes = [100.0, 101.0, 102.0];
    let p = strategy.propose("p1", input(&closes)).expect("proposal");
    assert_eq!(p.state, ProposalState::Proposed);
    assert_eq!(p.structure, StructureKind::Vertical);
    assert_eq!(p.contracts(), Some(1));
    assert_eq!(p.underlying, "SPY");
}

#[test]
fn optional_elements_swap_independently() {
    let strategy = Strategy::with_signal(signal())
        .with_structure(Arc::new(ExplicitStructure {
            structure: StructureKind::Condor,
        }))
        .with_sizing(Arc::new(FixedContracts { contracts: 4 }));
    let closes = [100.0, 101.0];
    let p = strategy.propose("p2", input(&closes)).expect("proposal");
    assert_eq!(p.structure, StructureKind::Condor);
    assert_eq!(p.contracts(), Some(4));
}

#[test]
fn loss_and_monitor_default_to_quiet() {
    let strategy = Strategy::with_signal(signal());
    let flat = PositionView {
        underlying: "SPY".to_owned(),
        side: Direction::Long,
        quantity: 1,
        entry_avg: money(250),
        max_loss: money(300),
        dte_days: None,
        legs: None,
        events: None,
    };
    let unmarked = MarkContext {
        basis: protopyne_types::MarkBasis::Absent,
        mark: None,
    };
    assert!(strategy.review_position(&flat, &unmarked).is_empty());
    assert!(strategy
        .observe(&PortfolioFacts::unobserved(), &[])
        .is_empty());
}

#[test]
fn limit_monitor_alerts_only_above_cap() {
    let monitor = LimitMonitor {
        portfolio: "tasty".to_owned(),
        cap: money(10_000),
    };
    let facts_at = |open_risk: i64| {
        PortfolioFacts::observed(Money::new(Decimal::from(open_risk)), None, None, None)
    };
    assert!(monitor.observe(&facts_at(100), &[]).is_empty());
    let alerts = monitor.observe(&facts_at(20_000), &[]);
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].portfolio, "tasty");
}

fn negative_money() -> Money {
    Money::new(Decimal::from(-1))
}

#[test]
fn propose_refuses_negative_economics() {
    // Negative economics would sail through caps and poison budget
    // accounting downward: each field refuses with a reason, never clamps.
    let strategy = Strategy::with_signal(signal());
    let closes = [100.0, 101.0];
    let mut bad_limit = input(&closes);
    bad_limit.limit_price = negative_money();
    let err = strategy
        .propose("p", bad_limit)
        .expect_err("negative limit");
    assert!(matches!(err, StrategyError::InvalidElement(_)));
    assert!(
        err.to_string().contains("limit"),
        "refusal names the field, got: {err}"
    );

    let mut bad_loss = input(&closes);
    bad_loss.max_loss = negative_money();
    let err = strategy
        .propose("p", bad_loss)
        .expect_err("negative max loss");
    assert!(matches!(err, StrategyError::InvalidElement(_)));
    assert!(
        err.to_string().contains("max loss"),
        "refusal names the field, got: {err}"
    );

    let mut bad_notional = input(&closes);
    bad_notional.notional = negative_money();
    let err = strategy
        .propose("p", bad_notional)
        .expect_err("negative notional");
    assert!(matches!(err, StrategyError::InvalidElement(_)));
    assert!(
        err.to_string().contains("notional"),
        "refusal names the field, got: {err}"
    );

    assert!(
        strategy.propose("p", input(&closes)).is_ok(),
        "non-negative economics still propose"
    );
}

/// The shared handle is stateless; each bind mints a fresh counter.
struct CountingSignal;

struct Counter {
    calls: u64,
}

impl Signal for CountingSignal {
    fn name(&self) -> &str {
        "counting"
    }

    fn bind(&self, _cfg: &SignalConfig) -> Result<Box<dyn SignalInstance>, StrategyError> {
        Ok(Box::new(Counter { calls: 0 }))
    }

    fn evaluate(&self, _view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        Ok(SignalOutput {
            direction: Direction::Long,
            conviction: 0.5,
            activity_score: 1.0,

            volatility: None,

            edge: None,

            odds: None,
        })
    }
}

impl SignalInstance for Counter {
    fn evaluate(&mut self, _view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        self.calls += 1;
        Ok(SignalOutput {
            direction: Direction::Long,
            conviction: self.calls as f64 / 10.0,
            activity_score: 1.0,

            volatility: None,

            edge: None,

            odds: None,
        })
    }

    fn warmup(&self) -> usize {
        0
    }
}

#[test]
fn bound_instances_own_independent_state() {
    let strategy = Strategy::with_signal(Arc::new(CountingSignal));
    let config = SignalConfig::new(protopyne_types::Timeframe::D1);
    let data = MarketData::empty(30);
    let view = MarketView {
        data: &data,
        closes: &[],
        as_of: Utc::now(),
    };
    let mut first = strategy.bind(&config).expect("binds");
    let mut second = strategy.bind(&config).expect("binds");
    let first_call = first.evaluate(&view).expect("decides");
    let second_call = second.evaluate(&view).expect("decides");
    let ninth = {
        for _ in 1..9 {
            first.evaluate(&view).expect("decides");
        }
        first.evaluate(&view).expect("decides")
    };
    let near = |actual: f64, expected: f64, why: &str| {
        assert!((actual - expected).abs() < 1e-12, "{why}: {actual}");
    };
    near(
        first_call.conviction,
        0.1,
        "the first instance counts from one",
    );
    near(
        second_call.conviction,
        0.1,
        "a second instance must start from zero",
    );
    near(ninth.conviction, 1.0, "only its own calls advance it");
    near(
        second.evaluate(&view).expect("decides").conviction,
        0.2,
        "the first instance must not advance the second",
    );
}

// --- Chain-derived economics: one selector prices the package ---

use crate::legs::LegSelector;
use crate::view::ChainSlice;
use chrono::{NaiveDate, TimeZone};
use protopyne_types::{ChainQuote, OptionContract, OptionKind, Timeframe};

fn chain_call(strike: i64, price: &str, ts: chrono::DateTime<Utc>) -> ChainQuote {
    ChainQuote::new()
        .contract(
            OptionContract::new()
                .underlying("SPY".to_owned())
                .expiry(NaiveDate::from_ymd_opt(2026, 7, 17).expect("expiry"))
                .strike(Money::new(Decimal::from(strike)))
                .kind(OptionKind::Call)
                .build(),
        )
        .bid(price.parse().expect("bid"))
        .ask(price.parse().expect("ask"))
        .timestamp(ts)
        .build()
}

#[test]
fn chain_derived_prices_the_package_and_attaches_legs() {
    let ts = Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 0).unwrap();
    let quotes = [chain_call(690, "3.50", ts), chain_call(695, "1.50", ts)];
    let chain = ChainSlice::new(&quotes, ts, "SPY").expect("quotes bind");
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let closes = [694.0];
    let view = MarketView {
        data: &data,
        closes: &closes,
        as_of: ts,
    };
    let strategy = Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    ))
    .with_structure(Arc::new(LegSelector::new().build()));
    let mut instance = strategy
        .bind(&SignalConfig::new(Timeframe::D1))
        .expect("binds");
    let proposal = strategy
        .propose_with(
            "p1",
            ProposeInput {
                strategy: "ribbon",
                underlying: "SPY",
                side: ProposalSide::Entry,
                closes: &closes,
                limit_price: money(690),
                max_loss: money(300),
                notional: money(1_000),
                dte_days: 30,
                max_dte_days: None,
                economics: EconomicsPolicy::ChainDerived,
                structure: StructureKind::Vertical,
                credit: false,
                fee_per_contract: Money::new(Decimal::new(65, 2)),
                caps: SizingCaps::default(),
                created_at: ts,
            },
            &view,
            instance.as_mut(),
        )
        .expect("chain-derived proposal");
    assert_eq!(proposal.limit_price, money(2));
    assert_eq!(proposal.max_loss, Money::new(Decimal::new(330, 2)));
    assert_eq!(proposal.notional, proposal.max_loss);
    assert_eq!(
        proposal.legs.as_ref().map(protopyne_types::SpreadLegs::len),
        Some(2)
    );
    assert!(proposal.economics.is_some());
}

/// A volatility-scaled policy with no σ to scale from sizes zero, and the
/// composed strategy refuses that zero by name rather than proposing a
/// zero-contract package.
#[test]
fn a_volatility_sized_zero_quantity_is_refused_with_a_reason() {
    let ts = Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 0).unwrap();
    let quotes = [chain_call(690, "3.50", ts), chain_call(695, "1.50", ts)];
    let chain = ChainSlice::new(&quotes, ts, "SPY").expect("quotes bind");
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let closes = [694.0];
    let view = MarketView {
        data: &data,
        closes: &closes,
        as_of: ts,
    };
    // The fixed signal reports no volatility: absence, not a zero.
    let signal = FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores");
    assert_eq!(signal.output().volatility, None);
    let strategy = Strategy::with_signal(Arc::new(signal))
        .with_sizing(Arc::new(
            VolatilityScaled::for_timeframe(10, Timeframe::M15).expect("defaults validate"),
        ))
        .with_structure(Arc::new(LegSelector::new().build()));
    let mut instance = strategy
        .bind(&SignalConfig::new(Timeframe::D1))
        .expect("binds");
    let refused = strategy
        .propose_with(
            "p1",
            ProposeInput {
                strategy: "ribbon",
                underlying: "SPY",
                side: ProposalSide::Entry,
                closes: &closes,
                limit_price: money(690),
                max_loss: money(300),
                notional: money(1_000),
                dte_days: 30,
                max_dte_days: None,
                economics: EconomicsPolicy::ChainDerived,
                structure: StructureKind::Vertical,
                credit: false,
                fee_per_contract: Money::new(Decimal::new(65, 2)),
                caps: SizingCaps::default(),
                created_at: ts,
            },
            &view,
            instance.as_mut(),
        )
        .expect_err("a zero-sized package is refused");
    assert!(
        refused.to_string().contains("quantity must be positive"),
        "the refusal names the reason, got: {refused}"
    );
}

/// The proposal carries the instrument's own DTE, so a grant window judges
/// the expiry that will actually be traded rather than the requested
/// minimum: 32 days to the listed expiry, not the 30 requested.
#[test]
fn the_proposal_carries_the_selected_expirys_dte() {
    let ts = Utc.with_ymd_and_hms(2026, 6, 15, 14, 30, 0).unwrap();
    let quotes = [chain_call(690, "3.50", ts), chain_call(695, "1.50", ts)];
    let chain = ChainSlice::new(&quotes, ts, "SPY").expect("quotes bind");
    let data = MarketData {
        bars: &[],
        chain: Some(chain),
        dte_days: 30,
        timeframe: Timeframe::D1,
    };
    let closes = [694.0];
    let view = MarketView {
        data: &data,
        closes: &closes,
        as_of: ts,
    };
    let strategy = Strategy::with_signal(Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    ))
    .with_structure(Arc::new(LegSelector::new().build()));
    let mut instance = strategy
        .bind(&SignalConfig::new(Timeframe::D1))
        .expect("binds");
    let proposal = chain_proposal(&strategy, instance.as_mut(), &closes, &view, ts, None)
        .expect("chain-derived proposal");
    assert_eq!(proposal.dte_days, 32);
    let refused = chain_proposal(&strategy, instance.as_mut(), &closes, &view, ts, Some(20))
        .expect_err("a 32-day expiry is outside a 20-day window");
    assert!(
        refused.to_string().contains("past the 20-day maximum"),
        "the refusal names the window: {refused}"
    );
}

/// One chain-derived proposal over the pinned ladder, with the request's DTE
/// window stated (`None` leaves selection unbounded).
fn chain_proposal(
    strategy: &Strategy,
    signal: &mut dyn SignalInstance,
    closes: &[f64],
    view: &MarketView<'_>,
    ts: chrono::DateTime<Utc>,
    max_dte_days: Option<u32>,
) -> Result<Proposal, StrategyError> {
    strategy.propose_with(
        "p1",
        ProposeInput {
            strategy: "ribbon",
            underlying: "SPY",
            side: ProposalSide::Entry,
            closes,
            limit_price: money(690),
            max_loss: money(300),
            notional: money(1_000),
            dte_days: 30,
            max_dte_days,
            economics: EconomicsPolicy::ChainDerived,
            structure: StructureKind::Vertical,
            credit: false,
            fee_per_contract: Money::new(Decimal::new(65, 2)),
            caps: SizingCaps::default(),
            created_at: ts,
        },
        view,
        signal,
    )
}

/// A sizing policy that always asks for more than any cap allows.
struct GreedySizing;

impl Sizing for GreedySizing {
    fn name(&self) -> &str {
        "greedy"
    }

    fn size(&self, _signal: &SignalOutput, _facts: &PortfolioFacts) -> Qty {
        Qty::new(Decimal::from(100))
    }
}

/// Containment is a reduction, never an invention: the composed strategy
/// holds a greedy policy to what the bounds permit, and leaves it standing
/// when no bounds are threaded (the pinned legacy).
#[test]
fn containment_reduces_a_greedy_policy_to_what_the_bounds_permit() {
    use crate::risk::RiskLimits;

    let closes = [100.0, 101.0, 102.0];
    let greedy = Strategy::with_signal(signal()).with_sizing(Arc::new(GreedySizing));
    // No bounds: the policy's answer stands.
    let open = greedy.propose("p1", input(&closes)).expect("proposal");
    assert_eq!(open.contracts(), Some(100));

    // The risk engine's per-trade cap binds on its own: 100 asks, 10 allowed.
    let limits = RiskLimits::new().max_contracts_per_trade(10).build();
    let mut risk_capped = input(&closes);
    risk_capped.caps = SizingCaps {
        risk: Some(&limits),
        facts: None,
    };
    let held = greedy.propose("p2", risk_capped).expect("proposal");
    assert_eq!(
        held.contracts(),
        Some(10),
        "the risk cap reduces the policy, never the reverse"
    );

    // Buying power binds through the caller's own per-unit limit (250):
    // 1,000 funds 4 contracts, which is tighter than the risk cap.
    let funded = PortfolioFacts::observed(money(0), None, Some(money(1_000)), None);
    let mut both_capped = input(&closes);
    both_capped.caps = SizingCaps {
        risk: Some(&limits),
        facts: Some(&funded),
    };
    let funded_proposal = greedy.propose("p3", both_capped).expect("proposal");
    assert_eq!(
        funded_proposal.contracts(),
        Some(4),
        "buying power at the priced limit reduces further than the risk cap"
    );

    // An unobserved bundle leaves the policy standing: absence of facts is
    // not a fact, so the strategy does not invent a reduction from it.
    let unobserved = PortfolioFacts::unobserved();
    let mut dark = input(&closes);
    dark.caps = SizingCaps {
        risk: None,
        facts: Some(&unobserved),
    };
    let standing = greedy.propose("p4", dark).expect("proposal");
    assert_eq!(standing.contracts(), Some(100));
}

/// A sizing policy that risks a fixed fraction of net liquidation at a
/// configured per-unit max loss: `floor(0.1 * 100,000 / 500) = 20`.
#[test]
fn percent_net_liq_sizes_within_observed_facts() {
    use crate::risk::RiskLimits;

    let policy = PercentNetLiq::new(0.1, money(500)).expect("valid policy");
    let signal = SignalOutput {
        direction: Direction::Long,
        conviction: 0.8,
        activity_score: 0.7,

        volatility: None,

        edge: None,

        odds: None,
    };
    let facts = PortfolioFacts {
        net_liq: Some(money(100_000)),
        ..PortfolioFacts::observed(money(0), None, None, None)
    };
    assert_eq!(
        policy.size(&signal, &facts),
        Qty::new(Decimal::from(20)),
        "a tenth of 100,000 at 500 per unit is 20 contracts"
    );

    // The composed strategy still contains the policy against the threaded
    // caps: 20 asks, the risk cap allows 10.
    let limits = RiskLimits::new().max_contracts_per_trade(10).build();
    let caps = SizingCaps {
        risk: Some(&limits),
        facts: Some(&facts),
    };
    assert_eq!(
        caps.contain(policy.size(&signal, &facts), None),
        Qty::new(Decimal::from(10)),
        "containment holds the policy to the risk cap"
    );
}

/// An unobserved fact sizes zero — downstream refuses a zero quantity with
/// a reason, so no gate can mistake it for a trade.
#[test]
fn sizing_policies_size_zero_on_unobserved_facts() {
    let signal = SignalOutput {
        direction: Direction::Long,
        conviction: 0.8,
        activity_score: 0.7,

        volatility: None,

        edge: None,

        odds: None,
    };
    let net_liq = PercentNetLiq::new(0.1, money(500)).expect("valid policy");
    let buying_power = BuyingPower::new(0.5, money(250)).expect("valid policy");
    for facts in [
        PortfolioFacts::unobserved(),
        PortfolioFacts::observed(money(0), None, None, None),
    ] {
        assert_eq!(
            net_liq.size(&signal, &facts),
            Qty::new(Decimal::ZERO),
            "no net liq observed means no size"
        );
        assert_eq!(
            buying_power.size(&signal, &facts),
            Qty::new(Decimal::ZERO),
            "no buying power observed means no size"
        );
    }
    // Each policy reads only its own fact: buying power observed (but no
    // net liq) funds the buying-power policy while the net-liq policy
    // stays at zero.
    let split = PortfolioFacts::observed(money(0), None, Some(money(1_000)), None);
    assert_eq!(net_liq.size(&signal, &split), Qty::new(Decimal::ZERO));
    assert_eq!(
        buying_power.size(&signal, &split),
        Qty::new(Decimal::from(2))
    );
}

/// A policy built past its validating constructor still fails closed: a NaN
/// fraction or a non-positive per-unit price sizes zero, never nonsense.
#[test]
fn sizing_policies_fail_closed_past_their_constructors() {
    assert!(PercentNetLiq::new(0.0, money(500)).is_err());
    assert!(PercentNetLiq::new(1.5, money(500)).is_err());
    assert!(PercentNetLiq::new(f64::NAN, money(500)).is_err());
    assert!(PercentNetLiq::new(0.1, money(0)).is_err());
    assert!(BuyingPower::new(0.0, money(250)).is_err());
    assert!(BuyingPower::new(0.5, money(-1)).is_err());

    let signal = SignalOutput {
        direction: Direction::Long,
        conviction: 0.8,
        activity_score: 0.7,

        volatility: None,

        edge: None,

        odds: None,
    };
    let facts = PortfolioFacts {
        net_liq: Some(money(100_000)),
        ..PortfolioFacts::observed(money(0), None, Some(money(1_000)), None)
    };
    let nan = PercentNetLiq {
        fraction: f64::NAN,
        per_unit_max_loss: money(500),
    };
    assert_eq!(nan.size(&signal, &facts), Qty::new(Decimal::ZERO));
}

/// Buying power funds through the configured per-unit price:
/// `floor(0.5 * 1,000 / 250) = 2`.
#[test]
fn buying_power_sizes_within_observed_facts() {
    let policy = BuyingPower::new(0.5, money(250)).expect("valid policy");
    let signal = SignalOutput {
        direction: Direction::Long,
        conviction: 0.8,
        activity_score: 0.7,

        volatility: None,

        edge: None,

        odds: None,
    };
    let facts = PortfolioFacts::observed(money(0), None, Some(money(1_000)), None);
    assert_eq!(
        policy.size(&signal, &facts),
        Qty::new(Decimal::from(2)),
        "half of 1,000 at 250 per unit is 2 contracts"
    );
}

fn live_position() -> PositionView<'static> {
    PositionView {
        underlying: "SPY".to_owned(),
        side: Direction::Long,
        quantity: 2,
        entry_avg: money(250),
        max_loss: money(600),
        dte_days: Some(30),
        legs: None,
        events: None,
    }
}

fn marked(price: i64) -> MarkContext {
    MarkContext {
        basis: protopyne_types::MarkBasis::ChainMid,
        mark: Some(money(price)),
    }
}

/// A long two-lot entered at 250 marked at 100 is down 300: past a 200
/// stop, short of a 400 target.
#[test]
fn stop_fires_past_its_threshold_target_holds() {
    let position = live_position();
    let mark = marked(100);
    let stop = StopLoss::new(money(200)).expect("valid stop");
    let target = ProfitTarget::new(money(400)).expect("valid target");
    let stops = stop.review(&position, &mark);
    assert_eq!(stops.len(), 1, "the stop fires once past its threshold");
    assert_eq!(stops[0].kind, "stop");
    assert!(
        target.review(&position, &mark).is_empty(),
        "the target holds below its threshold"
    );
}

/// The mirror book (marked at 500, up 500) fires the 400 target while the
/// 200 stop holds.
#[test]
fn target_fires_past_its_threshold_stop_holds() {
    let position = live_position();
    let mark = marked(500);
    let stop = StopLoss::new(money(200)).expect("valid stop");
    let target = ProfitTarget::new(money(400)).expect("valid target");
    assert!(
        stop.review(&position, &mark).is_empty(),
        "the stop holds on a winning book"
    );
    let targets = target.review(&position, &mark);
    assert_eq!(targets.len(), 1, "the target fires once past its threshold");
    assert_eq!(targets[0].kind, "profit-target");
}

/// A book exactly on a threshold holds: the boundary is not a trigger.
#[test]
fn thresholds_hold_exactly_on_the_boundary() {
    let position = live_position();
    // Down exactly 200 against a 200 stop; up exactly 400 against a 400
    // target on the mirror mark.
    let stop = StopLoss::new(money(200)).expect("valid stop");
    let target = ProfitTarget::new(money(400)).expect("valid target");
    assert!(
        stop.review(&position, &marked(150)).is_empty(),
        "exactly on the stop is not past it"
    );
    assert!(
        target.review(&position, &marked(450)).is_empty(),
        "exactly on the target is not past it"
    );
}

/// A short book gains when the mark prints below entry: marked at 25
/// against a 250 entry is up 450 on the two-lot.
#[test]
fn short_books_mirror_the_sign() {
    let short = PositionView {
        side: Direction::Short,
        ..live_position()
    };
    let stop = StopLoss::new(money(200)).expect("valid stop");
    let target = ProfitTarget::new(money(400)).expect("valid target");
    assert!(
        stop.review(&short, &marked(25)).is_empty(),
        "a short winning book holds its stop"
    );
    assert_eq!(
        target.review(&short, &marked(25)).len(),
        1,
        "a short winning book fires its target"
    );
    assert_eq!(
        stop.review(&short, &marked(500)).len(),
        1,
        "a short losing book fires its stop"
    );
}

/// The time rule fires below its DTE floor, holds above it and on it, and
/// holds when the book names no expiry.
#[test]
fn time_stop_fires_below_its_floor_only() {
    let rule = TimeStop::new(7);
    let mark = marked(250);
    let below = PositionView {
        dte_days: Some(6),
        ..live_position()
    };
    assert_eq!(rule.review(&below, &mark).len(), 1);
    assert_eq!(rule.review(&below, &mark)[0].kind, "time-stop");
    let on = PositionView {
        dte_days: Some(7),
        ..live_position()
    };
    assert!(
        rule.review(&on, &mark).is_empty(),
        "exactly on the floor is not below it"
    );
    assert!(
        rule.review(&live_position(), &mark).is_empty(),
        "thirty days out holds a seven-day floor"
    );
    let dateless = PositionView {
        dte_days: None,
        ..live_position()
    };
    assert!(
        rule.review(&dateless, &mark).is_empty(),
        "an unknown expiry never triggers the time rule"
    );
}

/// An absent mark acts on nothing: a book down 600 past every price
/// threshold still holds every rule, since absence is not a trigger and
/// no rule invents a P&L.
#[test]
fn absent_marks_hold_every_rule() {
    let position = PositionView {
        dte_days: Some(1),
        ..live_position()
    };
    let absent = MarkContext {
        basis: protopyne_types::MarkBasis::Absent,
        mark: None,
    };
    let stop = StopLoss::new(money(200)).expect("valid stop");
    let target = ProfitTarget::new(money(400)).expect("valid target");
    let time = TimeStop::new(7);
    assert!(stop.review(&position, &absent).is_empty());
    assert!(target.review(&position, &absent).is_empty());
    assert!(
        time.review(&position, &absent).is_empty(),
        "even the time rule waits for a mark — a dark chain prices no exit"
    );
}

/// Thresholds refuse nonsense at construction: a non-positive stop or
/// target can never be a trigger.
#[test]
fn loss_thresholds_refuse_at_construction() {
    assert!(StopLoss::new(money(0)).is_err());
    assert!(StopLoss::new(money(-50)).is_err());
    assert!(ProfitTarget::new(money(0)).is_err());
    assert!(ProfitTarget::new(money(-50)).is_err());
}

/// Rules compose in firing order: a book past both the stop and the time
/// floor reports the stop first.
#[test]
fn loss_rules_compose_in_firing_order() {
    let set = LossSet::new(vec![
        Arc::new(StopLoss::new(money(200)).expect("valid stop")),
        Arc::new(ProfitTarget::new(money(400)).expect("valid target")),
        Arc::new(TimeStop::new(7)),
    ]);
    let beaten = PositionView {
        dte_days: Some(3),
        ..live_position()
    };
    let actions = set.review(&beaten, &marked(100));
    let kinds: Vec<&str> = actions.iter().map(|action| action.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec!["stop", "time-stop"],
        "the stop and the time rule fire in order; the target holds"
    );
    assert!(
        set.review(&live_position(), &marked(250)).is_empty(),
        "a flat marked book holds the whole set"
    );
}

/// The deployed momentum composition installs the calibrated elements:
/// the kernel trend signal, the buying-power sizer, the ordered loss set.
#[test]
fn the_momentum_composition_installs_the_calibrated_elements() {
    let strategy = Strategy::momentum().expect("the deployment composition builds");
    assert_eq!(strategy.signal_name(), "kernel");
    assert_eq!(strategy.sizing_name(), "buying-power");
    assert_eq!(strategy.loss_name(), "set");
    // A flat marked book holds the whole set — the loss rules arrive
    // installed, not merely constructed.
    let flat = PositionView {
        underlying: "SPY".to_owned(),
        side: Direction::Long,
        quantity: 2,
        entry_avg: money(250),
        max_loss: money(600),
        dte_days: Some(30),
        legs: None,
        events: None,
    };
    let marked = MarkContext {
        basis: protopyne_types::MarkBasis::ChainMid,
        mark: Some(money(250)),
    };
    assert!(strategy.review_position(&flat, &marked).is_empty());
}
