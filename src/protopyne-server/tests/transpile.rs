use super::*;
use std::collections::HashMap;

use protopyne_kernels::{EmaIndicator, Ohlcv, ParamSet, SmaIndicator};

/// Five canonical scripts the spike must accept.
fn must_transpile() -> Vec<&'static str> {
    vec![
        "//@version=6\nindicator(\"Ribbon\", overlay=true)\ns = ta.sma(close, 14)\nplot(s, \"sma\")\n",
        "//@version=6\nindicator(\"Trend\", overlay=true)\nlen = input.int(14, \"len\")\ne = ta.ema(close, len)\nplot(e, \"ema\")\n",
        "//@version=6\nindicator(\"Open\", overlay=true)\ns = ta.sma(open, 10)\nplot(s, \"sma-open\")\n",
        "//@version=6\nindicator(\"Pair\", overlay=true)\nlen = input.int(14, \"len\")\ns = ta.sma(close, len)\ne = ta.ema(close, len)\nplot(s, \"s\")\nplot(e, \"e\")\n",
        "//@version=6\nindicator(\"Band\", overlay=true)\nmult = input.float(2.0, \"mult\")\nbasis = ta.sma(close, 14)\nupper = basis + mult\nplot(upper, \"upper\")\n",
    ]
}

/// Six scripts the spike must refuse, never miscompile.
fn must_reject() -> Vec<&'static str> {
    vec![
        "//@version=6\nindicator(\"Switch\", overlay=true)\nimport posthum4n/x as x\ns = switch close\n    1 => ta.sma(close, 14)\n    => ta.ema(close, 14)\nplot(s)\n",
        "//@version=6\nindicator(\"Ternary\", overlay=true)\ns = close > open ? ta.sma(close, 14) : close < open ? ta.ema(close, 14) : na\nplot(s)\n",
        "//@version=6\nindicator(\"Security\", overlay=true)\ns = request.security(syminfo.tickerid, \"D\", ta.sma(close, 14))\nplot(s)\n",
        "//@version=6\nindicator(\"Intrabar\", overlay=true)\nvarip x = 0\nx := nz(x[1]) + 1\ns = ta.sma(close, 14) + x\nplot(s)\n",
        "//@version=6\nstrategy(\"Backtest\", overlay=true)\ns = ta.sma(close, 14)\nplot(s)\n",
        "//@version=6\nindicator(\"Loop\", overlay=true)\nsum = 0.0\nfor i = 0 to 13\n    sum := sum + close[i]\nplot(sum)\n",
    ]
}

fn closes(count: usize) -> Vec<f64> {
    (0..count).map(|i| 600.0 + (i as f64) * 0.5).collect()
}

#[test]
fn accepted_scripts_lower_expected_bindings() {
    let outcome = compile_spike(must_transpile()[0]);
    let SpikeOutcome::Accepted { lowered, rust } = outcome else {
        panic!("first canonical script must be accepted");
    };
    assert_eq!(lowered.bindings.len(), 1);
    assert_eq!(lowered.plots, vec!["s".to_owned()]);
    assert!(rust.contains("SmaIndicator"), "emitted Rust must call the real kernel, got:\n{rust}");

    let outcome = compile_spike(must_transpile()[1]);
    let SpikeOutcome::Accepted { lowered, rust } = outcome else {
        panic!("input-bound ema script must be accepted");
    };
    assert!(matches!(
        lowered.bindings[0].hair,
        Hair::Rolling { ref kernel, ref length, .. }
        if kernel == "ema" && matches!(length, Length::Input(name, 14) if name == "len")
    ));
    assert!(rust.contains("EmaIndicator"));
}

#[test]
fn emitted_rust_is_syntactically_valid() {
    for source in must_transpile() {
        let SpikeOutcome::Accepted { rust, .. } = compile_spike(source) else {
            panic!("canonical script refused:\n{source}");
        };
        syn::parse_str::<syn::File>(&rust)
            .unwrap_or_else(|e| panic!("emitted Rust must parse: {e}\n{rust}"));
    }
}

