//! Signal-rule calibration: control groups plus frozen treatment tallies.
//!
//! A rule is untrusted until its controls are quiet and its treatment tallies
//! are frozen. The controls below are chosen so that a *naive* rule would
//! fire on them (a zero-band oscillator, a level the oscillator cannot
//! exceed, a state that stays above a crossing level); the calibrated rule
//! must not. The treatment tallies freeze firing counts and conviction
//! extrema; a threshold edit changes the tally and fails.
//!
//! Only the tallies are persisted (as a golden); the thresholds are literals
//! here, with a fingerprint recorded beside the tallies so that changing a
//! literal without re-freezing also fails.

use super::*;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use protopyne_kernels::{ParamSet, PriceSource};
use protopyne_types::Bar;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::elements::{Signal, SignalOutput};
use crate::view::{MarketData, MarketView};

// Thresholds under calibration. Editing any of these must move a tally.
const MOM_LENGTH: i64 = 10;
const SIGN_SCALE: f64 = 5.0;
const FAST_LENGTH: i64 = 3;
const SLOW_LENGTH: i64 = 8;
const ZERO_BAND: (f64, f64) = (-0.5, 0.5);
const OSC_CEILING: f64 = 100.0;
const STATE_LEVEL: f64 = 100.0;

fn length_params(length: i64) -> ParamSet {
    ParamSet::new().with_int("length", length)
}

fn bars(closes: &[f64]) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(i, close)| {
            let value = Decimal::from_f64_retain(*close).expect("finite close");
            let high = Decimal::from_f64_retain(close + 0.5).expect("finite high");
            let low = Decimal::from_f64_retain(close - 0.5).expect("finite low");
            Bar::new()
                .ts(Utc.timestamp_opt(1_767_344_400 + i as i64 * 60, 0).unwrap())
                .o(value)
                .h(high)
                .l(low)
                .c(value)
                .volume(1_000)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// One rule's behaviour over a series, walked one bar at a time.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Tally {
    firings: u64,
    bars: u64,
    max_conviction: f64,
    max_conviction_step: f64,
}

impl Tally {
    fn rate_per_1000(&self) -> f64 {
        if self.bars == 0 {
            0.0
        } else {
            self.firings as f64 * 1000.0 / self.bars as f64
        }
    }
}

fn walk(signal: &KernelSignal, bars: &[Bar], closes: &[f64]) -> Tally {
    let mut tally = Tally {
        firings: 0,
        bars: closes.len() as u64,
        max_conviction: 0.0,
        max_conviction_step: 0.0,
    };
    let mut last_conviction: Option<f64> = None;
    for end in 1..=bars.len() {
        let data = MarketData {
            bars: &bars[..end],
            chain: None,
            dte_days: 30,
            timeframe: protopyne_types::Timeframe::M1,
        };
        let view = MarketView {
            data: &data,
            closes: &closes[..end],
            as_of: bars[end - 1].ts,
        };
        if let Ok(output) = signal.evaluate(&view) {
            tally.firings += 1;
            tally.max_conviction = tally.max_conviction.max(output.conviction);
            if let Some(previous) = last_conviction {
                tally.max_conviction_step = tally
                    .max_conviction_step
                    .max((output.conviction - previous).abs());
            }
            last_conviction = Some(output.conviction);
        }
    }
    tally
}

fn evaluate_last(signal: &KernelSignal, bars: &[Bar], closes: &[f64]) -> SignalOutput {
    let data = MarketData {
        bars,
        chain: None,
        dte_days: 30,
        timeframe: protopyne_types::Timeframe::M1,
    };
    let view = MarketView {
        data: &data,
        closes,
        as_of: bars.last().expect("non-empty").ts,
    };
    signal
        .evaluate(&view)
        .expect("the fixture must produce a verdict")
}

fn trend_closes(count: usize) -> Vec<f64> {
    (0..count)
        .map(|i| {
            let i = i as f64;
            100.0 + i * 0.4 + (i * 0.7).sin() * 3.0
        })
        .collect()
}

fn reversal_closes() -> Vec<f64> {
    let down = (0..15).map(|i| 200.0 - f64::from(i));
    let up = (0..15).map(|i| 179.0 + f64::from(i) * 2.0);
    down.chain(up).collect()
}

