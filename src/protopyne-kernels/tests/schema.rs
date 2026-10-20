//! Every builtin kernel declares a usable parameter/output schema.

use crate::indicator::{builtin_registry, indicator_schema, Ohlcv, OutputRole, ParamValue};
use crate::OwnedOhlcv;

fn fixture() -> OwnedOhlcv {
    let n = 160usize;
    let price: Vec<f64> = (0..n)
        .map(|i| 100.0 + (i as f64 * 0.17).sin() * 5.0 + i as f64 * 0.03)
        .collect();
    OwnedOhlcv {
        open: price.iter().map(|p| p - 0.2).collect(),
        high: price.iter().map(|p| p + 1.0).collect(),
        low: price.iter().map(|p| p - 1.0).collect(),
        close: price,
        volume: (0..n).map(|i| 1_000.0 + i as f64).collect(),
    }
}

#[test]
fn every_registered_kernel_has_a_schema_and_evaluates_with_defaults() {
    let registry = builtin_registry().expect("registry builds");
    let frame = fixture();
    let mut checked = 0;
    for id in registry.ids() {
        let indicator = registry.get(&id).expect("registered");
        let spec = indicator.spec();
        let schema = indicator_schema(&id).unwrap_or_else(|| panic!("{id}: no schema"));
        assert_eq!(spec.params(), schema.params, "{id}: params table");
        assert_eq!(spec.outputs(), schema.outputs, "{id}: outputs table");
        assert!(!spec.outputs().is_empty(), "{id}: at least one output");
        assert_eq!(
            spec.outputs()[0].role,
            OutputRole::Primary,
            "{id}: primary comes first"
        );
        let params = spec.default_params();
        for param in spec.params() {
            assert!(
                params.get(param.name).is_some(),
                "{id}: default parameter {:?} is absent",
                param.name
            );
        }
        let result = indicator
            .batch(&frame.views(), &params)
            .unwrap_or_else(|error| panic!("{id}: defaults refused: {error}"));
        assert_eq!(result.values.len(), frame.len(), "{id}: primary length");
        for output in spec.outputs().iter().skip(1) {
            let series = result
                .secondary(output.name)
                .unwrap_or_else(|| panic!("{id}: declared output {:?} missing", output.name));
            assert_eq!(series.len(), frame.len(), "{id}: {:?} length", output.name);
        }
    }
    checked += registry.len();
    assert_eq!(checked, 56, "all 56 builtin kernels are covered");
}

#[test]
fn representative_schemas_snapshot_their_parameters_and_outputs() {
    let cases = [
        (
            "macd",
            vec!["fast_length", "slow_length", "signal_length"],
            vec!["value", "signal", "hist"],
        ),
        (
            "bbands",
            vec!["length", "nbdevup", "nbdevdn", "matype"],
            vec!["value", "upper", "lower"],
        ),
        ("stoch", vec!["length"], vec!["value", "d"]),
        (
            "ichimoku",
            vec!["tenkan_length", "kijun_length", "senkou_b_length"],
            vec!["value", "kijun", "senkou_a", "senkou_b", "chikou"],
        ),
        ("sma", vec!["length"], vec!["value"]),
    ];
    for (id, params, outputs) in cases {
        let schema = indicator_schema(id).expect("schema");
        let declared: Vec<&str> = schema.params.iter().map(|param| param.name).collect();
        assert_eq!(declared, params, "{id}: parameter names");
        let declared_outputs: Vec<&str> = schema.outputs.iter().map(|output| output.name).collect();
        assert_eq!(declared_outputs, outputs, "{id}: output names");
    }
}

#[test]
fn declared_defaults_typecheck() {
    let schema = indicator_schema("macd").expect("schema");
    for param in schema.params {
        match param.default {
            ParamValue::Int(value) => assert!(value > 0),
            ParamValue::Float(value) => assert!(value.is_finite()),
            ParamValue::Bool(_) => {}
        }
    }
    let _ = Ohlcv::close_only(&[]);
}
