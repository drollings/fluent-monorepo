//! Kernel-signal guards: the rule vocabulary must reach the ordinary signal
//! families (MA cross, secondary outputs, oscillator bands) and must refuse —
//! never default — when data or configuration cannot support a decision.

use super::*;
use chrono::{TimeZone, Utc};
use protopyne_kernels::{Indicator as _, Ohlcv, ParamSet, PriceSource, SmaIndicator};
use protopyne_types::Bar;
use rust_decimal::prelude::ToPrimitive as _;
use rust_decimal::Decimal;

use crate::elements::Signal;
use crate::proposal::Direction;
use crate::view::{MarketData, MarketView};

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

fn view<'a>(data: &'a MarketData<'a>, closes: &'a [f64]) -> MarketView<'a> {
    MarketView {
        data,
        closes,
        as_of: data.bars.last().map_or_else(Utc::now, |bar| bar.ts),
    }
}

fn data(bars: &[Bar]) -> MarketData<'_> {
    MarketData {
        bars,
        chain: None,
        dte_days: 30,
        timeframe: protopyne_types::Timeframe::M1,
    }
}

fn length_params(length: i64) -> ParamSet {
    ParamSet::new().with_int("length", length)
}

/// A descending-then-ascending series: the fast SMA crosses the slow SMA at
/// least once, and stays crossed afterwards.
fn v_closes() -> Vec<f64> {
    let down = (0..15).map(|i| 200.0 - f64::from(i));
    let up = (0..15).map(|i| 179.0 + f64::from(i) * 2.0);
    down.chain(up).collect()
}

fn sign_rule() -> DirectionRule {
    DirectionRule::Sign {
        line: 0,
        scale: 1.0,
        polarity: Polarity::Momentum,
    }
}

#[test]
fn unknown_indicator_id_is_refused_at_construction() {
    let err = KernelSignal::new(
        vec![SeriesRef::indicator(
            "nope",
            length_params(3),
            PriceSource::Close,
        )],
        sign_rule(),
    )
    .expect_err("unknown id refuses");
    assert!(err.to_string().contains("nope"), "{err}");
}

#[test]
fn a_rule_naming_a_missing_line_is_refused() {
    let err = KernelSignal::new(
        vec![SeriesRef::price(PriceSource::Close)],
        DirectionRule::CrossesLine {
            fast: 0,
            slow: 1,
            direction: CrossDir::Either,
            polarity: Polarity::Momentum,
        },
    )
    .expect_err("missing line refuses");
    assert!(err.to_string().contains("line 1"), "{err}");
}

#[test]
fn insufficient_warmup_refuses() {
    let bars = bars(&[100.0, 101.0, 102.0]);
    let closes: Vec<f64> = bars.iter().map(|b| b.c.to_f64().unwrap()).collect();
    let signal = KernelSignal::new(
        vec![SeriesRef::indicator(
            "sma",
            length_params(14),
            PriceSource::Close,
        )],
        sign_rule(),
    )
    .expect("known id");
    let data = data(&bars);
    let err = signal
        .evaluate(&view(&data, &closes))
        .expect_err("warmup refuses");
    assert!(err.to_string().contains("warmup"), "{err}");
}

#[test]
fn a_non_finite_latest_value_refuses() {
    // A flat series leaves RSI's gain/loss ratio undefined, so the latest
    // output is not finite.
    let bars = bars(&[100.0; 40]);
    let closes: Vec<f64> = bars.iter().map(|b| b.c.to_f64().unwrap()).collect();
    let signal = KernelSignal::new(
        vec![SeriesRef::indicator(
            "rsi",
            length_params(14),
            PriceSource::Close,
        )],
        sign_rule(),
    )
    .expect("known id");
    let data = data(&bars);
    assert!(signal.evaluate(&view(&data, &closes)).is_err());
}

#[test]
fn a_named_secondary_output_is_selectable() {
    let bars = bars(&v_closes());
    let closes: Vec<f64> = bars.iter().map(|b| b.c.to_f64().unwrap()).collect();
    let macd = ParamSet::new()
        .with_int("fast_length", 3)
        .with_int("slow_length", 6)
        .with_int("signal_length", 3);
    let signal = KernelSignal::new(
        vec![SeriesRef::named(
            "macd",
            "signal",
            macd.clone(),
            PriceSource::Close,
        )],
        sign_rule(),
    )
    .expect("known id");
    let data = data(&bars);
    assert!(
        signal.evaluate(&view(&data, &closes)).is_ok(),
        "the signal line is a real output"
    );

    let missing = KernelSignal::new(
        vec![SeriesRef::named("macd", "nope", macd, PriceSource::Close)],
        sign_rule(),
    )
    .expect("known id");
    let err = missing
        .evaluate(&view(&data, &closes))
        .expect_err("unknown output refuses");
    assert!(err.to_string().contains("nope"), "{err}");
}

