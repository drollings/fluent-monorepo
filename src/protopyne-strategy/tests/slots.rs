//! The slot registries: ids, refusals, and build-time resolution.

use std::sync::Arc;

use serde_json::json;

use crate::slots::{SlotError, SlotFactory, SlotRegistry};

struct Sum {
    id: &'static str,
    add: i64,
}

impl SlotFactory<i64> for Sum {
    fn id(&self) -> &'static str {
        self.id
    }

    fn build(&self, params: &serde_json::Value) -> Result<i64, SlotError> {
        let base = params
            .get("base")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| SlotError::invalid(self.id, "base must be an integer"))?;
        Ok(base + self.add)
    }
}

fn registry() -> SlotRegistry<i64> {
    let mut registry = SlotRegistry::new();
    register(&mut registry, "add_one", 1);
    register(&mut registry, "add_two", 2);
    registry
}

fn register(registry: &mut SlotRegistry<i64>, id: &'static str, add: i64) {
    registry
        .register(Arc::new(Sum { id, add }))
        .expect("unique id");
}

#[test]
fn ids_keep_registration_order_and_build_resolves_once() {
    let registry = registry();
    assert_eq!(registry.ids(), vec!["add_one", "add_two"]);
    assert_eq!(registry.build("add_one", &json!({"base": 4})), Ok(5));
    assert_eq!(registry.build("add_two", &json!({"base": 4})), Ok(6));
}

#[test]
fn an_unknown_id_and_bad_params_refuse_with_a_named_reason() {
    let registry = registry();
    assert_eq!(
        registry.build("missing", &json!({})),
        Err(SlotError::UnknownId("missing".to_owned()))
    );
    assert!(matches!(
        registry.build("add_one", &json!({})),
        Err(SlotError::Invalid { id: "add_one", .. })
    ));
}

#[test]
fn a_duplicate_id_refuses_instead_of_shadowing() {
    let mut registry = registry();
    assert!(registry
        .register(Arc::new(Sum {
            id: "add_one",
            add: 9
        }))
        .is_err());
}

// ── Catalog bijection and closure equivalence ─────────────────────────────

use chrono::{TimeZone as _, Utc};
use protopyne_kernels::lite::{lite_legs, LiteParams};
use protopyne_types::Bar;
use rust_decimal::Decimal;

use crate::integration_lite::{FilterSlot, TriggerSlot};
use crate::slots::filter::{filter_ids, filters, Admission, Filter, FilterFn, VolumeMinFilter};
use crate::slots::trigger::{
    trigger_ids, triggers, CrossLevel, Trigger, TriggerFamily, TriggerFn, TriggerVerdict,
};
use crate::tape::Tape;
use fluent_wvr::Describable;
use protopyne_kernels::util::{crossed, Crossing};

