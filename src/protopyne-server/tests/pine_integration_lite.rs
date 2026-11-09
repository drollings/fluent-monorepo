//! Headless end-to-end proof for the shipped `_IntegrationLite` indicator.
//!
//! The seven libraries plus the indicator link through `resolve_imports`
//! and run bar-by-bar through `PineEngine :: run_linked` over the real
//! histories, with every multi-timeframe number entering through explicit
//! injection — never a live fetch. Two passes compose the proof:
//!
//! * Closed-bar indicator runs over `SPY_1M.csv` (primary), `SPY_5M.csv`,
//!   and `SPY_15M.csv` (alternate chart timeframes). Each run pins the
//!   evaluated-bar count, the ten indicator snapshot series, the drawing
//!   captures, and rerun byte-identity.
//! * A trimmed-window tick run feeds the 20-row sparse window (one steady
//!   minute plus one pulled minute, verbatim tape slice) into `Bars ::
//!   fetch` (a Bars-only driver over the same closure) and reconciles the
//!   reconstructed minute volumes against `SPY_1M.csv`. The full tape stays
//!   an explicit run: it costs minutes per pass through the interpreted
//!   closure in debug.
//!
//! Headless injection contract (what `request.*` would do live vs what the
//! harness injects):
//!
//! * `request.security(sym, tf, exprs, gaps_off, lookahead_off,
//!   calc_bars_count)` is served from per-timeframe bar datasets
//!   registered under `(symbol, abbrev)`. Only confirmed bars count: rows
//!   with timestamps after the current chart bar are invisible
//!   (`lookahead_off`), and the last confirmed value carries forward
//!   (`gaps_off`). Any other merge mode, or an unregistered pair, refuses
//!   with a reason — the live-fetch refusal stays for everything the
//!   harness does not inject.
//! * `request.security_lower_tf` is served from the registered minute
//!   window (static, per the tick-engine design: the harness registers
//!   exactly the window a pull may observe).
//! * `M5`/`M15` contexts inject from their CSVs; `H1`/`H4` have no CSV, so
//!   the harness resamples the 15-minute bars into hour (4x15m) and
//!   four-hour (16x15m) floor buckets with first-open / high / low /
//!   last-close / summed-volume aggregation. The resampling is
//!   deterministic and documented here, not hidden in a fixture.
//!
//! Closed-bar assumption (from the platform vision): every evaluated bar is
//! a confirmed history bar, so `barstate.isnew/isconfirmed/ishistory` hold
//! on each bar and the confirmation-gated calculations run per bar.
//!
//! Tick-harness contract (the trimmed-window run below, and the trimmed windows
//! in the kernels `libbars` suite — same rules, smaller fixtures):
//!
//! * Legs: per tape row, bar-so-far OHLC with the rounded cumulative
//!   volume over a constant bar-open time; tick stamps ride the side
//!   channel (`run_linked_ticks`). Volumes are positive, so cumulative
//!   legs never decrease and the rollback path stays quiet on real tape.
//! * Flags: a bar-open change clears (`isnew`); the run's final tick
//!   confirms, so `onConfirm` always runs once per window with `ctf_vol`
//!   equal to the last leg. Seeding absorbs the first two ticks without
//!   accumulating, hence drift is exactly the seeding loss over the tape
//!   total and the accumulator telescopes to last-minus-second leg.
//! * Pulls: drift over the 4% gate rebuilds the ring oldest-first from
//!   the confirmed injection prefix (capacity wins) plus the remainder
//!   only when the ring has room; the remainder adds by magnitude (the
//!   abs-remainder rule), so pulled totals are window sums on deep
//!   windows. Unregistered pairs refuse like live fetch.
//! * Recorded baselines: trimmed windows plus the soak bound live in the
//!   kernels `tests/data/tick_harness_baselines.json` with their bounds;
//!   the full contract (per-minute recompute, both regimes non-empty) is
//!   asserted below, aggregates only in the soak.
//!
//! Goldens: `tests/data/pine_integration_lite_<tf>.json` holds the full
//! canonical dump of one recorded run (snapshots of the ten indicator
//! series plus every capture). Set `PROTOPYNE_RECORD=1` to regenerate
//! after an intentional change; otherwise the files compare within
//! `1e-9`, and any drift fails loudly instead of silently moving.

