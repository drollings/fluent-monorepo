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

/// The Technical tab lists every registry id under its display name, so two
/// kernels sharing one display name render as a duplicated indicator.
/// Bollinger Bands ships once, as the native kernel: the TA-Lib-shaped
/// `bbands` duplicate (asymmetric `nbdevup`/`nbdevdn`) is gone.
#[test]
fn bollinger_bands_is_listed_once() {
    let registry = builtin_registry().expect("registry builds");
    assert!(
        registry.get("bb").is_some(),
        "the native kernel stays registered"
    );
    assert!(
        registry.get("bbands").is_none(),
        "the asymmetric duplicate is gone"
    );
    let mut names: Vec<&str> = registry
        .ids()
        .iter()
        .filter_map(|id| registry.get(id))
        .map(|indicator| indicator.spec().display_name.as_str())
        .collect();
    names.sort_unstable();
    let len_before = names.len();
    names.dedup();
    assert_eq!(
        names.len(),
        len_before,
        "no two kernels share a display name"
    );
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
    assert_eq!(checked, 57, "all 57 builtin kernels are covered");
}

/// The names/ordering check above compares *values*; this one pins the source.
/// Every kernel's declared params/outputs must be the central table's own
/// slice — a per-spec copy (the shadowing channel this milestone removed) would
/// pass an equality check and fail a pointer check.
#[test]
fn every_spec_reads_the_schema_table_itself_never_a_copy() {
    let registry = builtin_registry().expect("registry builds");
    for id in registry.ids() {
        let indicator = registry.get(&id).expect("registered");
        let spec = indicator.spec();
        let schema = indicator_schema(&id).unwrap_or_else(|| panic!("{id}: no schema"));
        assert!(
            std::ptr::eq(spec.params(), schema.params),
            "{id}: params must be the schema table's slice, not a per-spec copy"
        );
        assert!(
            std::ptr::eq(spec.outputs(), schema.outputs),
            "{id}: outputs must be the schema table's slice, not a per-spec copy"
        );
    }
}

/// The identity fold's case list is derived from this same schema surface, so
/// the fold can only be missing a kernel if this assertion is missing one:
/// every registry id has exactly one case, and every declared secondary output
/// appears in that case exactly once. Extending the schema suite is what keeps
/// "all kernels" true instead of aspirational.
#[test]
fn the_identity_fold_covers_every_registry_id_and_declared_output() {
    let registry = builtin_registry().expect("registry builds");
    let cases = super::kernel_identity_tests::all_cases();
    let mut case_ids: Vec<&str> = cases.iter().map(|case| case.id.as_str()).collect();
    case_ids.sort_unstable();
    let mut unique = case_ids.clone();
    unique.dedup();
    assert_eq!(
        case_ids, unique,
        "each registry id has exactly one fold case"
    );
    let ids = registry.ids();
    let mut registry_ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    registry_ids.sort_unstable();
    assert_eq!(
        case_ids, registry_ids,
        "the fold covers the registry exactly, no orphans either way"
    );
    for case in &cases {
        let indicator = registry.get(&case.id).expect("registered");
        let spec = indicator.spec();
        let declared: Vec<&str> = spec
            .outputs()
            .iter()
            .filter(|output| output.role == OutputRole::Secondary)
            .map(|output| output.name)
            .collect();
        assert_eq!(
            case.secondary, declared,
            "{}: the case folds exactly the declared secondaries",
            case.id
        );
        let mut seen = std::collections::HashSet::new();
        for name in &case.secondary {
            assert!(
                seen.insert(*name),
                "{}: declared output {name} appears once",
                case.id
            );
        }
    }
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
            "bb",
            vec!["length", "mult"],
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

/// The moving-average tree shares one period contract: every MA kernel's
/// `length` spans 2..200 (the settings sliders' window), while non-MA
/// kernels keep the wide lookback range. One shared definition feeds both
/// the schema and the validation, so the slider and the refusal agree.
#[test]
fn ma_family_shares_one_period_contract() {
    for id in [
        "sma", "ema", "rma", "wma", "hma", "vwma", "dema", "tema", "trima", "alma", "kalman",
    ] {
        let schema = indicator_schema(id).expect("schema");
        let length = schema
            .params
            .iter()
            .find(|param| param.name == "length")
            .unwrap_or_else(|| panic!("{id}: declares a period length"));
        assert_eq!(
            (length.min, length.max),
            (Some(2.0), Some(200.0)),
            "{id}: the MA period spans 2..200"
        );
    }
    for id in ["rsi", "cci", "mom", "mfi", "stoch"] {
        let schema = indicator_schema(id).expect("schema");
        let length = schema
            .params
            .iter()
            .find(|param| param.name == "length")
            .unwrap_or_else(|| panic!("{id}: declares a lookback length"));
        assert_eq!(
            (length.min, length.max),
            (Some(1.0), Some(100_000.0)),
            "{id}: non-MA lookbacks keep the wide range"
        );
    }
}