fn tape(len: usize) -> (Vec<Bar>, protopyne_kernels::lite::LiteLegs) {
    let bars: Vec<Bar> = (0..len)
        .map(|index| {
            let close = 100.0 + (index as f64 / 5.0).sin() * 4.0 + index as f64 * 0.05;
            let value = Decimal::from_f64_retain(close).expect("finite");
            Bar::new()
                .ts(Utc
                    .with_ymd_and_hms(2026, 1, 5, 15, 0, 0)
                    .single()
                    .expect("time")
                    + chrono::Duration::minutes(index as i64))
                .o(value)
                .h(Decimal::from_f64_retain(close + 0.5).expect("finite"))
                .l(Decimal::from_f64_retain(close - 0.5).expect("finite"))
                .c(value)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect();
    let legs = lite_legs(&bars, &LiteParams::default(), 14).expect("legs");
    (bars, legs)
}

#[test]
fn the_trigger_catalog_is_the_schema_enum_and_every_id_builds() {
    let ids = trigger_ids();
    let schema = TriggerSlot::default().describe();
    let listed: Vec<String> = schema["properties"]["kind"]["enum"]
        .as_array()
        .expect("kind enum")
        .iter()
        .map(|value| value.as_str().expect("string").to_owned())
        .collect();
    assert_eq!(listed, ids, "schema and registry agree");
    let params = [
        ("momentum", json!({})),
        ("ribbon_flip", json!({})),
        ("osc_cross", json!({"fast": "fast", "slow": "slow"})),
        ("exhaustion", json!({"band": 50.0})),
        (
            "dual_exhaustion",
            json!({"slow_level": 50.0, "fast_level": 50.0}),
        ),
    ];
    for (id, params) in params {
        assert!(
            triggers().build(id, &params).is_ok(),
            "{id} must build from its schema parameters"
        );
    }
    assert!(triggers().build("not_a_trigger", &json!({})).is_err());
}

#[test]
fn the_filter_catalog_is_the_schema_enum_and_every_id_builds() {
    let ids = filter_ids();
    let schema = FilterSlot::default().describe();
    let listed: Vec<String> = schema["properties"]["kind"]["enum"]
        .as_array()
        .expect("kind enum")
        .iter()
        .map(|value| value.as_str().expect("string").to_owned())
        .collect();
    assert_eq!(listed, ids, "schema and registry agree");
    for id in &ids {
        let params = match *id {
            "volume_min" => json!({"minimum": 1.0}),
            "atr_distance_max" => json!({"maximum": 3.0}),
            _ => json!({}),
        };
        assert!(
            filters().build(id, &params).is_ok(),
            "{id} must build from its schema parameters"
        );
    }
    assert!(filters().build("not_a_filter", &json!({})).is_err());
}

#[test]
fn a_trigger_closure_matches_the_component_it_replaces() {
    let (bars, legs) = tape(60);
    let tape = Tape::new(&bars, &legs, &legs.close, 40).expect("tape");
    let component = CrossLevel {
        long_level: 0.0,
        short_level: 0.0,
        adaptive: false,
        family: TriggerFamily::Trend,
        reason: "component",
    };
    let closure = TriggerFn::new("closure", |tape| {
        let index = tape.at();
        let fast = &tape.legs().fast;
        let at = |index: usize| fast.get(index).copied().flatten();
        let prev = index.checked_sub(1);
        let long = prev.is_some_and(|prev| crossed(at(prev), at(index), 0.0) == Some(Crossing::Up));
        let short =
            prev.is_some_and(|prev| crossed(at(prev), at(index), 0.0) == Some(Crossing::Down));
        TriggerVerdict::armed(long, short, TriggerFamily::Trend, "closure")
    });
    let component = component.eval(&tape);
    let closure = closure.eval(&tape);
    assert_eq!(
        (component.long, component.short, component.family),
        (closure.long, closure.short, closure.family)
    );
}

#[test]
fn a_filter_closure_matches_the_component_it_replaces() {
    let (bars, legs) = tape(60);
    let tape = Tape::new(&bars, &legs, &legs.close, 40).expect("tape");
    let component = VolumeMinFilter { minimum: 1.0 };
    let closure = FilterFn::new("closure", |tape, _direction| {
        let ratio = tape.legs().vol_ratio.get(tape.at()).copied().unwrap_or(0.0);
        if ratio < 1.0 {
            Admission::Block("relative volume is below the floor")
        } else {
            Admission::Pass
        }
    });
    assert_eq!(
        component.admit(&tape, crate::proposal::Direction::Long),
        closure.admit(&tape, crate::proposal::Direction::Long)
    );
}

#[test]
fn the_sizing_and_composition_catalogs_are_non_empty() {
    assert!(crate::slots::sizing::sizing_ids().contains(&"equity"));
    assert!(crate::slots::composition::strategy_ids().contains(&"rehearsal"));
    assert!(crate::slots::composition::strategy_ids().contains(&"momentum"));
}

// ── The sizer and composition catalogs: the same bijection ──────────────────
//
// Correctness pin, not a calibration: no threshold is measured here. A
// registry row must build from its own parameters and refuse an unknown id
// by name, or a composition can spell a sizer the binder cannot build.

#[test]
fn every_sizer_id_builds_and_unknown_ids_refuse_by_name() {
    use crate::slots::sizing::{sizers, sizing_ids};
    use serde_json::json;

    let ids = sizing_ids();
    assert!(
        ids.contains(&"fixed")
            && ids.contains(&"percent_net_liq")
            && ids.contains(&"volatility_scaled")
            && ids.contains(&"buying_power")
            && ids.contains(&"equity"),
        "the catalog carries every shipped policy: {ids:?}"
    );
    let params = [
        ("fixed", json!({"contracts": 2})),
        (
            "percent_net_liq",
            json!({"fraction": 0.1, "per_unit_max_loss": "500"}),
        ),
        (
            "volatility_scaled",
            json!({"base": 2, "target_vol": 0.2, "vol_floor": 0.05}),
        ),
        (
            "buying_power",
            json!({"fraction": 0.5, "per_unit_price": "250"}),
        ),
        (
            "equity",
            json!({"mode": "Risk Percent", "risk_pct": 1.0, "size_pct": 10.0, "fixed_qty": 1.0, "max_notional_pct": 100.0}),
        ),
    ];
    for (id, params) in params {
        assert!(
            sizers().build(id, &params).is_ok(),
            "{id} must build from its own parameters"
        );
    }
    assert!(matches!(
        sizers().build("not_a_sizer", &json!({})),
        Err(crate::slots::SlotError::UnknownId(name)) if name == "not_a_sizer"
    ));
}

#[test]
fn every_composition_id_builds_and_unknown_ids_refuse_by_name() {
    use crate::slots::composition::{strategies, strategy_ids};
    use serde_json::json;

    let ids = strategy_ids();
    assert_eq!(ids, vec!["rehearsal", "momentum"]);
    assert!(strategies().build("rehearsal", &json!({})).is_ok());
    assert!(strategies()
        .build("rehearsal", &json!({"structure": "single"}))
        .is_ok());
    assert!(strategies().build("momentum", &json!({})).is_ok());
    assert!(matches!(
        strategies().build("not_a_strategy", &json!({})),
        Err(crate::slots::SlotError::UnknownId(name)) if name == "not_a_strategy"
    ));
}
