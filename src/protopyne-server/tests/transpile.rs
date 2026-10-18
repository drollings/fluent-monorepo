use super::*;
use std::collections::HashMap;

use protopyne_kernels::{
    EmaIndicator, MidpointIndicator, Ohlcv, ParamSet, SmaIndicator, TrimaIndicator,
};

/// Nine canonical scripts the spike must accept: the seven rolling-average
/// shapes plus the two verified single-output overlaps (`trima`, `midpoint`).
fn must_transpile() -> Vec<&'static str> {
    vec![
        "//@version=6\nindicator(\"Ribbon\", overlay=true)\ns = ta.sma(close, 14)\nplot(s, \"sma\")\n",
        "//@version=6\nindicator(\"Trend\", overlay=true)\nlen = input.int(14, \"len\")\ne = ta.ema(close, len)\nplot(e, \"ema\")\n",
        "//@version=6\nindicator(\"Open\", overlay=true)\ns = ta.sma(open, 10)\nplot(s, \"sma-open\")\n",
        "//@version=6\nindicator(\"Pair\", overlay=true)\nlen = input.int(14, \"len\")\ns = ta.sma(close, len)\ne = ta.ema(close, len)\nplot(s, \"s\")\nplot(e, \"e\")\n",
        "//@version=6\nindicator(\"Band\", overlay=true)\nmult = input.float(2.0, \"mult\")\nbasis = ta.sma(close, 14)\nupper = basis + mult\nplot(upper, \"upper\")\n",
        "//@version=6\nindicator(\"Mixed\", overlay=true)\nx = 2 * 3 + 4\ns = ta.sma(close, 14)\nplot(s, \"s\")\n",
        "//@version=6\nindicator(\"LeftAssoc\", overlay=true)\ny = 100 / 10 / 2\nz = 10 - 2 * 3\ne = ta.ema(close, 14)\nplot(e, \"e\")\n",
        "//@version=6\nindicator(\"Tri\", overlay=true)\nt = ta.trima(close, 14)\nplot(t, \"trima\")\n",
        "//@version=6\nindicator(\"Mid\", overlay=true)\nlen = input.int(14, \"len\")\nm = ta.midpoint(close, len)\nplot(m, \"mid\")\n",
    ]
}