#[test]
fn an_ma_crossover_fires_only_on_the_crossing_bar() {
    let closes = v_closes();
    let bars = bars(&closes);
    let signal = KernelSignal::new(
        vec![
            SeriesRef::indicator("sma", length_params(3), PriceSource::Close),
            SeriesRef::indicator("sma", length_params(8), PriceSource::Close),
        ],
        DirectionRule::CrossesLine {
            fast: 0,
            slow: 1,
            direction: CrossDir::Either,
            polarity: Polarity::Momentum,
        },
    )
    .expect("known ids");

    // Expected crossings from one full-series batch: a sign flip of the
    // fast-minus-slow spread between two finite samples.
    let fast = SmaIndicator::new()
        .batch(&Ohlcv::close_only(&closes), &length_params(3))
        .expect("fast sma");
    let slow = SmaIndicator::new()
        .batch(&Ohlcv::close_only(&closes), &length_params(8))
        .expect("slow sma");
    let expected: usize = (1..closes.len())
        .filter(|&i| {
            match (
                fast.values[i - 1],
                slow.values[i - 1],
                fast.values[i],
                slow.values[i],
            ) {
                (Some(fp), Some(sp), Some(fc), Some(sc)) => {
                    (fc - sc).signum() != (fp - sp).signum()
                }
                _ => false,
            }
        })
        .count();
    assert!(expected > 0, "the fixture must cross at least once");

    let mut fired = 0;
    for end in 1..=closes.len() {
        let prefix = data(&bars[..end]);
        let prefix_closes = &closes[..end];
        if signal.evaluate(&view(&prefix, prefix_closes)).is_ok() {
            fired += 1;
        }
    }
    assert_eq!(fired, expected, "cross rules fire on the crossing bar only");
}

#[test]
fn polarity_reversion_flips_the_direction() {
    let bars = bars(&[100.0, 101.0, 105.0]);
    let closes: Vec<f64> = bars.iter().map(|b| b.c.to_f64().unwrap()).collect();
    let data = data(&bars);
    let band = |polarity| {
        KernelSignal::new(
            vec![SeriesRef::price(PriceSource::Close)],
            DirectionRule::Band {
                line: 0,
                lower: 96.0,
                upper: 104.0,
                polarity,
            },
        )
        .expect("band builds")
    };
    let momentum = band(Polarity::Momentum)
        .evaluate(&view(&data, &closes))
        .expect("above the band");
    let reversion = band(Polarity::Reversion)
        .evaluate(&view(&data, &closes))
        .expect("above the band");
    assert_eq!(momentum.direction, Direction::Long);
    assert_eq!(reversion.direction, Direction::Short);
}

#[test]
fn a_fixed_window_is_deterministic() {
    let closes = v_closes();
    let bars = bars(&closes);
    let data = data(&bars);
    let signal = KernelSignal::new(
        vec![SeriesRef::indicator(
            "sma",
            length_params(3),
            PriceSource::Close,
        )],
        sign_rule(),
    )
    .expect("known id");
    let first = signal.evaluate(&view(&data, &closes)).expect("scores");
    let second = signal.evaluate(&view(&data, &closes)).expect("scores");
    assert_eq!(first, second);
}

#[test]
fn a_bound_instance_matches_the_stateless_reference() {
    let closes = v_closes();
    let bars = bars(&closes);
    let signal = KernelSignal::new(
        vec![
            SeriesRef::indicator("sma", length_params(3), PriceSource::Close),
            SeriesRef::indicator("sma", length_params(8), PriceSource::Close),
        ],
        DirectionRule::CrossesLine {
            fast: 0,
            slow: 1,
            direction: CrossDir::Either,
            polarity: Polarity::Momentum,
        },
    )
    .expect("sma is registered");
    let mut instance = signal
        .bind(&crate::SignalConfig::new(protopyne_types::Timeframe::M1))
        .expect("binds");
    assert_eq!(instance.warmup(), 8, "the slow SMA owns the bound");
    for end in 1..=bars.len() {
        let window = &bars[..end];
        let data = data(window);
        let view = view(&data, &closes[..end]);
        assert_eq!(
            instance.evaluate(&view).ok(),
            signal.evaluate(&view).ok(),
            "bound instance diverged at prefix {end}"
        );
    }
}
