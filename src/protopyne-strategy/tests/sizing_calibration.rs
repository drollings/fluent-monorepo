//! Sizing-policy calibration: control groups plus frozen treatment
//! tallies.
//!
//! A sizing policy is untrusted until its controls size zero and its
//! treatment tallies are frozen. The controls below are chosen so that a
//! naive sizer would allocate on them (zero or negative funds, absent facts,
//! a poisoned greek); the calibrated policies must not. The treatment
//! tallies freeze exact sizes plus the containment identity
//! (`size × per-unit max loss ≤ min(risk cap, buying power)`) on every
//! treatment; a threshold edit changes a tally and fails.
//!
//! Only the tallies are persisted (as a golden); the thresholds are literals
//! here, with a fingerprint recorded beside the tallies so that changing a
//! literal without re-freezing also fails. Deploying a calibrated policy as
//! the default without the tally fails the ship gate at the bottom.

use super::*;
use std::path::PathBuf;

use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::risk::{PortfolioFacts, RiskLimits};
use protopyne_types::{Money, Qty};

// Thresholds under calibration. Editing any of these must move a tally.
const PERCENT_FRACTION: f64 = 0.1;
const PER_UNIT_MAX_LOSS: i64 = 500;
const BP_FRACTION: f64 = 0.5;
const PER_UNIT_PRICE: i64 = 250;
// The harness caps the identity names: the risk engine's per-trade value cap
// and the buying power every treatment carries.
const RISK_VALUE_CAP: i64 = 30_000;
const RISK_CONTRACT_CAP: u64 = 1_000;
const TREATMENT_BUYING_POWER: i64 = 100_000;

fn money(value: i64) -> Money {
    Money::new(Decimal::from(value))
}

fn signal() -> SignalOutput {
    SignalOutput {
        direction: Direction::Long,
        conviction: 0.8,
        activity_score: 0.7,
    }
}

fn percent_policy() -> PercentNetLiq {
    PercentNetLiq::new(PERCENT_FRACTION, money(PER_UNIT_MAX_LOSS)).expect("valid fraction")
}

fn buying_power_policy() -> BuyingPower {
    BuyingPower::new(BP_FRACTION, money(PER_UNIT_PRICE)).expect("valid fraction")
}

fn risk_limits() -> RiskLimits {
    RiskLimits::new()
        .max_contracts_per_trade(RISK_CONTRACT_CAP)
        .max_loss_per_trade(RISK_VALUE_CAP as f64)
        .build()
}

fn thresholds_fingerprint() -> String {
    format!(
        "percent_fraction={PERCENT_FRACTION};per_unit_max_loss={PER_UNIT_MAX_LOSS};\
         bp_fraction={BP_FRACTION};per_unit_price={PER_UNIT_PRICE};\
         risk_value_cap={RISK_VALUE_CAP};risk_contract_cap={RISK_CONTRACT_CAP};\
         treatment_buying_power={TREATMENT_BUYING_POWER}"
    )
}

// ── Control group: must not size above zero ───────────────────────────────

fn unobserved() -> PortfolioFacts {
    PortfolioFacts::unobserved()
}

#[test]
fn an_unobserved_bundle_sizes_zero() {
    let facts = unobserved();
    assert_eq!(percent_policy().size(&signal(), &facts), Qty::zero());
    assert_eq!(buying_power_policy().size(&signal(), &facts), Qty::zero());
}

#[test]
fn zero_funds_size_zero() {
    let broke = PortfolioFacts::observed(money(0), None, Some(money(0)), Some(money(0)));
    assert_eq!(
        percent_policy().size(&signal(), &broke),
        Qty::zero(),
        "zero net liq funds nothing"
    );
    assert_eq!(
        buying_power_policy().size(&signal(), &broke),
        Qty::zero(),
        "zero buying power funds nothing"
    );
}

#[test]
fn negative_net_liq_sizes_zero() {
    let facts = PortfolioFacts {
        net_liq: Some(money(-5_000)),
        ..PortfolioFacts::observed(money(0), None, Some(money(100_000)), None)
    };
    assert_eq!(percent_policy().size(&signal(), &facts), Qty::zero());
}

#[test]
fn a_poisoned_greek_sizes_zero() {
    // A non-finite number anywhere in the bundle poisons sizing arithmetic:
    // the policies read only their own fact, but neither trusts a bundle
    // that cannot do math.
    let facts = PortfolioFacts {
        net_delta: Some(f64::NAN),
        ..PortfolioFacts::observed(
            money(0),
            None,
            Some(money(TREATMENT_BUYING_POWER)),
            Some(money(100_000)),
        )
    };
    assert_eq!(percent_policy().size(&signal(), &facts), Qty::zero());
    assert_eq!(buying_power_policy().size(&signal(), &facts), Qty::zero());
}

// ── Treatment: frozen sizes plus the containment identity ─────────────────

struct TreatmentCase {
    name: &'static str,
    size: u64,
    committed: i64,
    identity_holds: bool,
}

fn check_identity(size: Qty, per_unit: Money, buying_power: Money, cap: Decimal) -> (i64, bool) {
    let committed = size.as_decimal() * per_unit.as_decimal();
    let holds = committed <= cap.min(buying_power.as_decimal());
    (committed.to_i64().expect("committed fits"), holds)
}

