//! Pine signal guards: the version, parity claim, plot title, and engine
//! refusals are all fail-closed, and the linked closure never fetches data
//! the decision view did not supply.

use super::*;
use chrono::{TimeZone, Utc};
use protopyne_kernels::{closure_data_root, load_closure_sources, parse_pine_source};
use protopyne_types::{Bar, Timeframe};
use rust_decimal::Decimal;

use crate::elements::{Signal, SignalConfig};
use crate::proposal::Direction;
use crate::signals_kernel::Polarity;
use crate::view::{MarketData, MarketView};

const SCRIPT: &str =
    "//@version=6\nindicator(\"Fixture\", overlay=false)\nplot(close - close[1], \"Delta\")\n";

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
        timeframe: Timeframe::M1,
    }
}

fn delta_rule() -> DirectionRule {
    DirectionRule::Sign {
        line: 0,
        scale: 2.0,
        polarity: Polarity::Momentum,
    }
}

fn signal(script: &str) -> PineSignal {
    PineSignal::new(
        parse_pine_source("local/Signal", script),
        "Delta",
        delta_rule(),
        ParityClaim::none(),
    )
    .expect("the fixture links")
}

#[test]
fn a_script_without_the_supported_version_refuses() {
    let source = parse_pine_source("local/Signal", "//@version=5\nplot(close, \"Delta\")\n");
    let err = PineSignal::new(source, "Delta", delta_rule(), ParityClaim::none())
        .expect_err("an older surface refuses");
    assert!(
        err.to_string().contains("version 5"),
        "the refusal names the declared version: {err}"
    );
}

#[test]
fn a_refuse_parity_row_refuses_at_construction() {
    let err = PineSignal::new(
        parse_pine_source("local/Signal", SCRIPT),
        "Delta",
        delta_rule(),
        ParityClaim::new(["ta.dmi"]),
    )
    .expect_err("a refused construct refuses");
    assert!(
        err.to_string().contains("ta.dmi"),
        "the refusal names the construct: {err}"
    );
}

#[test]
fn an_unlisted_parity_construct_refuses() {
    let err = PineSignal::new(
        parse_pine_source("local/Signal", SCRIPT),
        "Delta",
        delta_rule(),
        ParityClaim::new(["not.a.construct"]),
    )
    .expect_err("the matrix cannot vouch for an unknown construct");
    assert!(err.to_string().contains("not.a.construct"), "{err}");
}

#[test]
fn a_correct_parity_row_is_accepted_as_a_declared_divergence() {
    let signal = PineSignal::new(
        parse_pine_source("local/Signal", SCRIPT),
        "Delta",
        delta_rule(),
        ParityClaim::new(["assignment (=)"]),
    )
    .expect("a declared divergence links");
    let closes = [100.0, 101.0];
    let bars = bars(&closes);
    let data = data(&bars);
    signal
        .evaluate(&view(&data, &closes))
        .expect("the fixture decides");
}

#[test]
fn an_unknown_plot_title_refuses_and_names_the_plots() {
    let signal = PineSignal::new(
        parse_pine_source("local/Signal", SCRIPT),
        "Missing",
        delta_rule(),
        ParityClaim::none(),
    )
    .expect("the fixture links");
    let closes = [100.0, 101.0, 102.0];
    let bars = bars(&closes);
    let data = data(&bars);
    let err = signal
        .evaluate(&view(&data, &closes))
        .expect_err("an unknown title refuses");
    assert!(
        err.to_string().contains("Delta"),
        "the refusal names the available plots: {err}"
    );
}

#[test]
fn a_bound_instance_matches_the_stateless_reference() {
    let closes = [100.0, 101.0, 99.0, 104.0, 103.0, 106.0, 105.0, 108.0];
    let bars = bars(&closes);
    let signal = signal(SCRIPT);
    let mut instance = signal
        .bind(&SignalConfig::new(Timeframe::M1))
        .expect("binds");
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
    let data = data(&bars);
    let last = signal
        .evaluate(&view(&data, &closes))
        .expect("the last bar decides");
    assert_eq!(last.direction, Direction::Long);
    assert!((last.conviction - 1.0).abs() < 1e-12);
}

/// The shipped closure links and runs, but a run that asks for a timeframe
/// the view does not carry refuses instead of fetching: the decision input is
/// the only data source.
#[test]
fn the_closure_refuses_to_fetch_outside_the_view() {
    let root = closure_data_root(env!("CARGO_MANIFEST_DIR"));
    let sources = load_closure_sources(&root).expect("the closure loads");
    let signal = PineSignal::from_sources(
        sources,
        "local/IntegrationLite",
        "Osc Fast",
        DirectionRule::Sign {
            line: 0,
            scale: 100.0,
            polarity: Polarity::Momentum,
        },
        ParityClaim::none(),
    )
    .expect("the closure links");
    let closes: Vec<f64> = (0..80).map(|i| 100.0 + f64::from(i) * 0.1).collect();
    let bars = bars(&closes);
    let data = data(&bars);
    let err = signal
        .evaluate(&view(&data, &closes))
        .expect_err("an unregistered security pair refuses");
    let frozen = "live request.security is refused headlessly; inject \
                  multi-timeframe bars and tick data through the run instead of a \
                  network fetch";
    assert!(
        err.to_string().contains(frozen),
        "the frozen refusal names the missing dataset: {err}"
    );
    let again = signal
        .evaluate(&view(&data, &closes))
        .expect_err("the refusal is stable");
    assert_eq!(again.to_string(), err.to_string());
}
