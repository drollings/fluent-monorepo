//! Slot-meaning identity: one id, one verdict, every provenance.
//!
//! The bespoke `build_triggers`/`build_filters` assembly and the slot
//! registries must construct the same slots: same structs, same verdicts,
//! both sides, every cursor. This is the contract the signal-assembly
//! migration (bespoke builders onto registries) must preserve.

use crate::integration_lite::IntegrationLiteStrategyConfig;
use crate::proposal::Direction;
use crate::signals_lite::{build_filters, build_triggers};
use crate::slots::filter::filters;
use crate::slots::trigger::triggers;
use crate::tape::sine_fixture;

fn all_enabled() -> IntegrationLiteStrategyConfig {
    IntegrationLiteStrategyConfig {
        sig_momentum: true,
        sig_ribbon_flip: true,
        sig_osc_cross: true,
        sig_exhaust_entry: true,
        sig_dual: true,
        sig_dual_lookback: 2,
        filt_ribbon: true,
        filt_ma_health: true,
        filt_ma_health_min: 10.0,
        filt_ribbon_health: true,
        filt_slow: true,
        filt_slow_min: 0.0,
        filt_delta: true,
        filt_delta_len: 3,
        filt_volume: true,
        filt_volume_min: 0.8,
        filt_extension: true,
        filt_extension_max: 2.5,
        filt_regime_lowvol: true,
        filt_regime_volatile: true,
        ..IntegrationLiteStrategyConfig::default()
    }
}

fn trigger_params(strategy: &IntegrationLiteStrategyConfig, id: &str) -> serde_json::Value {
    match id {
        "momentum" => serde_json::json!({
            "long_level": strategy.sig_entry_level,
            "short_level": -strategy.sig_entry_level,
            "adaptive": strategy.sig_adaptive_level,
        }),
        "ribbon_flip" => serde_json::json!({}),
        "osc_cross" => serde_json::json!({"fast": "fast", "slow": "slow"}),
        "exhaustion" => serde_json::json!({"band": strategy.sig_exhaust_level}),
        "dual_exhaustion" => serde_json::json!({
            "slow_level": strategy.sig_dual_slow,
            "fast_level": strategy.sig_dual_fast,
            "lookback": strategy.sig_dual_lookback.max(0),
        }),
        _ => panic!("no bespoke mapping for trigger {id}"),
    }
}

fn filter_params(strategy: &IntegrationLiteStrategyConfig, id: &str) -> serde_json::Value {
    match id {
        "ribbon" | "ribbon_health" => serde_json::json!({}),
        "ma_health" => serde_json::json!({"minimum": strategy.filt_ma_health_min}),
        "slow_side" => serde_json::json!({"minimum": strategy.filt_slow_min}),
        "delta_side" => serde_json::json!({"length": strategy.filt_delta_len.max(1)}),
        "volume_min" => serde_json::json!({"minimum": strategy.filt_volume_min}),
        "atr_distance_max" => serde_json::json!({"maximum": strategy.filt_extension_max}),
        "regime_skip" => serde_json::json!({
            "low_volume": strategy.filt_regime_lowvol,
            "volatile": strategy.filt_regime_volatile,
        }),
        _ => panic!("no bespoke mapping for filter {id}"),
    }
}

#[test]
fn bespoke_and_registry_triggers_agree_at_every_cursor() {
    let strategy = all_enabled();
    let bespoke = build_triggers(&strategy);
    let ids = [
        "momentum",
        "ribbon_flip",
        "osc_cross",
        "exhaustion",
        "dual_exhaustion",
    ];
    assert_eq!(
        bespoke.len(),
        ids.len(),
        "all five branches enabled means five triggers"
    );
    let via_registry: Vec<_> = ids
        .iter()
        .map(|id| {
            triggers()
                .build(id, &trigger_params(&strategy, id))
                .expect("catalog builds the bespoke mapping")
        })
        .collect();
    let (bars, legs) = sine_fixture(60);
    for at in 0..60 {
        let tape = crate::tape::Tape::new(&bars, &legs, &legs.close, at).expect("tape");
        for ((slot, catalog), id) in bespoke.iter().zip(via_registry.iter()).zip(ids) {
            assert_eq!(
                catalog.eval(&tape),
                slot.eval(&tape),
                "{id} at bar {at}: registry and bespoke verdicts agree"
            );
        }
    }
}

#[test]
fn bespoke_and_registry_filters_agree_for_both_sides_at_every_cursor() {
    let strategy = all_enabled();
    let bespoke = build_filters(&strategy);
    let ids = [
        "ribbon",
        "ma_health",
        "ribbon_health",
        "slow_side",
        "delta_side",
        "volume_min",
        "atr_distance_max",
        "regime_skip",
    ];
    assert_eq!(
        bespoke.len(),
        ids.len(),
        "all eight guards enabled means eight filters"
    );
    let via_registry: Vec<_> = ids
        .iter()
        .map(|id| {
            filters()
                .build(id, &filter_params(&strategy, id))
                .expect("catalog builds the bespoke mapping")
        })
        .collect();
    let (bars, legs) = sine_fixture(60);
    for at in 0..60 {
        let tape = crate::tape::Tape::new(&bars, &legs, &legs.close, at).expect("tape");
        for ((slot, catalog), id) in bespoke.iter().zip(via_registry.iter()).zip(ids) {
            for direction in [Direction::Long, Direction::Short] {
                assert_eq!(
                    catalog.admit(&tape, direction),
                    slot.admit(&tape, direction),
                    "{id} {direction:?} at bar {at}: registry and bespoke agree"
                );
            }
        }
    }
}
