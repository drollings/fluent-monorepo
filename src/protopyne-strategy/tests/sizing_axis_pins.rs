//! Sizing axis pins: every catalog sizer declares its axis.
//!
//! The axis is a declaration, not a default: a sizer that cannot name its
//! axis cannot back a trade. `BuyingPower` is the lone `Correctness`
//! sizer — it enforces the affordability bound; every other catalog sizer
//! expresses edge or allocation (`TaskValue`). The `EquitySizer`'s internal
//! max-notional cap stays the single `Correctness` bound on that path.

use crate::axes::GateAxis;
use crate::slots::sizing::{sizers, sizing_ids};

fn params(id: &str) -> serde_json::Value {
    match id {
        "fixed" => serde_json::json!({"contracts": 2}),
        "percent_net_liq" => serde_json::json!({"fraction": 0.1, "per_unit_max_loss": 100.0}),
        "volatility_scaled" => {
            serde_json::json!({"base": 10, "target_vol": 0.000411, "vol_floor": 0.000205})
        }
        "buying_power" => serde_json::json!({"fraction": 0.5, "per_unit_price": 600.0}),
        "equity" => serde_json::json!({
            "mode": "Risk Percent",
            "risk_pct": 0.5,
            "size_pct": 25.0,
            "fixed_qty": 1.0,
            "max_notional_pct": 100.0,
        }),
        // Research-gated catalog row: present only with the feature, but the
        // params arm is unconditional so the test compiles in both configs.
        "fractional_kelly" => serde_json::json!({
            "base": 10,
            "fraction": 0.5,
            "cap": 0.5,
        }),
        _ => panic!("no canonical params for sizer {id}"),
    }
}

#[test]
fn every_catalog_sizer_states_its_axis() {
    for id in sizing_ids() {
        let params = params(id);
        let sizer = sizers().build(id, &params).expect("catalog sizer builds");
        let expected = match id {
            "buying_power" => GateAxis::Correctness,
            _ => GateAxis::TaskValue,
        };
        assert_eq!(
            sizer.axis(),
            expected,
            "{id}: the axis is declared, never defaulted"
        );
    }
}

#[test]
fn unknown_sizers_still_refuse() {
    assert!(sizers()
        .build("not_a_sizer", &serde_json::json!({}))
        .is_err());
}