fn treatment_tallies() -> Value {
    let signal = signal();
    let cap = Decimal::from_f64_retain(RISK_VALUE_CAP as f64).expect("finite cap");
    let mut percent = Vec::new();
    for net_liq in [10_000, 50_000, 100_000, 250_000] {
        let facts = PortfolioFacts {
            net_liq: Some(money(net_liq)),
            ..PortfolioFacts::observed(money(0), None, Some(money(TREATMENT_BUYING_POWER)), None)
        };
        let size = percent_policy().size(&signal, &facts);
        let (committed, identity_holds) = check_identity(
            size,
            money(PER_UNIT_MAX_LOSS),
            money(TREATMENT_BUYING_POWER),
            cap,
        );
        assert!(
            identity_holds,
            "treatment net_liq={net_liq} must satisfy the identity"
        );
        percent.push(TreatmentCase {
            name: match net_liq {
                10_000 => "ten_k",
                50_000 => "fifty_k",
                100_000 => "hundred_k",
                _ => "two_fifty_k",
            },
            size: size.as_decimal().to_u64().expect("whole contracts"),
            committed,
            identity_holds,
        });
    }
    let mut power = Vec::new();
    for buying_power in [1_000, 4_000, 10_000] {
        let facts = PortfolioFacts::observed(
            money(0),
            None,
            Some(money(buying_power)),
            Some(money(100_000)),
        );
        let size = buying_power_policy().size(&signal, &facts);
        let (committed, identity_holds) =
            check_identity(size, money(PER_UNIT_PRICE), money(buying_power), cap);
        assert!(
            identity_holds,
            "treatment buying_power={buying_power} must satisfy the identity"
        );
        power.push(TreatmentCase {
            name: match buying_power {
                1_000 => "one_k",
                4_000 => "four_k",
                _ => "ten_k",
            },
            size: size.as_decimal().to_u64().expect("whole contracts"),
            committed,
            identity_holds,
        });
    }
    // The composed strategy contains the real policy output against the
    // threaded caps: loose caps leave the treatment sizes standing, a tight
    // contract cap binds them. `contain` is the exact call `propose_with`
    // makes after the policy returns.
    let loose_limits = risk_limits();
    let loose = SizingCaps {
        risk: Some(&loose_limits),
        facts: None,
    };
    let tight_limits = RiskLimits::new().max_contracts_per_trade(5).build();
    let tight = SizingCaps {
        risk: Some(&tight_limits),
        facts: None,
    };
    for case in percent.iter().chain(power.iter()) {
        let size = Qty::new(Decimal::from(case.size));
        assert_eq!(
            loose.contain(size, None),
            size,
            "loose caps leave treatment {} standing",
            case.name
        );
    }
    assert_eq!(
        tight.contain(Qty::new(Decimal::from(20)), None),
        Qty::new(Decimal::from(5)),
        "a tight contract cap binds the policy"
    );
    let case = |case: &TreatmentCase| {
        json!({
            "size": case.size,
            "committed": case.committed,
            "identity_holds": case.identity_holds,
        })
    };
    json!({
        "percent_net_liq": {
            "ten_k": case(&percent[0]),
            "fifty_k": case(&percent[1]),
            "hundred_k": case(&percent[2]),
            "two_fifty_k": case(&percent[3]),
        },
        "buying_power": {
            "one_k": case(&power[0]),
            "four_k": case(&power[1]),
            "ten_k": case(&power[2]),
        },
        "thresholds": thresholds_fingerprint(),
        "ship_state": SHIP_STATE,
        "operator_accepted": OPERATOR_ACCEPTED,
    })
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("sizing_tally.json")
}

fn recorded_tally() -> Value {
    let path = golden_path();
    let text = if std::env::var("PROTOPYNE_RECORD").is_ok() {
        let tally = treatment_tallies();
        let text = serde_json::to_string_pretty(&tally).expect("serializable");
        std::fs::create_dir_all(path.parent().expect("golden has a parent"))
            .expect("baseline directory exists");
        std::fs::write(&path, &text).expect("the tally records");
        text
    } else {
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("golden {} records first", path.display()))
    };
    serde_json::from_str(&text).expect("the tally is JSON")
}

#[test]
fn the_frozen_tally_matches() {
    let actual = serde_json::to_string_pretty(&treatment_tallies()).expect("serializable");
    let recorded = recorded_tally();
    assert_eq!(
        serde_json::to_string_pretty(&recorded).expect("serializable"),
        actual,
        "the calibration tally must match the frozen golden"
    );
}

#[test]
fn the_threshold_fingerprint_matches_the_golden() {
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        return;
    }
    let recorded = recorded_tally();
    assert_eq!(
        recorded["thresholds"]
            .as_str()
            .expect("fingerprint present"),
        thresholds_fingerprint(),
        "a threshold changed without re-freezing the tally"
    );
}

// ── Ship gate: no default deployment without the tally ──────────────────────

/// Recorded operator acceptance. Changing this is the operator's act.
const OPERATOR_ACCEPTED: bool = false;

/// The ship state the calibration currently supports.
const SHIP_STATE: &str = "unshipped";

/// The ship gate: the default installation state and the recorded ship state
/// must agree; shipping requires confirmed acceptance and a quiet control
/// group. A bare flip of either side fails.
#[test]
fn the_ship_state_matches_the_default_installation() {
    let recorded = recorded_tally();
    let strategy = Strategy::with_signal(std::sync::Arc::new(
        FixedSignal::new(Direction::Long, 0.8, 0.7).expect("valid scores"),
    ));
    let installed = strategy.sizing_name();
    let shipped = recorded["ship_state"].as_str().expect("ship state") == "shipped";
    assert_eq!(
        installed == "fixed",
        !shipped,
        "the default strategy installation must match the calibrated ship state"
    );
    if shipped {
        assert!(
            recorded["operator_accepted"].as_bool().unwrap_or(false),
            "shipping needs recorded operator acceptance"
        );
    }
}