use std::collections::HashMap;

mod csv {
    pub use protopyne_server::csv::load_bars_csv;
}
use std::path::PathBuf;

use protopyne_kernels::{
    parse_program, InputSet, LinkedModule, ModuleInput, PineEngine, PineRuntimeConfig, PineValue,
    RunOutcome,
};
use protopyne_types::{Bar, Timeframe};
use rust_decimal::Decimal;

#[path = "common/golden.rs"]
mod common_golden;
use common_golden::{check_golden, dump_canonical};
#[path = "common/mtf.rs"]
mod common_mtf;
use common_mtf::{
    bars_at, build_tick_legs, dataset_of, engine_for, field_num, fixture_closure_inputs,
    inject_all, link_fixture_closure, linked_from_graph, load_history, load_tick_rows_from, num,
    present, resolve_inputs, WARMUP,
};

/// Snapshot series the proof pins on confirmed bars.
const PINNED_SERIES: &[&str] = &[
    "_eit_adaptive",
    "_ribbon_signal",
    "_ribbon_mean",
    "ribbon_health",
    "_osc1_fast",
    "_osc1_slow",
    "alpha",
    "color_candle_trend",
    "dvc_buy_top",
    "dvc_sell_bot",
];

fn golden_path(timeframe: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(format!("pine_integration_lite_{timeframe}.json"))
}

/// The fixture eight-module closure (seven libraries plus the indicator
/// root) linked dependency-first, exactly like the loader hands the engine.
/// Fixture bytes are checked in; the volatile live root is never read.
fn linked_closure() -> Vec<LinkedModule> {
    link_fixture_closure("_IntegrationLite")
}

/// Link the seven fixture libraries with one driver root calling
/// through the same aliases the indicator uses.
fn link_driver(driver: &str) -> Vec<LinkedModule> {
    let mut inputs: Vec<ModuleInput> = fixture_closure_inputs("_IntegrationLite")
        .into_iter()
        .filter(|(key, _)| key != "local/_IntegrationLite")
        .map(|(key, source)| {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("fixture file {key} parses: {err}"));
            let (publisher, name) = key.split_once('/').expect("key shape");
            let id = protopyne_kernels::LibraryId {
                publisher: publisher.to_owned(),
                name: name.to_owned(),
                version: 1,
            };
            ModuleInput::new(id, source, program)
        })
        .collect();
    let source = protopyne_kernels::parse_pine_source("local/Driver", driver);
    let program = parse_program(driver).expect("driver parses");
    inputs.push(ModuleInput::new(
        protopyne_kernels::LibraryId {
            publisher: "local".to_owned(),
            name: "Driver".to_owned(),
            version: 1,
        },
        source,
        program,
    ));
    let graph = resolve_inputs(inputs);
    linked_from_graph(&graph)
}

fn run_chart(modules: &[LinkedModule], period: &str, bars: &[Bar]) -> RunOutcome {
    let minutes = load_history("SPY_1M.csv");
    let m5 = load_history("SPY_5M.csv");
    let m15 = load_history("SPY_15M.csv");
    let mut engine = engine_for(period);
    inject_all(&mut engine, &minutes, &m5, &m15);
    engine
        .run_linked(modules, bars, InputSet::new())
        .expect("linked indicator run evaluates with zero refusals")
}

