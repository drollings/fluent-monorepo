use super::*;
use crate::indicators::{default_style_for_kernel, ChartIndicator};
use crate::oscillators::{
    evaluable_oscillator_ids, oscillator_options, parse_oscillator_kind, OscillatorKind,
};
use protopyne_kernels::indicator::indicator_schema;
use protopyne_kernels::price_source::PriceSource;
use protopyne_kernels::smoothing::{
    length_smoothing_ids, smoothing_options, SmoothingFamily, MA_PERIOD_MAX, MA_PERIOD_MIN,
};

#[test]
fn panel_kinds_resolve_and_round_trip() {
    for kind in PANEL_KINDS {
        OscillatorPanelState::default()
            .entry(kind)
            .unwrap_or_else(|_| panic!("panel kind {kind:?} resolves"));
        assert!(
            KindDefaults::for_kind(kind).validate().is_ok(),
            "panel kind {kind:?} defaults validate"
        );
    }
    assert_eq!(
        PANEL_KINDS.len(),
        7,
        "panel kind count is a deliberate change, not drift"
    );
}

#[test]
fn oscillator_kinds_map_to_at_most_one_panel_kind() {
    for kind in OscillatorKind::all() {
        if let Some(kernel) = kind.kernel_id() {
            let mapped = panel_kind_for_kernel(kernel);
            if let Some(panel) = mapped {
                assert!(
                    PANEL_KINDS.contains(&panel),
                    "kernel {kernel:?} maps to a listed panel kind, got {panel:?}"
                );
            }
        }
    }
    assert_eq!(
        default_style_for_kernel("rsi"),
        "rsi",
        "style seeds delegate to the panel map"
    );
    assert_eq!(
        default_style_for_kernel("unknown_kernel_xyz"),
        "fast",
        "unmapped kernels fall back without a second list"
    );
}

#[test]
fn parse_round_trips_id_and_pine_spelling() {
    for kind in OscillatorKind::all() {
        assert_eq!(
            parse_oscillator_kind(kind.id()),
            Some(kind),
            "id {:?} parses",
            kind.id()
        );
        assert_eq!(
            parse_oscillator_kind(kind.pine_name()),
            Some(kind),
            "pine spelling {:?} parses",
            kind.pine_name()
        );
    }
    assert_eq!(
        parse_oscillator_kind("cdv"),
        Some(OscillatorKind::Cdv),
        "cdv parses"
    );
    assert!(
        crate::oscillators::normalize_oscillator(10.0, OscillatorKind::Cdv).is_err(),
        "cdv parses but refuses evaluation with its reason, never a default constant"
    );
}

#[test]
fn lite_leg_kinds_are_evaluable_panel_kinds() {
    assert_eq!(
        LITE_LEG_KINDS.len(),
        5,
        "lite leg count is a deliberate change, not drift"
    );
    let evaluable = evaluable_oscillator_ids();
    for kind in LITE_LEG_KINDS {
        assert!(
            evaluable.iter().any(|id| id == kind),
            "lite leg {kind:?} is evaluable"
        );
        assert!(
            parse_oscillator_kind(kind).is_some(),
            "lite leg {kind:?} parses through the catalog"
        );
    }
    assert_eq!(
        crate::oscillator_panel::lite_leg_kind_ids().len(),
        LITE_LEG_KINDS.len(),
        "leg picker lists the catalog without re-spelling it"
    );
}

#[test]
fn smoothing_single_truth_eit_refuses_length_only() {
    assert!(
        SmoothingFamily::Eit.params_for_length(14).is_err(),
        "eit refuses length-only construction and asks for alpha"
    );
    for family in SmoothingFamily::all() {
        if family.takes_length() {
            assert!(
                family.params_for_length(14).is_ok(),
                "{:?} length-only builds",
                family.id()
            );
            assert!(
                protopyne_kernels::smoothing::params_for_kernel(family.id(), 14).is_ok(),
                "{:?} routes through params_for_kernel",
                family.id()
            );
        }
    }
    for id in length_smoothing_ids() {
        assert!(
            SmoothingFamily::parse(&id).is_some_and(SmoothingFamily::takes_length),
            "length picker id {id:?} parses to a length-taking family"
        );
    }
    for option in smoothing_options() {
        assert!(
            SmoothingFamily::parse(&option.id).is_some(),
            "smoothing picker id {:?} parses",
            option.id
        );
    }
}

#[test]
fn ma_period_contract_matches_kernel_schema() {
    assert_eq!((MA_PERIOD_MIN, MA_PERIOD_MAX), (2, 200));
    for kernel in ["sma", "ema", "rma", "wma", "hma", "vwma", "alma"] {
        let schema = indicator_schema(kernel).unwrap_or_else(|| panic!("{kernel} has a schema"));
        let length = schema
            .params
            .iter()
            .find(|p| p.name == "length")
            .unwrap_or_else(|| panic!("{kernel} declares length"));
        assert_eq!(
            length.min,
            Some(MA_PERIOD_MIN as f64),
            "{kernel} schema min reads the contract"
        );
        assert_eq!(
            length.max,
            Some(MA_PERIOD_MAX as f64),
            "{kernel} schema max reads the contract"
        );
    }
}

#[test]
fn pickers_read_the_catalogs_not_hand_lists() {
    for option in oscillator_options() {
        assert!(
            parse_oscillator_kind(&option.id).is_some(),
            "oscillator picker id {:?} parses",
            option.id
        );
    }
    for id in crate::topology::price_source_ids() {
        assert!(
            PriceSource::parse(&id).is_some(),
            "price source picker id {id:?} parses"
        );
    }
}

#[test]
fn config_fingerprint_is_stable_and_sensitive() {
    let a = crate::integration_lite::IntegrationLiteConfig::default();
    let b = crate::integration_lite::IntegrationLiteConfig::new().build();
    let fa = crate::indicators::IntegrationLiteIndicator::new()
        .config_key(&a)
        .expect("default configs fingerprint");
    let fb = crate::indicators::IntegrationLiteIndicator::new()
        .config_key(&b)
        .expect("default configs fingerprint");
    assert_eq!(fa, fb, "unchanged defaults keep the cache key");
    let mut moved = a.clone();
    moved.pivot_threshold = 200.0;
    let fm = crate::indicators::IntegrationLiteIndicator::new()
        .config_key(&moved)
        .expect("edited config fingerprints");
    assert_ne!(fa, fm, "an edited dial moves the cache key");
}
