//! The one market-leg batch and the one oscillator catalog.

use chrono::TimeZone as _;
use protopyne_types::Bar;
use rust_decimal::Decimal;

use crate::indicator::builtin_registry;
use crate::lite::{
    leg_constants, leg_kernel, leg_refusal, leg_row, lite_legs, LiteParams, LEG_CATALOG,
    LITE_LEG_KINDS,
};

fn bar(close: f64, index: usize) -> Bar {
    let value = Decimal::from_f64_retain(close).expect("finite close");
    Bar::new()
        .ts(chrono::Utc
            .timestamp_opt(1_767_344_400 + index as i64 * 60, 0)
            .single()
            .expect("valid instant"))
        .o(value)
        .h(Decimal::from_f64_retain(close + 0.5).expect("finite high"))
        .l(Decimal::from_f64_retain(close - 0.5).expect("finite low"))
        .c(value)
        .volume(1_000 + index as u64)
        .maybe_adj(None)
        .build()
}

fn ramp(len: usize) -> Vec<Bar> {
    (0..len)
        .map(|index| {
            let close = 100.0 + (index as f64 / 7.0).sin() * 6.0 + index as f64 * 0.05;
            bar(close, index)
        })
        .collect()
}

/// The catalog is the one home for the affine triple: every advertised kind
/// has exactly one row, and every row either names a registered kernel or
/// states why it cannot be evaluated.
#[test]
fn the_catalog_is_a_bijection_against_the_registry() {
    assert_eq!(LITE_LEG_KINDS.len(), 11, "all eleven Pine oscillator ids");
    assert_eq!(LEG_CATALOG.len(), LITE_LEG_KINDS.len());
    for (index, kind) in LITE_LEG_KINDS.iter().enumerate() {
        assert_eq!(
            leg_row(kind).map(|row| row.kind),
            Some(*kind),
            "every advertised id has exactly one row"
        );
        assert_eq!(LEG_CATALOG[index].kind, *kind, "rows keep dispatch order");
    }
    let registry = builtin_registry().expect("the builtin registry builds");
    for kind in LITE_LEG_KINDS {
        if let Some(kernel) = leg_kernel(kind) {
            assert!(
                registry.get(kernel).is_some(),
                "{kind}: kernel {kernel:?} must resolve"
            );
            assert!(leg_constants(kind).is_some(), "{kind}: constants");
            assert!(leg_refusal(kind).is_none(), "{kind}: no refusal");
        } else {
            assert!(
                kind == &"cdv" || kind == &"raw",
                "{kind}: only the refused row and the pass-through source lack a kernel"
            );
            if kind == &"cdv" {
                assert!(leg_refusal(kind).is_some(), "the refused row names why");
                assert!(
                    leg_constants(kind).is_none(),
                    "a refusal substitutes nothing"
                );
            } else {
                assert_eq!(
                    leg_constants(kind),
                    Some((-50.0, 0.0084, 740.0)),
                    "the pass-through source carries the frozen squash"
                );
            }
        }
    }
}

/// The affine constants are the values three transcribed tables carried, so
/// the catalog cannot drift from what the display's frozen calibration pins.
#[test]
fn the_frozen_constants_are_the_shipped_ones() {
    for (kind, expected) in [
        ("aroon", (-50.0, 0.005, 800.0)),
        ("cci", (-50.0, 0.0035, 500.0)),
        ("cmo", (0.0, 0.002, 1000.0)),
        ("mfi", (-50.0, 0.0084, 740.0)),
        ("rsi", (-50.0, 0.0084, 740.0)),
        ("srsi", (-50.0, 0.0006, 7000.0)),
        ("stoch", (-50.0, 0.0005, 7000.0)),
        ("trendsi", (0.0, 0.016, 9000.0)),
        ("voss", (0.0, 1.5, 300.0)),
        ("raw", (-50.0, 0.0084, 740.0)),
    ] {
        assert_eq!(leg_constants(kind), Some(expected), "{kind}");
    }
}

/// The batch is causal: values at an index depend only on bars up to it, so
/// one batch over the tape equals a batch over each prefix. This is the
/// licence for computing once per run instead of once per bar.
#[test]
fn a_prefix_batch_agrees_with_the_whole_tape() {
    let bars = ramp(260);
    let params = LiteParams::default();
    let whole = lite_legs(&bars, &params, 14).expect("whole tape batches");
    for index in 0..bars.len() {
        let prefix = lite_legs(&bars[..=index], &params, 14).expect("prefix batches");
        assert_eq!(
            whole.at(index),
            prefix.at(index),
            "index {index}: the whole-tape batch must agree with the prefix"
        );
    }
}

/// Dialing the risk span changes only the risk ATR series.
#[test]
fn the_risk_span_is_its_own_series() {
    let bars = ramp(120);
    let params = LiteParams::default();
    let default = lite_legs(&bars, &params, 14).expect("default batches");
    let wide = lite_legs(&bars, &params, 21).expect("wide batches");
    assert_eq!(
        default.atr, wide.atr,
        "the context ATR stays at its own span"
    );
    assert_ne!(
        default.atr_risk, wide.atr_risk,
        "the risk span follows the dial"
    );
    assert_eq!(
        default.atr_long.len(),
        wide.atr_long.len(),
        "the squeeze surface is always present"
    );
}

/// The batch refuses an arm outside the decision-path policy, and refuses a
/// refused catalog row with its own reason.
#[test]
fn the_batch_refuses_what_the_policy_refuses() {
    let bars = ramp(40);
    let params = LiteParams {
        fast_kernel: "aroon".to_owned(),
        ..LiteParams::default()
    };
    let error = lite_legs(&bars, &params, 14).expect_err("aroon is catalog-only");
    assert!(
        error.to_string().contains("not a lite leg kind"),
        "got {error}"
    );

    let params = LiteParams {
        fast_kernel: "cdv".to_owned(),
        ..LiteParams::default()
    };
    let error = lite_legs(&bars, &params, 14).expect_err("cdv has no kernel");
    assert!(
        error.to_string().contains("not a lite leg kind"),
        "the decision-path policy refuses before the catalog does, got {error}"
    );
}

/// The schema exposes every dial under its own name.
#[test]
fn the_params_describe_every_dial() {
    use fluent_wvr::Describable;
    let schema = LiteParams::default().describe();
    let properties = schema["properties"]
        .as_object()
        .expect("a component schema has properties");
    for name in [
        "fast_kernel",
        "fast_length",
        "fast_smooth",
        "fast_smooth_length",
        "fast_gain",
        "slow_kernel",
        "slow_length",
        "slow_smooth",
        "slow_smooth_length",
        "slow_gain",
        "eit_alpha",
        "signal_type",
        "signal_length",
        "signal_param1",
        "mean_type",
        "mean_length",
        "mean_param1",
        "ribbon_health_k",
        "price_source",
        "adaptive",
        "session_filter",
        "session",
    ] {
        assert!(
            properties.contains_key(name),
            "schema is missing {name}: {properties:?}"
        );
    }
}