/// Structural proof shared by all three chart timeframes: bar count,
/// snapshot series, captures, session/pivot branches, and determinism.
fn prove_chart(modules: &[LinkedModule], period: &str, bars: &[Bar]) -> RunOutcome {
    let outcome = run_chart(modules, period, bars);
    let expect_evaluated = bars.len() - WARMUP as usize;
    assert_eq!(
        outcome.evaluated, expect_evaluated,
        "every non-warmup bar evaluates"
    );
    assert_eq!(
        outcome.snapshots.len(),
        expect_evaluated,
        "one snapshot per evaluated bar"
    );
    for name in PINNED_SERIES {
        // Bar 100: every kernel is warm (`atr(50)` needs 50 bars), so each
        // series binds a value. Bar 0 may read `na` (zero-initialized
        // fields divided before their first fetch) — genuine Pine warmth,
        // pinned in the golden, never asserted finite here.
        assert!(
            present(&outcome, 100, name),
            "snapshot carries {name} once kernels warm"
        );
        assert!(
            present(&outcome, expect_evaluated - 1, name),
            "snapshot carries {name} on the last evaluated bar"
        );
    }
    assert!(
        outcome
            .fills
            .iter()
            .any(|fill| fill.title == "Trend Ribbon"),
        "ribbon fill pair captured"
    );
    for title in ["Ribbon Health Osc", "MA Health Osc", "Osc Fast", "Osc Slow"] {
        assert!(
            outcome.plots.iter().any(|plot| plot.title == title),
            "{title} plot captured"
        );
    }
    for title in ["Delta Buy Volume", "Delta Sell Volume"] {
        assert!(
            outcome
                .plotcandles
                .iter()
                .any(|candle| candle.title == title),
            "{title} plotcandle captured"
        );
    }
    assert!(!outcome.barcolors.is_empty(), "barcolor captured");
    // The session filter runs (the `time()` call resolves, never `na`) and
    // the pivot path records labels exactly when threshold crossings occur
    // in-fixture; absence of crossings asserts explicitly, never vacuously.
    let labels: Vec<_> = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "label")
        .collect();
    let opt_num = |at: usize| match outcome.value_at(at, "_osc1_fast") {
        Some(Some(PineValue::Float(v))) => Some(v),
        Some(Some(PineValue::Int(v))) => Some(v as f64),
        _ => None,
    };
    let fast_crosses = (1..expect_evaluated).any(|at| {
        matches!(
            (opt_num(at - 1), opt_num(at)),
            (Some(prev), Some(now))
                if (prev >= 200.0 && now < 200.0) || (prev <= -200.0 && now > -200.0)
        )
    });
    if fast_crosses {
        assert!(
            !labels.is_empty(),
            "threshold crossings record pivot labels"
        );
    } else {
        assert!(
            labels.is_empty(),
            "no crossings in-fixture, so no pivot labels — asserted, not assumed"
        );
    }
    // Multi-timeframe contexts fetched through injection: first-bar init
    // skips the chart timeframe, and every other context carries its
    // fetched timeframe (never the chart placeholder) once the scheduler
    // gate fires. Expected members per chart timeframe. The seconds-
    // stretched schedule (engine `time` runs in whole seconds against the
    // library's millisecond intervals) decides which branch asserts: due
    // means every member fetched; not-due means the map still holds the
    // four pristine first-bar puts (map.put stores a snapshot, so the
    // first-bar fetch never lands through the map) with no table events.
    // Both outcomes assert concretely — never vacuously.
    let expect_mtf: &[&str] = match period {
        "1" => &["M5", "M15", "H1", "H4"],
        "5" => &["M15", "H1", "H4"],
        "15" => &["M5", "H1", "H4"],
        _ => panic!("chart period pins its MTF set"),
    };
    let mtf = match outcome.value_at(expect_evaluated - 1, "mtf_context") {
        Some(Some(PineValue::Map(entries))) => entries,
        other => panic!("mtf map on the last bar, got {other:?}"),
    };
    assert_eq!(mtf.len(), expect_mtf.len(), "one context per MTF member");
    let sched = match outcome.value_at(expect_evaluated - 1, "scheduler") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("scheduler object on the last bar, got {other:?}"),
    };
    let next_due = match sched.fields.get("next_update").cloned().flatten() {
        Some(PineValue::IntMap(entries)) => {
            entries.get(&180_000).copied().expect("M3 frame scheduled")
        }
        other => panic!("scheduler holds its map, got {other:?}"),
    };
    let last_ts = bars[expect_evaluated - 1].ts.timestamp();
    let tables = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "table")
        .count();
    if next_due <= last_ts {
        for member in expect_mtf.iter().copied() {
            let wanted =
                PineValue::EnumHandle(protopyne_kernels::EnumHandle::new("Timeframes", member));
            assert!(
                mtf.values().any(|(_, value)| {
                    matches!(value, Some(PineValue::Object(instance))
                        if instance.fields.get("tf") == Some(&Some(wanted.clone())))
                }),
                "{member} context fetched"
            );
        }
    } else {
        let placeholder =
            PineValue::EnumHandle(protopyne_kernels::EnumHandle::new("Timeframes", "C"));
        for (_, value) in mtf.values() {
            match value {
                Some(PineValue::Object(instance)) => assert_eq!(
                    instance.fields.get("tf"),
                    Some(&Some(placeholder.clone())),
                    "unfired gate leaves the pristine placeholder"
                ),
                other => panic!("pristine context object, got {other:?}"),
            }
        }
    }
    // The MTF table builds only when the scheduler gate is due on the
    // last bar. The seconds-stretched schedule (engine `time` runs in
    // whole seconds against the library's millisecond intervals) rarely
    // lands on `islast`, so both outcomes assert concretely: due means
    // table events exist; not-due means the next slot still lies past the
    // last bar (proven, not assumed) with no table events.
    if next_due <= last_ts {
        assert!(tables > 0, "due gate builds the MTF table on islast");
    } else {
        assert_eq!(
            tables, 0,
            "not-due gate builds no table — asserted with the slot proof"
        );
    }
    // Determinism: an identical second run is byte-identical.
    let again = run_chart(modules, period, bars);
    assert_eq!(
        dump_canonical(&again),
        dump_canonical(&outcome),
        "rerun over identical inputs is byte-identical"
    );
    outcome
}

