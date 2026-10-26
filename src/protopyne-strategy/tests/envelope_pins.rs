//! Envelope pins: the uniform handle agrees with the slot.
//!
//! For every catalog id, three paths must produce the same verdict: the
//! directly built slot, the envelope's `eval`, and the envelope's `execute`
//! through a `WorkContext` tape snapshot (compared as typed JSON data).
//! The axis table and the bind-time refusals are pinned alongside, so a
//! later composition change cannot silently re-axis or fail open.

use super::*;
use crate::proposal::Direction;
use crate::tape::sine_fixture;

fn trigger_params(id: &str) -> serde_json::Value {
    match id {
        "momentum" | "ribbon_flip" => serde_json::json!({}),
        "osc_cross" => serde_json::json!({"fast": "fast", "slow": "slow"}),
        "exhaustion" => serde_json::json!({"band": 50.0}),
        "dual_exhaustion" => serde_json::json!({"slow_level": 50.0, "fast_level": 50.0}),
        _ => panic!("no canonical params for trigger {id}"),
    }
}

fn filter_params(id: &str) -> serde_json::Value {
    match id {
        "volume_min" => serde_json::json!({"minimum": 1.0}),
        "atr_distance_max" => serde_json::json!({"maximum": 3.0}),
        _ => serde_json::json!({}),
    }
}

#[test]
fn every_trigger_envelope_agrees_with_its_slot_on_all_paths() {
    for id in crate::slots::trigger::trigger_ids() {
        let params = trigger_params(id);
        let slot = crate::slots::trigger::triggers()
            .build(id, &params)
            .expect("catalog id builds");
        let envelope = TriggerUnit::resolve(id, &params).expect("catalog id resolves");
        let (bars, legs) = sine_fixture(60);
        let bars = std::sync::Arc::new(bars);
        let legs = std::sync::Arc::new(legs);
        let tape = Tape::new(&bars, &legs, &legs.close, 40).expect("tape");
        let direct = slot.eval(&tape);
        assert_eq!(envelope.eval(&tape), direct, "{id}: envelope eval agrees");
        let mut ctx = WorkContext::default();
        ctx.set(
            TAPE_KEY,
            TapeSnapshot::new(
                std::sync::Arc::clone(&bars),
                std::sync::Arc::clone(&legs),
                40,
            ),
        );
        let output = envelope.execute(&ctx).expect("{id} executes");
        assert!(
            output.success,
            "{id}: envelope execution succeeds with a tape present"
        );
        assert_eq!(
            output.data,
            serde_json::to_value(direct).expect("verdict serializes"),
            "{id}: execute data equals the direct verdict"
        );
        assert_eq!(envelope.name(), id);
        assert!(envelope.depends().is_empty() && envelope.provides().is_empty());
        assert_eq!(envelope.axis(), GateAxis::TaskValue);
    }
}

#[test]
fn every_filter_envelope_agrees_with_its_slot_for_both_sides() {
    for id in crate::slots::filter::filter_ids() {
        let params = filter_params(id);
        for direction in [Direction::Long, Direction::Short] {
            let slot = crate::slots::filter::filters()
                .build(id, &params)
                .expect("catalog id builds");
            let envelope = FilterUnit::resolve(id, &params, direction).expect("resolves");
            let (bars, legs) = sine_fixture(60);
            let bars = std::sync::Arc::new(bars);
            let legs = std::sync::Arc::new(legs);
            let tape = Tape::new(&bars, &legs, &legs.close, 40).expect("tape");
            let direct = slot.admit(&tape, direction);
            assert_eq!(envelope.eval(&tape), direct, "{id}: envelope eval agrees");
            let mut ctx = WorkContext::default();
            ctx.set(
                TAPE_KEY,
                TapeSnapshot::new(
                    std::sync::Arc::clone(&bars),
                    std::sync::Arc::clone(&legs),
                    40,
                ),
            );
            let output = envelope.execute(&ctx).expect("{id} executes");
            assert_eq!(
                output.data,
                serde_json::to_value(direct).expect("admission serializes"),
                "{id}: execute data equals the direct admission"
            );
            assert_eq!(envelope.name(), id);
            assert_eq!(envelope.axis(), GateAxis::TaskValue);
        }
    }
}

#[test]
fn envelopes_refuse_unknown_ids_and_missing_tapes() {
    assert!(TriggerUnit::resolve("not_a_trigger", &serde_json::json!({})).is_err());
    assert!(FilterUnit::resolve("not_a_filter", &serde_json::json!({}), Direction::Long).is_err());
    let trigger = TriggerUnit::resolve("momentum", &serde_json::json!({})).expect("resolves");
    let bare = WorkContext::default();
    assert!(
        trigger.execute(&bare).is_err(),
        "no tape snapshot under the key must refuse, not invent a verdict"
    );
}

#[test]
fn research_slots_wrap_directly_with_a_stated_axis() {
    use crate::slots::trigger::{TriggerFamily, TriggerFn, TriggerVerdict};
    let closure = TriggerFn::new("research", |_| {
        TriggerVerdict::idle(TriggerFamily::Trend, "research")
    });
    let unit = TriggerUnit::new(Arc::new(closure), GateAxis::ProducerConfidence);
    assert_eq!(unit.name(), "research");
    assert_eq!(unit.axis(), GateAxis::ProducerConfidence);
    let (bars, legs) = sine_fixture(60);
    let bars = std::sync::Arc::new(bars);
    let legs = std::sync::Arc::new(legs);
    let tape = Tape::new(&bars, &legs, &legs.close, 40).expect("tape");
    assert_eq!(
        unit.eval(&tape),
        TriggerVerdict::idle(TriggerFamily::Trend, "research")
    );
    let mut ctx = WorkContext::default();
    ctx.set(
        TAPE_KEY,
        TapeSnapshot::new(
            std::sync::Arc::clone(&bars),
            std::sync::Arc::clone(&legs),
            40,
        ),
    );
    assert!(unit.execute(&ctx).expect("executes").success);
}