/// Deterministic, mean-zero step walk: the control a trend rule must ignore.
fn zero_drift_walk(count: usize) -> Vec<f64> {
    let mut price = 100.0;
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let step = ((state >> 33) as i64) % 3 - 1;
        price += step as f64;
        out.push(price);
    }
    out
}

fn mom_signal(rule: DirectionRule) -> KernelSignal {
    KernelSignal::new(
        vec![SeriesRef::indicator(
            "mom",
            length_params(MOM_LENGTH),
            PriceSource::Close,
        )],
        rule,
    )
    .expect("mom is registered")
}

fn thresholds_fingerprint() -> String {
    format!(
        "mom_length={MOM_LENGTH};sign_scale={SIGN_SCALE};fast={FAST_LENGTH};slow={SLOW_LENGTH};\
zero_band={ZERO_BAND:?};osc_ceiling={OSC_CEILING};state_level={STATE_LEVEL}"
    )
}

// ── Control group A: must not fire ────────────────────────────────────────

#[test]
fn a_constant_series_sits_inside_the_zero_band() {
    let closes = vec![100.0; 40];
    let bars = bars(&closes);
    let signal = mom_signal(DirectionRule::Band {
        line: 0,
        lower: ZERO_BAND.0,
        upper: ZERO_BAND.1,
        polarity: Polarity::Momentum,
    });
    assert_eq!(walk(&signal, &bars, &closes).firings, 0);
}

#[test]
fn an_all_nan_output_is_silent() {
    // RSI on a flat series is undefined, so the rule never sees a value.
    let closes = vec![100.0; 40];
    let bars = bars(&closes);
    let signal = KernelSignal::new(
        vec![SeriesRef::indicator(
            "rsi",
            length_params(14),
            PriceSource::Close,
        )],
        DirectionRule::Sign {
            line: 0,
            scale: 1.0,
            polarity: Polarity::Momentum,
        },
    )
    .expect("rsi is registered");
    assert_eq!(walk(&signal, &bars, &closes).firings, 0);
}

#[test]
fn a_bounded_oscillator_cannot_exceed_its_ceiling() {
    let closes = zero_drift_walk(200);
    let bars = bars(&closes);
    let signal = KernelSignal::new(
        vec![SeriesRef::indicator(
            "rsi",
            length_params(14),
            PriceSource::Close,
        )],
        DirectionRule::Above {
            line: 0,
            level: OSC_CEILING,
            polarity: Polarity::Momentum,
        },
    )
    .expect("rsi is registered");
    assert_eq!(walk(&signal, &bars, &closes).firings, 0);
}

// ── Control group B: boundary-exact conviction ────────────────────────────

#[test]
fn a_boundary_exact_value_has_zero_conviction() {
    let closes = vec![100.0, 101.0, 104.0];
    let bars = bars(&closes);
    let half_band = (104.0 - 96.0) / 2.0;
    let signal = KernelSignal::new(
        vec![SeriesRef::price(PriceSource::Close)],
        DirectionRule::Band {
            line: 0,
            lower: 96.0,
            upper: 104.0,
            polarity: Polarity::Momentum,
        },
    )
    .expect("band builds");
    let output = evaluate_last(&signal, &bars, &closes);
    assert!(output.conviction.abs() <= f64::EPSILON);
    assert!(output.conviction <= half_band);
}

// ── Control group C: state true, no crossing ──────────────────────────────

#[test]
fn a_state_that_stays_above_never_crosses() {
    let closes: Vec<f64> = (0..40).map(|i| 110.0 + f64::from(i)).collect();
    let bars = bars(&closes);
    let signal = KernelSignal::new(
        vec![SeriesRef::price(PriceSource::Close)],
        DirectionRule::CrossesLevel {
            line: 0,
            level: STATE_LEVEL,
            direction: CrossDir::Above,
            polarity: Polarity::Momentum,
        },
    )
    .expect("cross rule builds");
    assert_eq!(walk(&signal, &bars, &closes).firings, 0);
}

// ── Treatment: frozen tallies ─────────────────────────────────────────────