#[test]
fn integration_lite_runs_clean_on_1m() {
    let modules = linked_closure();
    let bars = load_history("SPY_1M.csv");
    assert_eq!(bars.len(), 436, "1M fixture row count pinned");
    let outcome = prove_chart(&modules, "1", &bars);
    // Late-bar finiteness: every kernel is warm by the last bar, so a
    // stuck-`na` (a masked refusal or a dead update path) fails here.
    // Early-bar `na` is genuine Pine warmth and stays pinned in the
    // golden, never asserted finite.
    let last = outcome.evaluated - 1;
    for name in [
        "_eit_adaptive",
        "_ribbon_signal",
        "_ribbon_mean",
        "ribbon_health",
        "_osc1_fast",
        "_osc1_slow",
        "alpha",
        "dvc_buy_top",
        "dvc_sell_bot",
    ] {
        assert!(
            num(&outcome, last, name).is_finite(),
            "{name} is finite on the last bar"
        );
    }
    check_golden(&golden_path("1m"), &outcome, PINNED_SERIES);
}

#[test]
fn integration_lite_runs_clean_on_5m() {
    let modules = linked_closure();
    let bars = load_history("SPY_5M.csv");
    assert_eq!(bars.len(), 346, "5M fixture row count pinned");
    let outcome = prove_chart(&modules, "5", &bars);
    check_golden(&golden_path("5m"), &outcome, PINNED_SERIES);
}

#[test]
fn integration_lite_runs_clean_on_15m() {
    let modules = linked_closure();
    let bars = load_history("SPY_15M.csv");
    assert_eq!(bars.len(), 116, "15M fixture row count pinned");
    let outcome = prove_chart(&modules, "15", &bars);
    check_golden(&golden_path("15m"), &outcome, PINNED_SERIES);
}
/// Trimmed tick window: verbatim copy of the kernels `window_sparse.csv`
/// slice (one steady minute plus one pulled minute, 20 rows). The full
/// tape costs minutes per run through the interpreted closure in debug,
/// so the reconcile contract is proved here and the full sweep stays an
/// explicit run.
fn tick_window_rows() -> Vec<(i64, f64, f64)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("ticks")
        .join("window_sparse.csv");
    let rows = load_tick_rows_from(&path);
    assert_eq!(rows.len(), 20, "sparse window row count pinned");
    rows
}

