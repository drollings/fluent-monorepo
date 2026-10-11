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

/// Baseline vectors for the indicator under test: the exact kernel/length
/// pairs it evaluates over the 5-minute history (three `ta.sma` plots plus
/// the six factory smoothings it needs at default inputs).
/// Proof command: `UPDATE_FASTMAS_BASELINE=1 cargo test -p protopyne-kernels
/// spy5m_fastmas_baseline` writes the goldens; the frozen run compares.
fn fastmas_baseline_configs() -> Vec<(&'static str, ParamSet)> {
    let len = |n: i64| ParamSet::new().with_int("length", n);
    vec![
        ("sma50", len(50)),
        ("sma100", len(100)),
        ("sma200", len(200)),
        ("ema8", len(8)),
        ("ema21", len(21)),
        ("ema34", len(34)),
        ("ema55", len(55)),
        ("wma3", len(3)),
        (
            "alma8",
            ParamSet::new()
                .with_int("length", 8)
                .with_float("offset", 0.85)
                .with_float("sigma", 6.0),
        ),
    ]
}

fn fastmas_baseline_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("golden")
        .join(format!("fastmas_baseline_{name}.csv"))
}

/// Test-local reader for the 5-minute history. Positional like
/// [`spy_ohlcv`]. The vendored copy drops the upstream file's empty trailing
/// `,Plot` header (data rows byte-identical) so the strict column reader
/// stays strict; see the server `spy_5m_ingest_golden` comment.
fn spy5m_ohlcv() -> OwnedOhlcv {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(data_path("SPY_5M.csv"))
        .expect("vendored SPY_5M present");
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

fn write_baseline_csv(values: &[Option<f64>]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("value\n");
    for v in values {
        match v {
            Some(x) => {
                let _ = writeln!(out, "{x:?}");
            }
            None => out.push('\n'),
        }
    }
    out
}

fn read_baseline_csv(content: &str) -> Vec<Option<f64>> {
    let mut lines = content.lines();
    assert_eq!(lines.next(), Some("value"), "baseline header drift");
    lines
        .map(|line| {
            let cell = line.trim();
            if cell.is_empty() {
                None
            } else {
                Some(cell.parse::<f64>().expect("parseable golden value"))
            }
        })
        .collect()
}

#[test]
fn spy5m_fastmas_baseline() {
    use protopyne_types::warmup_headroom;

    let data = spy5m_ohlcv();
    assert_eq!(data.len(), 1149, "SPY_5M row-count golden");
    let views = data.views();
    let registry = builtin_registry().expect("builtins register");
    let regen = std::env::var("UPDATE_FASTMAS_BASELINE").is_ok();

    for (name, params) in fastmas_baseline_configs() {
        let kernel_id = name.trim_end_matches(|c: char| c.is_ascii_digit());
        let kernel = registry.get(kernel_id).expect("kernel registered");
        let result = kernel.batch(&views, &params).expect("valid params");
        assert_eq!(result.values.len(), data.len(), "length drift in {name}");

        // Warmup honesty: exactly the headroom prefix is `None`.
        let warmup = kernel.warmup(&params).expect("valid warmup");
        let headroom = warmup_headroom(warmup).min(data.len());
        assert!(
            result.values[..headroom].iter().all(Option::is_none),
            "warmup None prefix drift in {name}"
        );
        // `fin_or_none` boundary: no `NaN` survives inside `Some`.
        assert!(
            result.values.iter().flatten().all(|v| v.is_finite()),
            "non-finite value inside Some in {name}"
        );
        // Batch/update identity over the same input (close-only kernels).
        let mut state = IndicatorState::new();
        for (i, &c) in data.close.iter().enumerate() {
            let stepped = kernel
                .update(&mut state, BarTick::close_only(c), &params)
                .expect("valid update");
            assert_eq!(stepped, result.values[i], "batch/update drift in {name}[{i}]");
        }

        let path = fastmas_baseline_path(name);
        if regen {
            std::fs::write(&path, write_baseline_csv(&result.values)).expect("golden writable");
        }
        let frozen = read_baseline_csv(
            &std::fs::read_to_string(&path).expect("golden frozen; rerun with UPDATE_FASTMAS_BASELINE=1 to write it"),
        );
        assert_series_close(&result.values, &frozen, 1e-12, name, "primary");
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
