use super::*;
#[cfg(feature = "pine")]
use fluent_wvr::prelude::*;
#[cfg(feature = "pine")]
use protopyne_types::check_for_execute;
use protopyne_types::Validate;

#[cfg(feature = "pine")]
#[test]
fn runtime_config_trait_parity() {
    let good = PineRuntimeConfig::default();
    assert!(good.validate().is_ok());
    assert!(Validate::validate(&good).is_ok());
    let bad = PineRuntimeConfig::new().max_bars(0).warmup_bars(0).build();
    assert!(bad.validate().is_err());
    assert!(Validate::validate(&bad).is_err());
    let inverted = PineRuntimeConfig::new()
        .max_bars(10)
        .warmup_bars(11)
        .build();
    assert!(inverted.validate().is_err());
    assert!(Validate::validate(&inverted).is_err());
}

#[cfg(feature = "pine")]
#[test]
fn invalid_runtime_config_fails_execute_bridge() {
    let bad = PineRuntimeConfig::new().max_bars(0).warmup_bars(0).build();
    assert!(check_for_execute(&bad, "pine runtime invalid").is_err());
    let ctx = WorkContext::default();
    assert!(bad.execute(&ctx).is_err());
    assert!(PineRuntimeConfig::default().execute(&ctx).is_ok());
}

#[test]
fn input_def_trait_parity() {
    let good = InputDef::Float {
        default: 5.0,
        min: 1.0,
        max: 10.0,
    };
    assert!(good.validate().is_ok());
    assert!(Validate::validate(&good).is_ok());
    let inverted = InputDef::Float {
        default: 5.0,
        min: 10.0,
        max: 1.0,
    };
    assert!(inverted.validate().is_err());
    assert!(Validate::validate(&inverted).is_err());
    let out_of_range = InputDef::Int {
        default: 99,
        min: 1,
        max: 10,
    };
    assert!(out_of_range.validate().is_err());
    assert!(Validate::validate(&out_of_range).is_err());
    let non_finite = InputDef::Float {
        default: f64::NAN,
        min: 0.0,
        max: 1.0,
    };
    assert!(non_finite.validate().is_err());
    assert!(Validate::validate(&non_finite).is_err());
}