/// Trimmed-window tick proof: the sparse window's rows feed `Bars ::
/// fetch` through the linked closure (indicator root included, so its MTF
/// market contexts fetch through the same injected datasets as the chart
/// runs), and each covered minute matches the library's own confirm
/// contract recomputed from the tape legs: the first-tick seeding loss
/// sets the drift the pull gate sees, quiet minutes reconcile to the leg
/// volume, and pulled minutes rebuild the ring from the confirmed
/// injection window (oldest rows first, ring capacity wins) plus the
/// remainder.
/// Out-of-order rows ride the real rollback path; fractional share volumes
/// accumulate exactly in the harness with rounded cumulatives on the legs.
///
/// Float gates below compare exactly (no epsilon): the recompute must
/// decide bit-identically with the engine, and any tolerance would fork
/// the pull/remainder decisions it pins.
#[allow(clippy::float_cmp)]
#[ignore = "soak: full-tape tick run costs minutes in debug (interpreted closure per tick); the trimmed-window proof runs explicitly"]
#[test]
fn integration_lite_tick_tape_reconciles_minute_volumes() {
    let rows = tick_window_rows();
    // Minute buckets with running bar-so-far legs (shared geometry): the
    // leg tape per bucket drives the confirm-contract recompute below.
    let (ticks, stamps, buckets) = build_tick_legs(&rows);
    let minutes = load_history("SPY_1M.csv");
    let m5 = load_history("SPY_5M.csv");
    let m15 = load_history("SPY_15M.csv");
    // The driver closure includes the indicator root, whose first-tick
    // setup and scheduler-gated updates fetch the MTF market contexts —
    // the same per-timeframe datasets the chart runs inject.
    let fresh_engine = || {
        let mut engine = PineEngine::new(
            PineRuntimeConfig::new()
                .chart_symbol("SPY".to_owned())
                .chart_period("1".to_owned())
                .max_bars(200_000)
                .build(),
        );
        inject_all(&mut engine, &minutes, &m5, &m15);
        engine
    };
    const DRIVER: &str = "import showmethegrail/libUtil/12 as UTIL\n\
        import showmethegrail/libBars/12 as BARS\n\
        var bars = BARS.Bars.new()\n\
        if barstate.isfirst\n    \
        bars.init(UTIL.Timeframes.M1, UTIL.Timeframes.M1, UTIL.Timeframes.D1)\n\
        bars.fetch()\n";
    let tick_modules: Vec<LinkedModule> = linked_from_graph(&resolve_inputs(
        fixture_closure_inputs("_IntegrationLite")
            .into_iter()
            .map(|(key, source)| {
                let program = parse_program(&source.text).expect("fixture file parses");
                let (publisher, name) = key.split_once('/').expect("key shape");
                let id = protopyne_kernels::LibraryId {
                    publisher: publisher.to_owned(),
                    name: name.to_owned(),
                    version: 1,
                };
                ModuleInput::new(id, source, program)
            })
            .chain(std::iter::once({
                let source = protopyne_kernels::parse_pine_source("local/Driver", DRIVER);
                let program = parse_program(DRIVER).expect("driver parses");
                ModuleInput::new(
                    protopyne_kernels::LibraryId {
                        publisher: "local".to_owned(),
                        name: "Driver".to_owned(),
                        version: 1,
                    },
                    source,
                    program,
                )
            }))
            .collect(),
    ));
    let mut engine = fresh_engine();
    let outcome = engine
        .run_linked_ticks(&tick_modules, &ticks, &stamps, InputSet::new())
        .expect("tick run evaluates with zero refusals");
    assert_eq!(
        outcome.evaluated,
        ticks.len(),
        "every tape row evaluates exactly once"
    );
    // The library's confirm contract, recomputed per minute from the tape
    // legs (no engine internals). `fetch` clears on the first tick and
    // seeds tracking on the second without accumulating, so the confirmed
    // accumulator is the last leg minus the second-tick leg; legs are
    // non-decreasing cumulatives (no negative tape volumes), hence no
    // rollback path and exact telescoping. Constants mirror the library
    // source: pull gate `drift_threshold` (percent), remainder/volume
    // gate `VOLUME_EPSILON`, pull window `calc_bars_count = 300`, ring
    // capacity read from the object itself.
    const MEANINGFUL: f64 = 0.0001;
    const PULL_WINDOW: usize = 300;
    let minute_data = dataset_of(&minutes, Timeframe::M1);
    let reference: HashMap<i64, &Bar> = minutes
        .iter()
        .map(|bar| (bar.ts.timestamp_millis(), bar))
        .collect();
    let mut covered = 0usize;
    let mut pulled = 0usize;
    let mut steady = 0usize;
    let mut prev_pulls = 0.0f64;
    for bucket in &buckets {
        let open_ms = bucket.open_ms;
        let legs = &bucket.legs;
        assert!(
            reference.contains_key(&open_ms),
            "every tape minute has a reference bar, missing {open_ms}"
        );
        let state = bars_at(&outcome, bucket.last_at);
        let total = field_num(&state, "total_vol");
        let count = field_num(&state, "count");
        let pulls_now = field_num(&state, "pull_count");
        let threshold = field_num(&state, "drift_threshold");
        let capacity = field_num(&state, "max_candles") as usize;
        let leg_last = *legs.last().expect("nonempty bucket");
        let leg_first = if legs.len() >= 2 { legs[1] } else { leg_last };
        let seeded = leg_last - leg_first;
        let finalized = if seeded > MEANINGFUL { seeded } else { 0.0 };
        let drift = if leg_last > 0.0 {
            (finalized - leg_last).abs() / leg_last * 100.0
        } else {
            0.0
        };
        let expect_pull = drift > threshold;
        assert_eq!(
            pulls_now - prev_pulls,
            f64::from(u8::from(expect_pull)),
            "pull gate fires exactly on seeding drift at {open_ms}"
        );
        prev_pulls = pulls_now;
        let close_enough = |got: f64, want: f64| (got - want).abs() <= 1e-9 * want.abs().max(1.0);
        if expect_pull {
            // Pulled minute: the ring rebuilds from the confirmed prefix
            // (rows whose bucket closed at or before the pull tick, last
            // `PULL_WINDOW` of them) keeping the oldest rows up to capacity,
            // then the remainder tops up only when the ring has room. The
            // visibility predicate is the engine's single
            // `confirmed_prefix` rule (closes_at, never times): a row whose
            // bucket is still forming at the pull tick is not served.
            let chart_ms = stamps[bucket.last_at];
            let visible = minute_data
                .closes_at
                .iter()
                .take_while(|close| close.is_some_and(|c| c.get() <= chart_ms))
                .count();
            let served = visible.min(PULL_WINDOW);
            let kept = served.min(capacity);
            let skip = visible - served;
            let window: f64 = minute_data.volumes[skip..skip + kept].iter().sum();
            let remainder = leg_last - window;
            let fires = remainder.abs() > MEANINGFUL && kept < capacity;
            let expected = window + if fires { remainder.abs() } else { 0.0 };
            assert!(
                close_enough(total, expected),
                "pulled minute {open_ms} rebuilds the confirmed window, got {total}, want {expected}"
            );
            assert_eq!(
                count,
                (kept + usize::from(fires)) as f64,
                "pulled minute {open_ms} holds window plus possible remainder candles"
            );
            pulled += 1;
        } else {
            // Quiet minute: the remainder tops the seeded gap to the leg.
            let remainder = leg_last - finalized;
            let expected = finalized
                + if remainder.abs() > MEANINGFUL {
                    remainder
                } else {
                    0.0
                };
            let expected_count = f64::from(u8::from(finalized > MEANINGFUL))
                + f64::from(u8::from(remainder.abs() > MEANINGFUL));
            assert!(
                close_enough(total, expected),
                "steady minute {open_ms} reconstructs its leg volume, got {total}, want {expected}"
            );
            assert_eq!(
                count, expected_count,
                "steady minute {open_ms} holds finalize plus remainder candles"
            );
            steady += 1;
        }
        covered += 1;
    }
    assert!(covered > 0, "tick tape covers reference minutes");
    assert!(steady > 0, "quiet minutes exercise exact reconstruction");
    assert!(pulled > 0, "drifted minutes exercise the pull contract");
    // Determinism: an identical short-window rerun is byte-identical (the
    // full tape already takes minutes; the window proves the path, the
    // full run proves the coverage).
    let window = 3000usize.min(ticks.len());
    let rerun = |engine: &mut PineEngine| {
        engine
            .run_linked_ticks(
                &tick_modules,
                &ticks[..window],
                &stamps[..window],
                InputSet::new(),
            )
            .expect("window reruns")
    };
    let first = rerun(&mut fresh_engine());
    let second = rerun(&mut fresh_engine());
    assert_eq!(
        dump_canonical(&first),
        dump_canonical(&second),
        "tick rerun over identical inputs is byte-identical"
    );
}