fn treatment_tallies() -> Value {
    let trend = trend_closes(200);
    let trend_bars = bars(&trend);
    let trend_signal = mom_signal(DirectionRule::Sign {
        line: 0,
        scale: SIGN_SCALE,
        polarity: Polarity::Momentum,
    });
    let trend_tally = walk(&trend_signal, &trend_bars, &trend);

    let reversal = reversal_closes();
    let reversal_bars = bars(&reversal);
    let cross_signal = KernelSignal::new(
        vec![
            SeriesRef::indicator("sma", length_params(FAST_LENGTH), PriceSource::Close),
            SeriesRef::indicator("sma", length_params(SLOW_LENGTH), PriceSource::Close),
        ],
        DirectionRule::CrossesLine {
            fast: 0,
            slow: 1,
            direction: CrossDir::Either,
            polarity: Polarity::Momentum,
        },
    )
    .expect("sma is registered");
    let cross_tally = walk(&cross_signal, &reversal_bars, &reversal);

    json!({
        "trend": {
            "firings": trend_tally.firings,
            "bars": trend_tally.bars,
            "max_conviction": trend_tally.max_conviction,
            "max_conviction_step": trend_tally.max_conviction_step,
            "rate_per_1000": trend_tally.rate_per_1000(),
        },
        "cross": {
            "firings": cross_tally.firings,
            "bars": cross_tally.bars,
            "max_conviction": cross_tally.max_conviction,
            "max_conviction_step": cross_tally.max_conviction_step,
            "rate_per_1000": cross_tally.rate_per_1000(),
        },
        "thresholds": thresholds_fingerprint(),
    })
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("signal_rule_tally.json")
}

#[test]
fn treatment_tallies_match_the_frozen_golden() {
    let actual = serde_json::to_string_pretty(&treatment_tallies()).expect("serializable");
    let path = golden_path();
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::create_dir_all(path.parent().expect("golden has a parent"))
            .expect("baseline directory exists");
        std::fs::write(&path, &actual).expect("tally records");
        return;
    }
    let recorded = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("golden {} records first", path.display()));
    assert_eq!(actual, recorded, "tallies must match the frozen golden");
}

/// A threshold edit must accompany a re-frozen tally: the fingerprint is
/// persisted with the tallies and compared on every run.
#[test]
fn the_threshold_fingerprint_matches_the_golden() {
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        // The tally test writes the golden; this guard only compares it.
        return;
    }
    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(golden_path()).expect("golden records first"),
    )
    .expect("golden is JSON");
    assert_eq!(
        recorded["thresholds"]
            .as_str()
            .expect("fingerprint present"),
        thresholds_fingerprint(),
        "a threshold changed without re-freezing the tally"
    );
}

// ── Realised volatility: measured by the signal, through a kernel ──────────

/// The signal reports realised σ from the registered `stddev` kernel over the
/// tape's own returns: a real measurement, in the same per-bar unit the
/// volatility sizer compares against its target.
#[test]
fn a_kernel_signal_reports_realised_volatility_from_its_registry() {
    let closes = trend_closes(80);
    let tape = bars(&closes);
    let signal = KernelSignal::new(
        vec![SeriesRef::indicator(
            "sma",
            length_params(5),
            PriceSource::Close,
        )],
        DirectionRule::Above {
            line: 0,
            level: 0.0,
            polarity: Polarity::Momentum,
        },
    )
    .expect("sma is registered");
    let output = evaluate_last(&signal, &tape, &closes);
    let volatility = output.volatility.expect("enough bars to measure σ");
    assert!(
        volatility.is_finite() && volatility > 0.0,
        "a trending tape has a positive realised σ: {volatility}"
    );
    // A per-bar figure: the fixture's own step is ~0.4 on ~100, so σ sits far
    // below 1.0 and far above 1e-9.
    assert!(
        volatility < 0.1 && volatility > 1e-9,
        "realised σ must be a per-bar return scale, got {volatility}"
    );

    // Too few bars is absence, never a zero.
    let short = trend_closes(crate::signals_kernel::KERNEL_VOLATILITY_WINDOW);
    let short_bars = bars(&short);
    let short_output = evaluate_last(&signal, &short_bars, &short);
    assert_eq!(
        short_output.volatility, None,
        "a window that cannot be filled reports absence, never a zero"
    );
}
