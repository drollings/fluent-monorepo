//! Volatility-scaled sizing: the control, the trigger, the floor, absence, and
//! the containment property.
//!
//! The calibration shape is the one every trigger in this phase carries: a
//! **control** that must not move (σ == target ⇒ `base`), a **trigger** that must
//! move in exactly one direction (σ doubles ⇒ half), a **floor** that must stop
//! the growth (σ → 0 ⇒ `base`), and an **absence** case that must size zero —
//! never a default, never an infinity. The property sweep then asserts the
//! reduce-only invariant over generated inputs rather than a handful of points.

use super::*;

use crate::risk::PortfolioFacts;
use protopyne_types::Qty;
use rust_decimal::prelude::ToPrimitive as _;

/// A signal reporting `volatility`.
fn signal(volatility: Option<f64>) -> SignalOutput {
    SignalOutput {
        direction: Direction::Long,
        conviction: 0.8,
        activity_score: 0.7,
        volatility,
        edge: None,
        odds: None,
    }
}

fn policy(base: u32, target_vol: f64, vol_floor: f64) -> VolatilityScaled {
    VolatilityScaled::new(base, target_vol, vol_floor).expect("valid policy")
}

fn size(policy: &VolatilityScaled, volatility: Option<f64>) -> u32 {
    policy
        .size(&signal(volatility), &PortfolioFacts::unobserved())
        .as_decimal()
        .to_u32()
        .expect("whole contracts")
}

/// Deterministic generator: a small LCG so the 1,000-case control is
/// reproducible without a random-number dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

// ── Control: σ at the target must not move the size ───────────────────────

#[test]
fn a_thousand_cases_at_the_target_size_exactly_the_base() {
    let mut lcg = Lcg(0x5eed);
    for case in 0..1_000 {
        let base = 1 + (lcg.next() % 1_000) as u32;
        // Targets across four orders of magnitude, including values whose
        // decimal expansion is not exact in binary.
        let target = 1e-4 * f64::from(1 + (lcg.next() % 9_999) as u32);
        let vol_floor = target * 0.5;
        let policy = policy(base, target, vol_floor);
        assert_eq!(
            size(&policy, Some(target)),
            base,
            "case {case}: base={base} target={target} must size exactly base"
        );
    }
}

// ── Trigger: doubling σ halves the size; halving it cannot grow ───────────

#[test]
fn a_doubled_sigma_halves_the_size_and_a_halved_sigma_never_grows_it() {
    let target = 0.01;
    let policy = policy(10, target, target * 0.5);
    assert_eq!(size(&policy, Some(target)), 10, "at target: base");
    assert_eq!(size(&policy, Some(2.0 * target)), 5, "doubled: half");
    assert_eq!(
        size(&policy, Some(0.5 * target)),
        10,
        "halved: clamped to base, never 2×base"
    );
    assert_eq!(size(&policy, Some(target * 0.25)), 10, "calmer still: base");
}

// ── Floor: collapsing σ stops at base, never unbounded ────────────────────

#[test]
fn a_collapsing_sigma_returns_the_base_and_never_an_infinity() {
    let target = 0.01;
    let policy = policy(10, target, target);
    for sigma in [target * 0.5, 1e-9, 0.0] {
        assert_eq!(
            size(&policy, Some(sigma)),
            10,
            "σ={sigma} must size base, not exceed it"
        );
    }
}

// ── Absence: no measurement sizes zero ────────────────────────────────────

#[test]
fn an_absent_sigma_sizes_zero_and_the_composed_strategy_refuses_it() {
    let policy = policy(10, 0.01, 0.005);
    assert_eq!(size(&policy, None), 0, "absence sizes zero");
    // Non-finite σ is absence too: it must never reach the arithmetic.
    for sigma in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(size(&policy, Some(sigma)), 0, "σ={sigma} sizes zero");
    }
    // The composed refusal is the economics gate's: a zero-contract package is
    // refused by name, so no gate downstream can mistake it for a trade.
    let facts = economics_zero_quantity_refusal();
    assert_eq!(facts, "quantity must be positive");
}

/// The composed path's refusal for a zero quantity, read from the same gate the
/// strategy hits: `spread_economics` refuses a zero package by name before it
/// prices anything, so a zero-sized proposal is an error with a reason rather
/// than a trade for nothing.
fn economics_zero_quantity_refusal() -> String {
    use crate::economics::{spread_economics, EconRefusal};
    use protopyne_types::{LegSide, Money, OptionContract, OptionKind, SpreadLeg, SpreadLegs};
    let contract = OptionContract::new()
        .underlying("SPY".to_owned())
        .expiry(chrono::NaiveDate::from_ymd_opt(2026, 7, 17).expect("expiry"))
        .strike(Money::new(Decimal::from(690)))
        .kind(OptionKind::Call)
        .build();
    let legs = SpreadLegs::try_new(vec![SpreadLeg::new()
        .contract(contract)
        .side(LegSide::Long)
        .build()])
    .expect("one-leg spread");
    let refused = spread_economics(&legs, &[], Qty::zero(), Money::zero());
    assert!(
        matches!(refused, Err(EconRefusal::ZeroQuantity)),
        "a zero-quantity package must refuse: {refused:?}"
    );
    EconRefusal::ZeroQuantity.to_string()
}