/// Sixty rising bars with whole-second stamps: chart legs and injection
/// datasets share the rows, so every confirmed prefix is exact.
fn ramp_bars(n: u32) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let close = 100.0 + 5.0 * f64::from(i);
            let decimal = |v: f64| format!("{v:.2}").parse::<Decimal>().expect("decimal");
            Bar::new()
                .ts(chrono::TimeZone::timestamp_opt(
                    &chrono::Utc,
                    1_700_000_000 + i64::from(i) * 60,
                    0,
                )
                .single()
                .expect("valid test timestamp"))
                .o(decimal(close - 0.25))
                .h(decimal(close + 0.5))
                .l(decimal(close - 0.5))
                .c(decimal(close))
                .volume(1_000 + u64::from(i) * 10)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn ctx_field(outcome: &RunOutcome, at_bar: usize, field: &str) -> Option<PineValue> {
    match outcome.value_at(at_bar, "ctx") {
        Some(Some(PineValue::Object(instance))) => instance.fields.get(field).cloned().flatten(),
        other => panic!("MarketContext object on bar {at_bar}, got {other:?}"),
    }
}

fn field_num_at(outcome: &RunOutcome, at_bar: usize, field: &str) -> f64 {
    match ctx_field(outcome, at_bar, field) {
        Some(PineValue::Float(value)) => value,
        Some(PineValue::Int(value)) => value as f64,
        other => panic!("number field {field}, got {other:?}"),
    }
}

