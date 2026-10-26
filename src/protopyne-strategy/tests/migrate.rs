//! The nested view and its compatibility round trip.

use crate::integration_lite::spec::IntegrationLiteStrategyConfig;
use crate::integration_lite::{entry_from_config, IntegrationLiteSpec, TriggerSlot};

#[test]
fn the_flat_config_round_trips_through_the_nested_view() {
    let flat = IntegrationLiteStrategyConfig {
        sig_window: 17,
        sig_exhaust_entry: true,
        filt_volume: true,
        filt_volume_min: 1.4,
        ..IntegrationLiteStrategyConfig::default()
    };
    let spec = IntegrationLiteSpec::from(&flat);
    assert_eq!(spec.entry.setup_window, 17);
    assert!(spec
        .entry
        .triggers
        .iter()
        .any(|slot| slot.kind == "exhaustion"));
    assert!(spec
        .entry
        .filters
        .iter()
        .any(|slot| slot.kind == "volume_min" && slot.enable));
    let restored = IntegrationLiteStrategyConfig::try_from(&spec).expect("the view restores");
    assert_eq!(
        restored, flat,
        "the compatibility window returns exactly the bytes that entered"
    );
}

#[test]
fn a_nested_trigger_slot_is_a_schema_round_trip() {
    use fluent_wvr::{Describable, FieldAccess};

    let mut slot = TriggerSlot::default();
    slot.set_field("kind", "momentum").expect("kind");
    slot.set_field("level", "12.5").expect("level");
    assert_eq!(slot.get_field("level").expect("level"), "12.5");
    let schema = slot.describe();
    assert_eq!(schema["properties"]["kind"]["type"], "string");

    let spec = IntegrationLiteSpec::from(&IntegrationLiteStrategyConfig::default());
    let json = serde_json::to_string(&spec).expect("serializes");
    let decoded: IntegrationLiteSpec = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(
        serde_json::to_string(&decoded).expect("re-serializes"),
        json,
        "spec serialization is byte-stable"
    );
}

#[test]
fn flat_and_nested_entry_builds_agree_at_every_cursor() {
    use crate::signals_lite::{build_triggers, build_triggers_from_entry};
    use crate::tape::sine_fixture;

    let flat = IntegrationLiteStrategyConfig {
        sig_momentum: true,
        sig_ribbon_flip: true,
        sig_osc_cross: true,
        sig_exhaust_entry: true,
        sig_dual: true,
        filt_ribbon: true,
        filt_ma_health: true,
        filt_ribbon_health: true,
        filt_slow: true,
        filt_delta: true,
        filt_volume: true,
        filt_extension: true,
        filt_regime_lowvol: true,
        filt_regime_volatile: true,
        ..IntegrationLiteStrategyConfig::default()
    };
    let from_flat = build_triggers(&flat);
    let entry = entry_from_config(&flat);
    let from_entry = build_triggers_from_entry(&entry);
    assert_eq!(
        from_flat.len(),
        from_entry.len(),
        "the projection keeps every enabled row"
    );
    let (bars, legs) = sine_fixture(60);
    for at in 0..60 {
        let tape = crate::tape::Tape::new(&bars, &legs, &legs.close, at).expect("tape");
        for (flat_built, entry_built) in from_flat.iter().zip(from_entry.iter()) {
            assert_eq!(
                entry_built.eval(&tape),
                flat_built.eval(&tape),
                "bar {at}: the entry path builds what the flat path built"
            );
        }
    }
}

#[test]
fn a_hand_edited_entry_row_is_honored_by_the_builders() {
    use crate::integration_lite::entry_from_config;
    use crate::signals_lite::{build_filters_from_entry, build_triggers_from_entry};
    use crate::tape::sine_fixture;

    let flat = IntegrationLiteStrategyConfig::default();
    let mut entry = entry_from_config(&flat);
    // Disable every trigger except a hand-raised momentum arm, and drop
    // every filter: the verdict must follow the edited rows, not the flat
    // dials they were projected from.
    entry.triggers.retain(|slot| slot.kind == "momentum");
    assert!(
        !entry.triggers.is_empty(),
        "the default config arms momentum"
    );
    for slot in &mut entry.triggers {
        slot.level = 1.0;
        slot.adaptive = false;
    }
    entry.filters.clear();
    let triggers = build_triggers_from_entry(&entry);
    assert_eq!(triggers.len(), entry.triggers.len());
    let filters = build_filters_from_entry(&entry);
    assert!(filters.is_empty());
    let (bars, legs) = sine_fixture(60);
    let mut armed_long = false;
    let mut armed_short = false;
    for at in 0..60 {
        let tape = crate::tape::Tape::new(&bars, &legs, &legs.close, at).expect("tape");
        for trigger in &triggers {
            let verdict = trigger.eval(&tape);
            armed_long = armed_long || verdict.long;
            armed_short = armed_short || verdict.short;
        }
    }
    assert!(
        armed_long || armed_short,
        "a 1.0 crossing level on an oscillating leg must arm somewhere in 60 bars"
    );
}
