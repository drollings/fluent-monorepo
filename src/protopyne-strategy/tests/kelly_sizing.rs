//! Capped fractional-Kelly sizing: the control, the trigger, the cap, and the
//! containment property.
//!
//! The calibration shape is the one every trigger in this phase carries: a
//! **control** that must not move (zero edge ⇒ zero size), a **trigger** that
//! must equal a hand computation, a **negative** case that must never flip
//! direction, and a **cap** case that must stop at the cap — never the full
//! base. The property sweep then asserts the size never exceeds `base` over
//! generated inputs rather than a handful of points.
//!
//! Research-only: this module compiles only under the `research` feature, and
//! the build-matrix gate pins that the lean build cannot name the type.

use super::*;

use crate::risk::PortfolioFacts;
use protopyne_types::Qty;
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

/// A signal reporting an observed edge and odds pair.
fn signal(edge: Option<f64>, odds: Option<f64>) -> SignalOutput {
    SignalOutput {
        direction: Direction::Long,
        conviction: 0.8,
        activity_score: 0.7,
        volatility: None,
        edge,
        odds,
    }
}

fn policy(base: u32, fraction: f64, cap: f64) -> FractionalKelly {
    FractionalKelly::new(base, fraction, cap).expect("valid policy")
}

fn size(policy: &FractionalKelly, edge: Option<f64>, odds: Option<f64>) -> u32 {
    policy
        .size(&signal(edge, odds), &PortfolioFacts::unobserved())
        .as_decimal()
        .to_u32()
        .expect("whole contracts")
}

/// Deterministic generator: a small LCG so the sweep is reproducible without
/// a random-number dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn unit(&mut self) -> f64 {
        (self.next() % 1_000_000) as f64 / 1_000_000.0
    }
}

// ── Control: zero edge must not move the size ─────────────────────────────

#[test]
fn zero_edge_sizes_zero_exactly() {
    let kelly = policy(100, 0.5, 1.0);
    assert_eq!(size(&kelly, Some(0.0), Some(2.0)), 0);
    // A negative-zero edge is still no edge.
    assert_eq!(size(&kelly, Some(-0.0), Some(2.0)), 0);
}

// ── Trigger: a known edge prices the hand-computed size ────────────────────

#[test]
fn a_known_edge_prices_the_hand_computed_size() {
    // f* = 0.2 / 2.0 = 0.1; min(1.0, 0.5 * 0.1) = 0.05; floor(100 * 0.05) = 5.
    assert_eq!(size(&policy(100, 0.5, 1.0), Some(0.2), Some(2.0)), 5);
    // f* = 0.3 / 2.0 = 0.15; min(1.0, 1.0 * 0.15) = 0.15; floor(10 * 0.15) = 1.
    assert_eq!(size(&policy(10, 1.0, 1.0), Some(0.3), Some(2.0)), 1);
}

// ── Negative edge: never a short flip ─────────────────────────────────────

#[test]
fn a_negative_edge_sizes_zero_in_either_direction() {
    let kelly = policy(100, 0.5, 1.0);
    assert_eq!(size(&kelly, Some(-0.2), Some(2.0)), 0);
    // Direction is the signal's business: a short signal with a negative edge
    // still sizes zero — the sizer never flips a direction into a trade.
    let short = SignalOutput {
        direction: Direction::Short,
        ..signal(Some(-0.2), Some(2.0))
    };
    assert_eq!(
        kelly
            .size(&short, &PortfolioFacts::unobserved())
            .as_decimal()
            .to_u32()
            .expect("whole contracts"),
        0
    );
}

// ── Cap: a wildly favourable edge stops at the cap ────────────────────────

#[test]
fn a_wildly_favourable_edge_stops_at_the_cap_never_the_base() {
    // f* = 10.0 / 1.0 = 10; min(0.5, 1.0 * 10) = 0.5; floor(10 * 0.5) = 5.
    let capped = size(&policy(10, 1.0, 0.5), Some(10.0), Some(1.0));
    assert_eq!(capped, 5);
    assert_eq!(capped, (10.0_f64 * 0.5).floor() as u32);
    assert!(capped < 10, "the cap binds before the base: {capped}");
}

// ── Absence and nonsense: missing or non-finite input sizes zero ──────────

#[test]
fn missing_or_non_finite_input_sizes_zero() {
    let kelly = policy(100, 0.5, 1.0);
    assert_eq!(size(&kelly, None, Some(2.0)), 0);
    assert_eq!(size(&kelly, Some(0.2), None), 0);
    assert_eq!(size(&kelly, None, None), 0);
    assert_eq!(size(&kelly, Some(f64::NAN), Some(2.0)), 0);
    assert_eq!(size(&kelly, Some(0.2), Some(f64::NAN)), 0);
    assert_eq!(size(&kelly, Some(f64::INFINITY), Some(2.0)), 0);
    assert_eq!(size(&kelly, Some(0.2), Some(f64::INFINITY)), 0);
    // Non-positive odds are not a price anyone observed.
    assert_eq!(size(&kelly, Some(0.2), Some(0.0)), 0);
    assert_eq!(size(&kelly, Some(0.2), Some(-2.0)), 0);
}

// ── Construction: fraction and cap live in (0, 1] ─────────────────────────

#[test]
fn nonsense_fraction_or_cap_is_refused_at_construction() {
    for (fraction, cap) in [
        (0.0, 1.0),
        (-0.5, 1.0),
        (1.5, 1.0),
        (f64::NAN, 1.0),
        (0.5, 0.0),
        (0.5, -1.0),
        (0.5, 1.5),
        (0.5, f64::NAN),
        (0.5, f64::INFINITY),
    ] {
        assert!(
            FractionalKelly::new(10, fraction, cap).is_err(),
            "fraction {fraction}, cap {cap} must be refused"
        );
    }
    // A literal-built policy past the validating constructor still fails
    // closed rather than sizing an infinity.
    let smuggled = FractionalKelly {
        base: 100,
        fraction: f64::NAN,
        cap: 1.0,
    };
    assert_eq!(size(&smuggled, Some(0.2), Some(2.0)), 0);
}

// ── Property: the size never exceeds the base, and caps still contain ──────

#[test]
fn across_a_sweep_size_never_exceeds_the_base_or_the_caps() {
    let mut lcg = Lcg(0x8e77);
    for _ in 0..500 {
        let base = 1 + (lcg.next() % 100) as u32;
        let fraction = 0.01 + lcg.unit() * 0.99;
        let cap = 0.01 + lcg.unit() * 0.99;
        let kelly = policy(base, fraction, cap);
        // Edges across three orders of magnitude, odds across two; negatives
        // included so the sweep also covers the never-flip arm.
        let edge = (lcg.unit() - 0.3) * 10.0;
        let odds = 0.5 + lcg.unit() * 50.0;
        let sized = size(&kelly, Some(edge), Some(odds));
        assert!(
            sized <= base,
            "base {base}, fraction {fraction}, cap {cap}, edge {edge}, odds {odds}: {sized} > {base}"
        );
        // The composed strategy's containment still applies on top.
        let caps = SizingCaps {
            risk: Some(
                &crate::risk::RiskLimits::new()
                    .max_contracts_per_trade(7)
                    .build(),
            ),
            facts: None,
        };
        let contained = caps.contain(Qty::new(Decimal::from(sized)), None);
        let contained_u32 = contained.as_decimal().to_u32().expect("whole contracts");
        assert!(
            contained_u32 <= sized.min(7),
            "containment must only ever reduce: {contained_u32} > min({sized}, 7)"
        );
    }
}