#[test]
fn evaluated_scripts_match_kernel_goldens() {
    let bars = closes(30);
    let frame = Ohlcv::close_only(&bars);
    for (index, source) in must_transpile().iter().enumerate() {
        let SpikeOutcome::Accepted { lowered, .. } = compile_spike(source) else {
            panic!("canonical script {index} refused");
        };
        let inputs: HashMap<String, i64> =
            [("len".to_owned(), 14), ("mult".to_owned(), 2)].into_iter().collect();
        let evaluated = evaluate_lowered(&lowered, &bars, &inputs).expect("evaluation runs");
        let by_name: HashMap<&str, &[Option<f64>]> = evaluated
            .iter()
            .map(|(name, values)| (name.as_str(), values.as_slice()))
            .collect();
        for binding in &lowered.bindings {
            let Hair::Rolling { kernel, length, .. } = &binding.hair else {
                continue;
            };
            let values = by_name.get(binding.name.as_str()).expect("rolling binding evaluated");
            let resolved = match length {
                Length::Literal(value) => *value,
                Length::Input(name, default) => inputs.get(name).copied().unwrap_or(*default),
            };
            let params = ParamSet::new().with_int("length", resolved);
            let expected = match kernel.as_str() {
                "sma" => SmaIndicator::new().batch(&frame, &params).expect("kernel runs").values,
                _ => EmaIndicator::new().batch(&frame, &params).expect("kernel runs").values,
            };
            assert_eq!(values.len(), expected.len());
            for (got, want) in values.iter().zip(expected.iter()) {
                match (got, want) {
                    (Some(got), Some(want)) => {
                        assert!((got - want).abs() <= 1e-9, "script {index} drifts from golden");
                    }
                    (None, None) => {}
                    _ => panic!("script {index} warmup shape differs from golden"),
                }
            }
        }
    }
}

#[test]
fn rejected_scripts_name_their_reason() {
    for source in must_reject() {
        match compile_spike(source) {
            SpikeOutcome::Refused { refusal } => {
                assert!(!refusal.reasons.is_empty(), "refusal must explain itself");
            }
            SpikeOutcome::Accepted { .. } => panic!("must-REJECT script accepted:\n{source}"),
        }
    }
    // Version, declaration, and plot hygiene also refuse loudly.
    assert!(matches!(
        compile_spike("indicator(\"No version\")\ns = ta.sma(close, 14)\nplot(s)\n"),
        SpikeOutcome::Refused { .. }
    ));
    assert!(matches!(
        compile_spike("//@version=5\nindicator(\"Old\")\ns = ta.sma(close, 14)\nplot(s)\n"),
        SpikeOutcome::Refused { .. }
    ));
    assert!(matches!(
        compile_spike("//@version=6\nstrategy(\"S\")\ns = ta.sma(close, 14)\nplot(s)\n"),
        SpikeOutcome::Refused { .. }
    ));
    assert!(matches!(
        compile_spike("//@version=6\nindicator(\"Dangling\")\ns = ta.sma(close, 14)\nplot(missing)\n"),
        SpikeOutcome::Refused { .. }
    ));
}

#[test]
fn calibration_is_green_before_any_caching() {
    let tally = calibrate(
        &must_transpile().iter().map(|s| s as &str).collect::<Vec<_>>(),
        &must_reject().iter().map(|s| s as &str).collect::<Vec<_>>(),
    );
    assert_eq!(tally.must_transpile, 5);
    assert_eq!(tally.must_reject, 6);
    assert!((tally.accept_rate() - 1.0).abs() < f64::EPSILON);
    assert!((tally.reject_rate() - 1.0).abs() < f64::EPSILON);
    assert!(tally.is_green(), "spike ships only at precision=recall=1.0, got {tally:?}");
}

#[test]
fn hir_helpers_split_and_lower_edge_cases() {
    assert!(split_args("").is_empty());
    assert_eq!(split_args("a, f(b, c)").len(), 2);
    assert!(split_assignment("a == b").is_none());
    let inputs: HashMap<String, i64> = HashMap::new();
    assert!(lower_length("0", &inputs).is_err());
    assert!(lower_length("mystery", &inputs).is_err());
    assert!(parse_input_binding("len = input.int(0, \"len\")").is_none());
    assert!(parse_plot("not-a-plot").is_none());
}