/// Focused `request.security` injection proof: one `MarketContext.fetch`
/// over injected MTF bars agrees with the kernel batch over the same
/// confirmed `calc_bars_count` tail within kernel tolerance. Runs
/// independently of the scheduler lottery, so the injection path stays
/// pinned even on runs whose gate never fires.
#[test]
fn security_injection_agrees_with_kernels() {
    const DRIVER: &str = "import showmethegrail/libUtil/12 as UTIL\n\
        import showmethegrail/libApex/88 as libApex\n\
        var ctx = libApex.MarketContext.new()\n\
        if barstate.isfirst\n    \
        ctx.init()\n\
        ctx.fetch(UTIL.Timeframes.M5)\n";
    let bars = ramp_bars(60);
    let modules = link_driver(DRIVER);
    let mut engine = engine_for("1");
    engine
        .inject_security("SPY", "5", dataset_of(&bars, Timeframe::M1))
        .expect("M5 injects");
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("injected fetch evaluates");
    let last = bars.len() - 1;
    // Oracle: kernel batch over the same full confirmed prefix the bridge
    // serves. The request's `calc_bars_count = 30` bounds the rows it
    // *returns*, never the history its kernels compute over, so the confirmed
    // prefix — not its last thirty rows — is the input. The injected rows are
    // minute buckets, so the bucket opening at the final chart bar is still
    // forming there and the confirmed prefix ends one row earlier.
    let confirmed = &bars[..bars.len() - 1];
    let fed = dataset_of(confirmed, Timeframe::M1);
    let ohlcv = protopyne_kernels::OwnedOhlcv {
        open: fed.opens.clone(),
        high: fed.highs.clone(),
        low: fed.lows.clone(),
        close: fed.closes.clone(),
        volume: fed.volumes.clone(),
    };
    let registry = protopyne_kernels::indicator::builtin_registry().expect("builtins");
    let batch = |kernel: &str, params: protopyne_kernels::ParamSet| {
        registry
            .get(kernel)
            .expect("kernel registered")
            .batch(&ohlcv.views(), &params)
            .expect("valid batch")
            .values
            .last()
            .copied()
            .flatten()
            .expect("kernel warm on the tail")
    };
    // Volume-sourced oracle: the bridge overrides `close` with the source
    // column, so the oracle batches over the same override.
    let vol_ohlcv = protopyne_kernels::OwnedOhlcv {
        close: fed.volumes.clone(),
        open: fed.opens.clone(),
        high: fed.highs.clone(),
        low: fed.lows.clone(),
        volume: fed.volumes.clone(),
    };
    let batch_vol = |kernel: &str, params: protopyne_kernels::ParamSet| {
        registry
            .get(kernel)
            .expect("kernel registered")
            .batch(&vol_ohlcv.views(), &params)
            .expect("valid batch")
            .values
            .last()
            .copied()
            .flatten()
            .expect("kernel warm on the tail")
    };
    let int = protopyne_kernels::ParamSet::new().with_int("length", 21);
    let close = |at: &str| field_num_at(&outcome, last, at);
    assert!(
        (close("ma_mean") - batch("ema", int)).abs() < 1e-9,
        "ema over tail"
    );
    let alma_params = protopyne_kernels::ParamSet::new()
        .with_int("length", 8)
        .with_float("offset", 0.85)
        .with_float("sigma", 6.0);
    assert!(
        (close("ma_primary") - batch("alma", alma_params)).abs() < 1e-9,
        "alma over tail"
    );
    assert!(
        (close("atr")
            - batch(
                "atr",
                protopyne_kernels::ParamSet::new().with_int("length", 14)
            ))
        .abs()
            < 1e-9,
        "atr over tail"
    );
    assert!(
        (close("vol_sma")
            - batch_vol(
                "sma",
                protopyne_kernels::ParamSet::new().with_int("length", 20)
            ))
        .abs()
            < 1e-9,
        "volume sma over tail"
    );
    match ctx_field(&outcome, last, "tf") {
        Some(PineValue::EnumHandle(handle)) => assert_eq!(
            handle.member, "M5",
            "context records the requested timeframe"
        ),
        other => panic!("timeframe field, got {other:?}"),
    }
}

