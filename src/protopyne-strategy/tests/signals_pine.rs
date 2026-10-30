//! Pine signal guards: the version, parity claim, plot title, and engine
//! refusals are all fail-closed, and the linked closure never fetches data
//! the decision view did not supply.

use super::*;
use chrono::{TimeZone, Utc};
use protopyne_kernels::manifest_files::load_pine_sources;
use protopyne_kernels::parse_pine_source;
use protopyne_kernels::{discover_closures, ClosureOutcome};
use protopyne_types::{Bar, Timeframe};
use rust_decimal::Decimal;
use std::path::PathBuf;

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
        ParityClaim::new(["ta.adx"]),
    )
    .expect_err("a refused construct refuses");
    assert!(
        err.to_string().contains("ta.adx"),
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

/// The fixture closure links and runs, but a run that asks for a timeframe
/// the view does not carry refuses instead of fetching: the decision input is
/// the only data source. Fixture bytes are checked in
/// (`tests/data/pine_closure`), never the volatile live root.
#[test]
fn the_closure_refuses_to_fetch_outside_the_view() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../protopyne-server/tests/data/pine_closure");
    let (indicators, libraries) = load_pine_sources(&root).expect("fixtures load");
    let sources = discover_closures(&indicators, &libraries)
        .into_iter()
        .find_map(|outcome| match outcome {
            ClosureOutcome::Linked(closure) if closure.stem == "_IntegrationLite" => {
                Some(closure.sources)
            }
            _ => None,
        })
        .expect("fixture closure discovered");
    let signal = PineSignal::from_sources(
        sources,
        "local/_IntegrationLite",
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

#[cfg(feature = "bytecode")]
mod compiled_selection {
    use super::*;
    use crate::signals_pine::PineExecution;

    #[test]
    fn production_signal_path_matches_tree_when_compiled() {
        // The default (compiled) selection must produce the identical
        // decision series as the tree-walk oracle: both selections report
        // byte-identical outcomes on lowered programs.
        let closes: Vec<f64> = (0..40).map(|i| 100.0 + f64::from(i).sin() * 2.0).collect();
        let bars = bars(&closes);
        let data = data(&bars);
        let view = view(&data, &closes);
        let tree = signal(SCRIPT).with_execution(PineExecution::TreeWalk);
        let compiled = signal(SCRIPT);
        assert_eq!(compiled.execution(), PineExecution::Compiled);
        let a = tree.evaluate(&view).expect("tree evaluates");
        let b = compiled.evaluate(&view).expect("compiled evaluates");
        assert_eq!(a.direction, b.direction, "directions agree");
        assert_eq!(
            a.conviction.to_bits(),
            b.conviction.to_bits(),
            "conviction agrees bitwise"
        );
    }

    #[test]
    fn production_path_evaluates_dead_branches_identically() {
        // A script with an unreached refusal still evaluates: the dead
        // branch stays silent on the loop exactly as on the tree, so the
        // decision is unchanged instead of refused.
        let ok_script = "//@version=6\nindicator(\"Fixture\", overlay=false)\nif close < 0.0\n    probe = runtime.error(\"dead\")\nplot(close - close[1], \"Delta\")\n";
        let closes: Vec<f64> = (0..20).map(|i| 100.0 + f64::from(i)).collect();
        let bars = bars(&closes);
        let data = data(&bars);
        let view = view(&data, &closes);
        let tree = signal(ok_script).with_execution(PineExecution::TreeWalk);
        let compiled = signal(ok_script);
        let a = tree.evaluate(&view).expect("tree evaluates");
        let b = compiled
            .evaluate(&view)
            .expect("compiled selection evaluates");
        assert_eq!(a.direction, b.direction);
        assert_eq!(a.conviction.to_bits(), b.conviction.to_bits());
    }

    #[test]
    fn fresh_signals_run_compiled_by_default() {
        // The shipping default is the compiled loop, and the setting is
        // fixed at construction.
        let signal = signal(SCRIPT);
        assert_eq!(signal.execution(), PineExecution::Compiled);
        assert_eq!(PineExecution::default(), PineExecution::Compiled);
    }

    #[test]
    fn fresh_signals_need_no_opt_in() {
        // No production constructor selects the tree-walk: every Pine
        // signal built without an explicit opt-out evaluates on the
        // compiled loop, and only the builder can change that.
        for script in [
            SCRIPT,
            "//@version=6\nindicator(\"Fixture\")\nplot(close, \"Delta\")\n",
        ] {
            let signal = signal(script);
            assert_eq!(
                signal.execution(),
                PineExecution::Compiled,
                "construction needs no opt-in: {script}"
            );
        }
    }
}

#[cfg(not(feature = "bytecode"))]
#[test]
fn feature_off_build_keeps_tree_walk() {
    // Without the feature the compiled selection does not exist at all, so
    // no production path can name it; the fixture still evaluates on the
    // tree-walk exactly as before.
    let closes: Vec<f64> = (0..10).map(|i| 100.0 + i as f64).collect();
    let bars = bars(&closes);
    let data = data(&bars);
    let view = view(&data, &closes);
    let outcome = signal(SCRIPT).evaluate(&view).expect("tree evaluates");
    assert!(
        matches!(outcome.direction, Direction::Long | Direction::Short),
        "the tree-walk still decides: {outcome:?}"
    );
}
