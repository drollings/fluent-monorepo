use super::*;
use std::path::PathBuf;

fn data_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name)
}

/// Test-local minimal reader for every OHLCV leg. The production CSV loader
/// lives in the server crate; this stays dependency-light so the kernel
/// crate never depends on a downstream layer.
fn spy_ohlcv() -> OwnedOhlcv {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(data_path("SPY_15M.csv"))
        .expect("vendored CSV present");
    let mut data = OwnedOhlcv::default();
    for record in reader.records() {
        let row = record.expect("readable row");
        let num = |i: usize| {
            row.get(i)
                .expect("column present")
                .parse::<f64>()
                .expect("parseable number")
        };
        data.open.push(num(1));
        data.high.push(num(2));
        data.low.push(num(3));
        data.close.push(num(4));
        data.volume.push(num(5));
    }
    data
}

fn load_golden(name: &str) -> Vec<Option<f64>> {
    serde_json::from_str(
        &std::fs::read_to_string(data_path(name)).expect("golden file present"),
    )
    .expect("golden file parses")
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("golden")
        .join(name)
}

/// Frozen full results (primary plus secondaries) for every M5 kernel under
/// canonical parameters. Files were generated once from the reviewed
/// implementation, hand-verified against independent recomputation
/// (leading-`None` counts, range invariants, seed/slide recurrences), then
/// frozen: any numeric change fails here first.
fn golden_params(id: &str) -> ParamSet {
    let len14 = || ParamSet::new().with_int("length", 14);
    match id {
        "alma" => ParamSet::new()
            .with_int("length", 14)
            .with_float("offset", 0.85)
            .with_float("sigma", 6.0),
        "macd" => ParamSet::new()
            .with_int("fast_length", 12)
            .with_int("slow_length", 26)
            .with_int("signal_length", 9),
        "bb" => ParamSet::new()
            .with_int("length", 14)
            .with_float("mult", 2.0),
        "kc" => ParamSet::new()
            .with_int("length", 14)
            .with_float("mult", 2.0)
            .with_bool("use_true_range", true),
        "tr" => ParamSet::new(),
        _ => len14(),
    }
}

#[test]
fn m5_kernels_match_frozen_goldens() {
    let data = spy_ohlcv();
    let views = data.views();
    let registry = builtin_registry().expect("builtins register");
    let files = [
        ("rma", "rma14.json"),
        ("wma", "wma14.json"),
        ("hma", "hma14.json"),
        ("vwma", "vwma14.json"),
        ("alma", "alma14.json"),
        ("rsi", "rsi14.json"),
        ("macd", "macd12269.json"),
        ("stoch", "stoch14.json"),
        ("tr", "tr.json"),
        ("atr", "atr14.json"),
        ("bb", "bb14.json"),
        ("kc", "kc14.json"),
    ];
    for (id, file) in files {
        let kernel = registry.get(id).expect("kernel registered");
        let result = kernel
            .batch(&views, &golden_params(id))
            .expect("valid params");
        let frozen: IndicatorResult =
            serde_json::from_str(&std::fs::read_to_string(golden_path(file)).expect("golden"))
                .expect("golden parses");
        assert_golden_close(&result, &frozen, id);
    }
}

/// Golden comparison with a 1e-12 tolerance. Bit-exactness is proven by the
/// in-process batch/update fold tests; the JSON text round-trip itself is
/// not bit-exact on every value in this toolchain (the float display prints
/// fewer digits than a full round-trip needs on ~5% of values), so file
/// goldens guard against semantic drift while the folds guard the bits.
fn assert_golden_close(result: &IndicatorResult, frozen: &IndicatorResult, id: &str) {
    const TOL: f64 = 1e-12;
    assert_eq!(result.spec_id, frozen.spec_id, "id drift in {id}");
    assert_series_close(&result.values, &frozen.values, TOL, id, "primary");
    assert_eq!(
        result.secondary.len(),
        frozen.secondary.len(),
        "secondary count drift in {id}"
    );
    for (got, want) in result.secondary.iter().zip(frozen.secondary.iter()) {
        assert_eq!(got.name, want.name, "secondary name drift in {id}");
        assert_series_close(&got.values, &want.values, TOL, id, &got.name);
    }
}

fn assert_series_close(
    got: &[Option<f64>],
    want: &[Option<f64>],
    tol: f64,
    id: &str,
    leg: &str,
) {
    assert_eq!(got.len(), want.len(), "length drift in {id}:{leg}");
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        match (g, w) {
            (None, None) => {}
            (Some(g), Some(w)) => assert!(
                (g - w).abs() <= tol,
                "golden drift in {id}:{leg}[{i}]: {g} vs {w}"
            ),
            _ => panic!("golden na drift in {id}:{leg}[{i}]: {g:?} vs {w:?}"),
        }
    }
}

#[test]
fn spy_close_column_shape() {
    let data = spy_ohlcv();
    assert_eq!(data.len(), 384);
    assert!((data.close[0] - 686.56).abs() < 1e-9);
}

#[test]
fn sma14_matches_frozen_golden() {
    let sma = SmaIndicator::new();
    let data = spy_ohlcv();
    let result = sma
        .batch(
            &data.views(),
            &ParamSet::new().with_int("length", 14),
        )
        .expect("valid params");
    assert_eq!(result.values, load_golden("sma14.json"));
}

#[test]
fn ema14_matches_frozen_golden() {
    let ema = EmaIndicator::new();
    let data = spy_ohlcv();
    let result = ema
        .batch(
            &data.views(),
            &ParamSet::new().with_int("length", 14),
        )
        .expect("valid params");
    assert_eq!(result.values, load_golden("ema14.json"));
}