/// Fourteen scripts the spike must refuse, never miscompile: the six original
/// shapes plus unbalanced parentheses, an empty operator side, a version
/// number that merely starts with 6, a ternary nested in an otherwise
/// canonical script, a multi-leg overlap outside the single-series accept
/// set, a momentum kernel (overlaps only), and the two previously
/// substituted series (a binary expression and a nested kernel fed to a
/// rolling call).
fn must_reject() -> Vec<&'static str> {
    vec![
        "//@version=6\nindicator(\"Switch\", overlay=true)\nimport posthum4n/x as x\ns = switch close\n    1 => ta.sma(close, 14)\n    => ta.ema(close, 14)\nplot(s)\n",
        "//@version=6\nindicator(\"Ternary\", overlay=true)\ns = close > open ? ta.sma(close, 14) : close < open ? ta.ema(close, 14) : na\nplot(s)\n",
        "//@version=6\nindicator(\"Security\", overlay=true)\ns = request.security(syminfo.tickerid, \"D\", ta.sma(close, 14))\nplot(s)\n",
        "//@version=6\nindicator(\"Intrabar\", overlay=true)\nvarip x = 0\nx := nz(x[1]) + 1\ns = ta.sma(close, 14) + x\nplot(s)\n",
        "//@version=6\nstrategy(\"Backtest\", overlay=true)\ns = ta.sma(close, 14)\nplot(s)\n",
        "//@version=6\nindicator(\"Loop\", overlay=true)\nsum = 0.0\nfor i = 0 to 13\n    sum := sum + close[i]\nplot(sum)\n",
        "//@version=6\nindicator(\"Unbalanced\", overlay=true)\nx = (2 + 3\ns = ta.sma(close, 14)\nplot(s)\n",
        "//@version=6\nindicator(\"EmptySide\", overlay=true)\nx = 2 +\ns = ta.sma(close, 14)\nplot(s)\n",
        "//@version=60\nindicator(\"Sixty\", overlay=true)\ns = ta.sma(close, 14)\nplot(s)\n",
        "//@version=6\nindicator(\"NestedTernary\", overlay=true)\ns = ta.sma(close, 14 > 10 ? 12 : 16)\nplot(s)\n",
        "//@version=6\nindicator(\"Bands\", overlay=true)\nb = ta.bbands(close, 14)\nplot(b, \"bands\")\n",
        "//@version=6\nindicator(\"Momentum\", overlay=true)\nr = ta.rsi(close, 14)\nplot(r, \"rsi\")\n",
        "//@version=6\nindicator(\"BinarySeries\", overlay=true)\ns = ta.sma(close + open, 14)\nplot(s, \"s\")\n",
        "//@version=6\nindicator(\"NestedKernel\", overlay=true)\ns = ta.sma(ta.ema(close, 9), 14)\nplot(s, \"s\")\n",
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
    assert!(
        rust.contains("SmaIndicator"),
        "emitted Rust must call the real kernel, got:\n{rust}"
    );

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
fn mixed_arithmetic_respects_precedence_hir_shape() {
    // Constant-foldable arithmetic asserts HIR shape (evaluation only runs
    // rolling kernel bindings through `evaluate_lowered`).
    let inputs: HashMap<String, i64> = HashMap::new();
    let bound: std::collections::HashSet<String> = std::collections::HashSet::new();
    let binary = |op: char, left: Hair, right: Hair| Hair::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
    };
    // `2 * 3 + 4` groups as `(2 * 3) + 4`.
    assert_eq!(
        lower_expr("2 * 3 + 4", &inputs, &bound),
        Ok(binary(
            '+',
            binary('*', Hair::Const(2.0), Hair::Const(3.0)),
            Hair::Const(4.0)
        ))
    );
    // `10 - 2 * 3` groups as `10 - (2 * 3)`.
    assert_eq!(
        lower_expr("10 - 2 * 3", &inputs, &bound),
        Ok(binary(
            '-',
            Hair::Const(10.0),
            binary('*', Hair::Const(2.0), Hair::Const(3.0))
        ))
    );
    // `100 / 10 / 2` groups left-associatively as `(100 / 10) / 2`.
    assert_eq!(
        lower_expr("100 / 10 / 2", &inputs, &bound),
        Ok(binary(
            '/',
            binary('/', Hair::Const(100.0), Hair::Const(10.0)),
            Hair::Const(2.0)
        ))
    );
    // Unresolvable scans refuse instead of guessing.
    assert!(lower_expr("(2 + 3", &inputs, &bound).is_err());
    assert!(lower_expr("2 + ", &inputs, &bound).is_err());
    // Current behavior pinned: unary `-x` refuses, `1e-3` is a literal.
    assert!(lower_expr("-x", &inputs, &bound).is_err());
    assert!(matches!(
        lower_expr("1e-3", &inputs, &bound),
        Ok(Hair::Const(value)) if (value - 0.001).abs() < 1e-12
    ));
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
    // Distinct legs prove each binding resolves its own series: the `open`
    // script must match the kernel over opens, never over closes.
    let close = closes(30);
    let open: Vec<f64> = close.iter().map(|v| v + 1.0).collect();
    let high: Vec<f64> = close.iter().map(|v| v + 2.0).collect();
    let low: Vec<f64> = close.iter().map(|v| v - 2.0).collect();
    let volume: Vec<f64> = close
        .iter()
        .enumerate()
        .map(|(i, _)| 1_000.0 + i as f64)
        .collect();
    let frame = Ohlcv {
        open: &open,
        high: &high,
        low: &low,
        close: &close,
        volume: &volume,
    };
    for (index, source) in must_transpile().iter().enumerate() {
        let SpikeOutcome::Accepted { lowered, .. } = compile_spike(source) else {
            panic!("canonical script {index} refused");
        };
        let inputs: HashMap<String, i64> = [("len".to_owned(), 14), ("mult".to_owned(), 2)]
            .into_iter()
            .collect();
        let evaluated = evaluate_lowered(&lowered, &frame, &inputs).expect("evaluation runs");
        let by_name: HashMap<&str, &[Option<f64>]> = evaluated
            .iter()
            .map(|(name, values)| (name.as_str(), values.as_slice()))
            .collect();
        for binding in &lowered.bindings {
            let Hair::Rolling {
                kernel,
                length,
                series,
            } = &binding.hair
            else {
                continue;
            };
            let Hair::Ohlcv(leg) = series.as_ref() else {
                panic!("canonical script {index} binds a non-leg series");
            };
            let leg_values: &[f64] = match leg.as_str() {
                "open" => &open,
                "high" => &high,
                "low" => &low,
                "close" => &close,
                "volume" => &volume,
                unknown => panic!("canonical script {index} binds unknown leg {unknown}"),
            };
            let leg_frame = Ohlcv::close_only(leg_values);
            let values = by_name
                .get(binding.name.as_str())
                .expect("rolling binding evaluated");
            let resolved = match length {
                Length::Literal(value) => *value,
                Length::Input(name, default) => inputs.get(name).copied().unwrap_or(*default),
            };
            let params = ParamSet::new().with_int("length", resolved);
            let expected = match kernel.as_str() {
                "sma" => {
                    SmaIndicator::new()
                        .batch(&leg_frame, &params)
                        .expect("kernel runs")
                        .values
                }
                "ema" => {
                    EmaIndicator::new()
                        .batch(&leg_frame, &params)
                        .expect("kernel runs")
                        .values
                }
                "trima" => {
                    TrimaIndicator::new()
                        .batch(&leg_frame, &params)
                        .expect("kernel runs")
                        .values
                }
                _ => {
                    MidpointIndicator::new()
                        .batch(&leg_frame, &params)
                        .expect("kernel runs")
                        .values
                }
            };
            assert_eq!(values.len(), expected.len());
            for (got, want) in values.iter().zip(expected.iter()) {
                match (got, want) {
                    (Some(got), Some(want)) => {
                        assert!(
                            (got - want).abs() <= 1e-9,
                            "script {index} drifts from golden"
                        );
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
    // A fractional length is refused, never truncated.
    assert!(matches!(
        compile_spike("//@version=6\nindicator(\"Frac\", overlay=true)\nn = input.float(2.9, \"n\")\ns = ta.sma(close, 14)\nplot(s)\n"),
        SpikeOutcome::Refused { .. }
    ));
    assert!(matches!(
        compile_spike(
            "//@version=6\nindicator(\"Dangling\")\ns = ta.sma(close, 14)\nplot(missing)\n"
        ),
        SpikeOutcome::Refused { .. }
    ));
}

#[test]
fn calibration_is_green_before_any_caching() {
    let tally = calibrate(
        &must_transpile()
            .iter()
            .map(|s| s as &str)
            .collect::<Vec<_>>(),
        &must_reject().iter().map(|s| s as &str).collect::<Vec<_>>(),
    );
    assert_eq!(tally.must_transpile, 9);
    assert_eq!(tally.must_reject, 14);
    assert!((tally.accept_rate() - 1.0).abs() < f64::EPSILON);
    assert!((tally.reject_rate() - 1.0).abs() < f64::EPSILON);
    assert!(
        tally.is_green(),
        "spike ships only at precision=recall=1.0, got {tally:?}"
    );
}

#[test]
fn acceptance_is_parse_plus_numeric_not_rustc() {
    // Acceptance bar, stated plainly: emitted Rust must syn-parse AND accepted
    // scripts must match kernel goldens numerically. Emitted Rust is NEVER
    // handed to `rustc` here — hermeticity: tests must not invoke a
    // toolchain, so schematic emission plus numeric parity is the bar.
    for source in must_transpile() {
        let SpikeOutcome::Accepted { rust, lowered } = compile_spike(source) else {
            panic!("canonical script refused:\n{source}");
        };
        syn::parse_str::<syn::File>(&rust)
            .unwrap_or_else(|e| panic!("emitted Rust must parse: {e}\n{rust}"));
        let bars = closes(30);
        let frame = Ohlcv::close_only(&bars);
        let inputs: HashMap<String, i64> = [("len".to_owned(), 14), ("mult".to_owned(), 2)]
            .into_iter()
            .collect();
        let evaluated = evaluate_lowered(&lowered, &frame, &inputs).expect("evaluation runs");
        assert!(
            !evaluated.is_empty(),
            "accepted script must evaluate rolling bindings"
        );
    }
    // Non-rolling bindings emit comment-only lines, never `let` bindings.
    let SpikeOutcome::Accepted { rust, .. } = compile_spike(must_transpile()[4]) else {
        panic!("band script must be accepted");
    };
    assert!(rust
        .lines()
        .any(|line| line.trim_start().starts_with("//") && line.contains("'upper'")));
    assert!(!rust.contains("let _upper"));
    // Input-length accessors name the bound input: no anonymous receiver that
    // could resolve to the wrong `ParamSet` entry.
    let SpikeOutcome::Accepted { rust, .. } = compile_spike(must_transpile()[1]) else {
        panic!("input-bound ema script must be accepted");
    };
    assert!(
        rust.contains("params.int(\"ema\", \"len\")"),
        "emitted accessor must name its input, got:\n{rust}"
    );
    assert!(!rust.contains("params.int(\"_\""));
}

#[test]
fn overlap_scripts_lower_emit_and_evaluate() {
    // The widened accept set names its kernel in HIR, emission, and
    // evaluation; multi-leg and momentum kernels stay refused.
    let outcome = compile_spike(must_transpile()[7]);
    let SpikeOutcome::Accepted { lowered, rust } = outcome else {
        panic!("trima script must be accepted");
    };
    assert!(matches!(
        lowered.bindings[0].hair,
        Hair::Rolling { ref kernel, ref length, .. }
        if kernel == "trima" && matches!(length, Length::Literal(14))
    ));
    assert!(
        rust.contains("TrimaIndicator"),
        "emission names the kernel, got:\n{rust}"
    );
    syn::parse_str::<syn::File>(&rust).expect("trima emission parses");

    let outcome = compile_spike(must_transpile()[8]);
    let SpikeOutcome::Accepted { lowered, rust } = outcome else {
        panic!("midpoint script must be accepted");
    };
    assert!(matches!(
        lowered.bindings[0].hair,
        Hair::Rolling { ref kernel, ref length, .. }
        if kernel == "midpoint" && matches!(length, Length::Input(name, 14) if name == "len")
    ));
    assert!(
        rust.contains("MidpointIndicator"),
        "emission names the kernel, got:\n{rust}"
    );
    syn::parse_str::<syn::File>(&rust).expect("midpoint emission parses");

    for source in [&must_reject()[10], &must_reject()[11]] {
        assert!(
            matches!(compile_spike(source), SpikeOutcome::Refused { .. }),
            "non-accept-set kernel refused:\n{source}"
        );
    }
}

#[test]
fn emitter_refuses_unsupported_series_instead_of_substituting() {
    fn rolling_over(series: Hair) -> Lowered {
        Lowered {
            bindings: vec![Binding {
                name: "s".to_owned(),
                hair: Hair::Rolling {
                    kernel: "sma".to_owned(),
                    length: Length::Literal(14),
                    series: Box::new(series),
                },
            }],
            plots: vec!["s".to_owned()],
        }
    }
    let close = || Hair::Ohlcv("close".to_owned());
    let unsupported = [
        Hair::Binary {
            op: '+',
            left: Box::new(close()),
            right: Box::new(Hair::Ohlcv("open".to_owned())),
        },
        Hair::Where {
            cond: Box::new(Hair::Const(1.0)),
            then: Box::new(close()),
            otherwise: Box::new(close()),
        },
        Hair::Shift {
            offset: 1,
            series: Box::new(close()),
        },
        Hair::Rolling {
            kernel: "ema".to_owned(),
            length: Length::Literal(9),
            series: Box::new(close()),
        },
        Hair::Call {
            kernel: "sma".to_owned(),
            args: vec![close()],
        },
        // A bound name needs binding resolution the spike does not do;
        // a constant is not a series at all. Both refuse rather than
        // falling back to closes.
        Hair::Name("basis".to_owned()),
        Hair::Const(2.0),
    ];
    for series in unsupported {
        let reason = emit_rust(&rolling_over(series)).expect_err("unsupported series refuses");
        assert!(!reason.is_empty(), "refusal explains itself");
    }
    for leg in ["open", "high", "low", "close", "volume"] {
        let rust =
            emit_rust(&rolling_over(Hair::Ohlcv(leg.to_owned()))).expect("a plain leg emits");
        assert!(
            rust.contains(&format!("values.{leg}")),
            "the leg feeds the kernel, got:\n{rust}"
        );
    }
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
    assert!(parse_input_binding("n = input.float(2.9, \"n\")").is_none());
    assert_eq!(
        parse_input_binding("n = input.float(3.0, \"n\")"),
        Some(("n".to_owned(), 3))
    );
    assert!(parse_plot("not-a-plot").is_none());
}

#[test]
fn evaluation_refuses_unknown_input_names() {
    // A length bound to an input name with no supplied value refuses naming
    // the input — substituting the parse-time default would silently
    // evaluate a script the caller never configured.
    let lowered = Lowered {
        bindings: vec![Binding {
            name: "s".to_owned(),
            hair: Hair::Rolling {
                kernel: "sma".to_owned(),
                length: Length::Input("mystery".to_owned(), 14),
                series: Box::new(Hair::Ohlcv("close".to_owned())),
            },
        }],
        plots: vec!["s".to_owned()],
    };
    let empty: HashMap<String, i64> = HashMap::new();
    let bars = closes(30);
    let frame = Ohlcv::close_only(&bars);
    let err = evaluate_lowered(&lowered, &frame, &empty)
        .expect_err("unknown input must refuse, never substitute the default");
    assert!(
        err.to_string().contains("mystery"),
        "refusal names the input, got: {err}"
    );
    let supplied: HashMap<String, i64> = [("mystery".to_owned(), 14)].into_iter().collect();
    let evaluated =
        evaluate_lowered(&lowered, &frame, &supplied).expect("supplied input evaluates");
    assert_eq!(evaluated.len(), 1);
    assert_eq!(evaluated[0].0, "s");
    assert_eq!(evaluated[0].1.len(), 30);
}

#[test]
fn evaluation_resolves_legs_and_refuses_non_leg_series() {
    fn rolling_over(series: Hair) -> Lowered {
        Lowered {
            bindings: vec![Binding {
                name: "s".to_owned(),
                hair: Hair::Rolling {
                    kernel: "sma".to_owned(),
                    length: Length::Literal(14),
                    series: Box::new(series),
                },
            }],
            plots: vec!["s".to_owned()],
        }
    }
    let close = closes(30);
    let open: Vec<f64> = close.iter().map(|v| v + 1.0).collect();
    let frame = Ohlcv {
        open: &open,
        high: &close,
        low: &close,
        close: &close,
        volume: &close,
    };
    let inputs: HashMap<String, i64> = HashMap::new();
    // Opens average opens: every output sits exactly 1.0 above the
    // closes-run golden on this fixture.
    let evaluated = evaluate_lowered(
        &rolling_over(Hair::Ohlcv("open".to_owned())),
        &frame,
        &inputs,
    )
    .expect("a leg resolves");
    let params = ParamSet::new().with_int("length", 14);
    let over_closes = SmaIndicator::new()
        .batch(&Ohlcv::close_only(&close), &params)
        .expect("kernel runs")
        .values;
    assert_eq!(evaluated.len(), 1);
    for (got, want) in evaluated[0].1.iter().zip(over_closes.iter()) {
        match (got, want) {
            (Some(got), Some(want)) => {
                assert!((got - want - 1.0).abs() <= 1e-9, "opens average opens");
            }
            (None, None) => {}
            _ => panic!("warmup shape must match"),
        }
    }
    // Anything but a frame leg refuses: the evaluator never silently runs a
    // computed, bound, constant, or unknown series over closes.
    for series in [
        Hair::Binary {
            op: '+',
            left: Box::new(Hair::Ohlcv("close".to_owned())),
            right: Box::new(Hair::Ohlcv("open".to_owned())),
        },
        Hair::Name("basis".to_owned()),
        Hair::Const(2.0),
        Hair::Shift {
            offset: 1,
            series: Box::new(Hair::Ohlcv("close".to_owned())),
        },
        Hair::Ohlcv("vwap".to_owned()),
    ] {
        let err = evaluate_lowered(&rolling_over(series), &frame, &inputs)
            .expect_err("a non-leg series refuses");
        assert!(!err.to_string().is_empty(), "the refusal explains itself");
    }
}