/// Focused MTF-table assembly proof: `createMTFTable` over fetched
/// contexts records the table lifecycle with row-label cells through the
/// recorders, independent of the scheduler lottery.
#[test]
fn mtf_table_assembly_records_cells() {
    const DRIVER: &str = "import showmethegrail/libUtil/12 as UTIL\n\
        import showmethegrail/libApex/88 as libApex\n\
        import showmethegrail/libCandles/21 as libCandles\n\
        var ctx = libApex.MarketContext.new()\n\
        var mt = libApex.ManagedTable.new()\n\
        if barstate.isfirst\n    \
        ctx.init()\n\
        ctx.fetch()\n\
        if barstate.islast\n    \
        keys = array.from(UTIL.Timeframes.M5)\n    \
        mtf = map.new<UTIL.Timeframes, libApex.MarketContext>()\n    \
        libCandles.createMTFTable(mt, UTIL.getPosition(UTIL.Positions.MIDDLE_LEFT), keys, mtf, ctx, UTIL.getSize(UTIL.Sizes.SMALL))\n";
    let bars = ramp_bars(60);
    let modules = link_driver(DRIVER);
    let mut engine = engine_for("1");
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("table assembly evaluates");
    let tables: Vec<_> = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "table")
        .collect();
    assert!(
        tables
            .iter()
            .any(|event| format!("{:?}", event.kind) == "Created"),
        "table created once on the last bar"
    );
    let cells = tables
        .iter()
        .filter(|event| format!("{:?}", event.kind) == "Updated")
        .count();
    assert!(cells >= 20, "header and data cells recorded, got {cells}");
    assert!(
        outcome
            .draw_events
            .iter()
            .any(|event| event.object == "table"
                && event
                    .args
                    .iter()
                    .any(|arg| arg.value == Some(PineValue::Str("Regime".to_owned())))),
        "row-label cell carries its text through the recorder"
    );
}