// ── Property: reduce-only, and containment keeps it ───────────────────────

#[test]
fn across_a_sweep_size_never_exceeds_the_base_or_the_caps() {
    let mut lcg = Lcg(0xc0ffee);
    for _ in 0..500 {
        let base = 1 + (lcg.next() % 500) as u32;
        let target = 1e-3 * f64::from(1 + (lcg.next() % 500) as u32);
        let vol_floor = target * f64::from(1 + (lcg.next() % 10) as u32) / 10.0;
        let sigma = target * f64::from(1 + (lcg.next() % 400) as u32) / 100.0;
        let policy = policy(base, target, vol_floor);
        let sized = size(&policy, Some(sigma));
        assert!(
            sized <= base,
            "base={base} target={target} floor={vol_floor} σ={sigma} sized {sized}"
        );
        let caps = SizingCaps {
            risk: Some(
                &crate::risk::RiskLimits::new()
                    .max_contracts_per_trade(7)
                    .build(),
            ),
            facts: None,
        };
        let contained = caps.contain(Qty::new(Decimal::from(sized)), None);
        assert!(
            contained.as_decimal().to_u32().expect("whole") <= 7.min(sized),
            "containment must only ever reduce"
        );
    }
}

// ── Construct-time validation ─────────────────────────────────────────────

#[test]
fn nonsense_thresholds_are_refused_at_construction() {
    for (target, floor) in [
        (0.0, 0.005),
        (-0.01, 0.005),
        (f64::NAN, 0.005),
        (f64::INFINITY, 0.005),
        (0.01, 0.0),
        (0.01, -0.005),
        (0.01, f64::NAN),
        (0.01, f64::INFINITY),
    ] {
        assert!(
            VolatilityScaled::new(10, target, floor).is_err(),
            "target={target} floor={floor} must refuse"
        );
    }
    assert_eq!(policy(10, 0.01, 0.005).name(), "volatility-scaled");
}

// ── Calibration record: the documented defaults against the observed tape ──

/// The documented one-minute target is the tape's own median, and the
/// per-timeframe defaults are that number scaled by √t — not a backtest fit.
#[test]
fn the_documented_defaults_sit_inside_the_observed_sigma_band() {
    use protopyne_types::Timeframe;
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../protopyne-kernels/tests/data/SPY_1M.csv");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("vendored tape {}: {error}", path.display()));
    let mut closes = Vec::new();
    for line in text.lines().skip(1) {
        let mut fields = line.split(',');
        let _time = fields.next();
        let _open = fields.next();
        let _high = fields.next();
        let _low = fields.next();
        let Some(close) = fields.next() else { continue };
        closes.push(close.parse::<f64>().expect("numeric close"));
    }
    assert!(closes.len() > 200, "the tape is a real series");
    let returns: Vec<f64> = closes
        .windows(2)
        .filter(|pair| pair[0] > 0.0 && pair[0].is_finite() && pair[1].is_finite())
        .map(|pair| (pair[1] - pair[0]) / pair[0])
        .collect();
    let window = crate::signals_kernel::VOLATILITY_WINDOW;
    let mut sigmas: Vec<f64> = returns
        .windows(window)
        .map(|slice| {
            let mean = slice.iter().sum::<f64>() / slice.len() as f64;
            let variance = slice
                .iter()
                .map(|value| (value - mean).powi(2))
                .sum::<f64>()
                / slice.len() as f64;
            variance.sqrt()
        })
        .collect();
    sigmas.sort_by(f64::total_cmp);
    let percentile = |p: f64| sigmas[((sigmas.len() - 1) as f64 * p) as usize];
    let (p10, median, p90) = (percentile(0.10), percentile(0.50), percentile(0.90));
    let target = VolatilityScaled::default_target_vol(Timeframe::M1);
    assert!(
        target >= p10 && target <= p90,
        "the documented 1m target {target} must sit inside the observed band [{p10}, {p90}]"
    );
    assert!(
        (target - median).abs() / median < 0.10,
        "the documented 1m target {target} must track the observed median {median}"
    );
    // √t scaling keeps every timeframe's target inside that timeframe's
    // rescaled band, and the floor never exceeds its own target.
    for timeframe in [
        Timeframe::M1,
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::H1,
        Timeframe::D1,
    ] {
        let scale = (timeframe.total_seconds() as f64 / 60.0).sqrt();
        let scaled_target = VolatilityScaled::default_target_vol(timeframe);
        assert!(
            (scaled_target - target * scale).abs() < 1e-12,
            "{timeframe:?}: the default is the √t-scaled measurement"
        );
        assert!(
            scaled_target >= p10 * scale && scaled_target <= p90 * scale,
            "{timeframe:?}: target {scaled_target} inside [{}, {}]",
            p10 * scale,
            p90 * scale
        );
        assert!(
            VolatilityScaled::default_vol_floor(timeframe)
                <= VolatilityScaled::default_target_vol(timeframe),
            "{timeframe:?}: the floor sits at or below the target"
        );
    }
    // And the defaults really do hold `base` at their own target.
    let policy = VolatilityScaled::for_timeframe(10, Timeframe::M15).expect("defaults validate");
    assert_eq!(
        size(
            &policy,
            Some(VolatilityScaled::default_target_vol(Timeframe::M15))
        ),
        10,
        "the documented default sizes base at its own target"
    );
}
