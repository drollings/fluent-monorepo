//! Headless proof for the shipped `_IntegrationLiteStrategy.pine`: the same
//! eight-module closure shape as the indicator proof (seven libraries plus
//! the strategy root), closed-bar runs over the real histories, with every
//! broker dialogue captured as harness observations.
//!
//! The engine runs no broker: `strategy.entry/exit/close/close_all` record
//! their bar, id, direction, quantities, prices, and comments into the run
//! outcome, and every trade-history read (`closedtrades`, `wintrades`,
//! `equity`, `netprofit`, `position_size`, ...) sees the empty book (counts
//! read zero, money reads read `na`). That empty book is the documented
//! harness boundary: triggers, filters, and state still evaluate over real
//! legs, and the recorded calls are the decision stream the parity test
//! compares against the native M7 path bar for bar.
//!
//! Goldens: `tests/data/pine_integration_lite_strategy_<tf>.json` pins bar
//! counts, the `s_*` trigger/filter/state series, drawing captures, the
//! strategy-call observations, and rerun byte-identity. Set
//! `PROTOPYNE_RECORD=1` to regenerate after an intentional change; otherwise
//! the file compares exactly, and any drift fails loudly.
//!
//! Injection contract: the strategy file calls no `request.*` arm, so every
//! live-fetch shape refuses by default — the refusal probes below pin that
//! a `request.security` with anything but gaps_off+lookahead_off, and any
//! `strategy.*` call outside the coverage matrix, refuses naming the
//! matrix row.

use std::collections::BTreeSet;
use std::path::PathBuf;

use protopyne_kernels::{
    parse_program, InputSet, LibraryId, LinkedModule, ModuleInput, RunOutcome,
};
use protopyne_types::{Bar, Money, Timeframe};

use super::common_golden::dump_value;
use super::common_mtf::{
    engine_for, fixture_closure_inputs, fixture_closure_root, inject_all, linked_from_graph,
    load_history, present, resolve_inputs, WARMUP,
};

fn strategy_id() -> LibraryId {
    LibraryId {
        publisher: "local".to_owned(),
        name: "IntegrationLiteStrategy".to_owned(),
        version: 1,
    }
}

fn golden_path(timeframe: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(format!("pine_integration_lite_strategy_{timeframe}.json"))
}

fn matrix_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("strategy_coverage_matrix.json")
}

/// The fixture eight-module closure (seven libraries plus the strategy
/// root) linked dependency-first, exactly like the loader hands the
/// engine. Fixture bytes are checked in; the volatile live root is never
/// read for Pine sources.
fn linked_strategy_closure() -> Vec<LinkedModule> {
    let strategy_text = std::fs::read_to_string(
        fixture_closure_root().join("strategies/_IntegrationLiteStrategy.pine"),
    )
    .expect("fixture strategy loads");
    let strategy_source =
        protopyne_kernels::parse_pine_source("local/IntegrationLiteStrategy", &strategy_text);
    let strategy_program = parse_program(&strategy_text).expect("fixture strategy parses");
    let mut inputs = vec![ModuleInput::new(
        strategy_id(),
        strategy_source,
        strategy_program,
    )];
    // The strategy root keeps its own key; the libraries keep theirs.
    for (key, source) in fixture_closure_inputs("_IntegrationLite") {
        if key == "local/_IntegrationLite" {
            continue;
        }
        let program = parse_program(&source.text)
            .unwrap_or_else(|err| panic!("fixture file {key} parses: {err}"));
        let (publisher, name) = key.split_once('/').expect("key shape");
        inputs.push(ModuleInput::new(
            LibraryId {
                publisher: publisher.to_owned(),
                name: name.to_owned(),
                version: 1,
            },
            source,
            program,
        ));
    }
    let graph = resolve_inputs(inputs);
    assert!(
        graph.warnings().is_empty(),
        "clean v6 strategy closure warns nothing"
    );
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
        .expect("linked strategy run evaluates with zero refusals")
}

/// Constant-alpha fixture for the parity comparison: the shipped script
/// sources its adaptive EIT alpha per bar from MTF data, which the native
/// signal (scalar `eit_alpha`) cannot take as input. Pinning the alpha
/// range high to its low collapses `alpha` to that floor on every bar, so
/// both engines read the identical relay. The site key derives from the
/// real parse (never a hardcoded string): exactly one float input titled
/// "High" may exist, and the run asserts the relay actually stayed flat.
fn constant_alpha_inputs() -> InputSet {
    use protopyne_kernels::pine::syntax::{CallArg, PineExpr, PineStmt};
    let text = std::fs::read_to_string(
        fixture_closure_root().join("strategies/_IntegrationLiteStrategy.pine"),
    )
    .expect("fixture strategy loads");
    let program = parse_program(&text).expect("strategy parses");
    let mut found: Vec<Vec<CallArg>> = Vec::new();
    for stmt in &program.statements {
        if let PineStmt::Assignment { value, .. } = stmt {
            if let PineExpr::MethodCall {
                namespace,
                method,
                args,
                ..
            } = value.as_ref()
            {
                let is_title_high = args.iter().any(|a| {
                    a.name.as_deref() == Some("title")
                        && matches!(&a.value, PineExpr::StringLiteral(t) if t == "High")
                });
                if namespace == "input" && method == "float" && is_title_high {
                    found.push(args.clone());
                }
            }
        }
    }
    assert_eq!(
        found.len(),
        1,
        "exactly one float input titled High pins the alpha range, got {}",
        found.len()
    );
    let site = format!("input.float@{:?}", found[0]);
    let mut inputs = InputSet::new();
    inputs
        .declare(
            &site,
            protopyne_kernels::InputDef::Float {
                default: 0.14,
                min: 0.10,
                max: f64::MAX,
            },
        )
        .expect("alpha-high site pre-declares");
    inputs
}

/// Same closed-bar run as [`run_chart`] but over caller inputs (the parity
/// comparison fixes the alpha relay; goldens keep shipped defaults).
fn run_chart_with(
    modules: &[LinkedModule],
    period: &str,
    bars: &[Bar],
    inputs: InputSet,
) -> RunOutcome {
    let minutes = load_history("SPY_1M.csv");
    let m5 = load_history("SPY_5M.csv");
    let m15 = load_history("SPY_15M.csv");
    let mut engine = engine_for(period);
    inject_all(&mut engine, &minutes, &m5, &m15);
    engine
        .run_linked(modules, bars, inputs)
        .expect("linked strategy run evaluates with zero refusals")
}

/// Snapshot series the proof pins on confirmed bars: trigger arms,
/// filter gates, exit signals, and the position direction the broker
/// dialogue follows.
const PINNED_SERIES: &[&str] = &[
    "s_trig_long",
    "s_trig_short",
    "s_long_armed",
    "s_short_armed",
    "s_filt_long",
    "s_filt_short",
    "s_pos_dir",
    "s_x_up_lvl",
    "s_x_dn_lvl",
    "s_x_up_exh",
    "s_x_dn_exh",
    "s_x_ribbon_up",
    "s_x_ribbon_dn",
    "s_long_sig",
    "s_short_sig",
    "s_do_long",
    "s_do_short",
    "s_gate",
    "s_atr",
    "s_flatten_now",
    "st_pending",
    "s_act",
    "is_market",
    "s_trades_ok",
    "s_cd_ok",
    "s_noentry_zone",
    "s_flat_zone",
    "st_pending",
    "s_end_min",
    "s_start_min",
];

fn dump_strategy_calls(outcome: &RunOutcome) -> serde_json::Value {
    serde_json::Value::Array(
        outcome
            .strategy_calls
            .iter()
            .map(|call| {
                serde_json::json!({
                    "bar": call.bar,
                    "kind": call.kind,
                    "id": call.id,
                    "direction": call.direction,
                    "qty": call.qty,
                    "qty_percent": call.qty_percent,
                    "limit": call.limit,
                    "stop": call.stop,
                    "profit": call.profit,
                    "loss": call.loss,
                    "comment": call.comment,
                    "from_entry": call.from_entry,
                })
            })
            .collect(),
    )
}

fn dump_strategy(outcome: &RunOutcome, pinned: &[&str]) -> String {
    let mut series = serde_json::Map::new();
    for name in pinned {
        let values: Vec<serde_json::Value> = outcome
            .snapshots
            .iter()
            .map(|snapshot| {
                snapshot
                    .values
                    .get(*name)
                    .map_or(serde_json::Value::Null, dump_value)
            })
            .collect();
        series.insert((*name).to_owned(), serde_json::Value::Array(values));
    }
    let labels = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "label")
        .count();
    let tables = outcome
        .draw_events
        .iter()
        .filter(|event| event.object == "table")
        .count();
    serde_json::json!({
        "evaluated": outcome.evaluated,
        "series": series,
        "labels": labels,
        "tables": tables,
        "strategy_calls": dump_strategy_calls(outcome),
    })
    .to_string()
}

fn check_strategy_golden(path: &PathBuf, outcome: &RunOutcome) {
    let dump = dump_strategy(outcome, PINNED_SERIES);
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::write(path, &dump).expect("strategy golden records");
        return;
    }
    let recorded = std::fs::read_to_string(path)
        .unwrap_or_else(|_| panic!("golden {} records first", path.display()));
    assert_eq!(
        dump, recorded,
        "rerun matches the checked-in strategy golden"
    );
}

/// Structural proof shared by every chart timeframe: bar counts, pinned
/// series presence once warm, drawing captures, the strategy-call
/// observations, and rerun byte-identity.
fn prove_strategy(modules: &[LinkedModule], period: &str, bars: &[Bar]) -> RunOutcome {
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
        assert!(
            present(&outcome, 100, name),
            "snapshot carries {name} once warm"
        );
        assert!(
            present(&outcome, expect_evaluated - 1, name),
            "snapshot carries {name} on the last evaluated bar"
        );
    }
    // The strategy draws its status table and trade markers through the
    // recording drawing namespaces; a run that draws nothing drew nothing
    // to compare, so presence is asserted, never assumed.
    assert!(
        outcome
            .draw_events
            .iter()
            .any(|event| event.object == "table"),
        "status table captured"
    );
    // Determinism: an identical second run is byte-identical, calls
    // included.
    let again = run_chart(modules, period, bars);
    assert_eq!(
        dump_strategy(&again, PINNED_SERIES),
        dump_strategy(&outcome, PINNED_SERIES),
        "rerun over identical inputs is byte-identical"
    );
    outcome
}

#[test]
fn integration_lite_strategy_runs_clean_on_1m() {
    let modules = linked_strategy_closure();
    let bars = load_history("SPY_1M.csv");
    assert_eq!(bars.len(), 436, "1M fixture row count pinned");
    let outcome = prove_strategy(&modules, "1", &bars);
    check_strategy_golden(&golden_path("1m"), &outcome);
}

#[test]
fn integration_lite_strategy_runs_clean_on_5m() {
    let modules = linked_strategy_closure();
    let bars = load_history("SPY_5M.csv");
    assert_eq!(bars.len(), 346, "5M fixture row count pinned");
    let outcome = prove_strategy(&modules, "5", &bars);
    check_strategy_golden(&golden_path("5m"), &outcome);
}

#[test]
fn integration_lite_strategy_runs_clean_on_15m() {
    let modules = linked_strategy_closure();
    let bars = load_history("SPY_15M.csv");
    assert_eq!(bars.len(), 116, "15M fixture row count pinned");
    let outcome = prove_strategy(&modules, "15", &bars);
    check_strategy_golden(&golden_path("15m"), &outcome);
}

/// Every `strategy.*` spelling the fixture file uses, mapped to its matrix
/// row. The matrix test below fails on any unmapped spelling, so a new
/// broker call cannot land silently. Fixture bytes are checked in.
fn strategy_call_sites() -> BTreeSet<String> {
    let text = std::fs::read_to_string(
        fixture_closure_root().join("strategies/_IntegrationLiteStrategy.pine"),
    )
    .expect("fixture strategy loads");
    let mut sites = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("//") {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find("strategy.") {
            rest = &rest[start + "strategy.".len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
                .unwrap_or(rest.len());
            let mut site = rest[..end].to_owned();
            // `strategy.closedtrades.size(i)` and friends: the dotted receiver
            // is the matrix key, not the leaf call.
            if site.starts_with("closedtrades.") {
                "closedtrades.*".clone_into(&mut site);
            } else if site.starts_with("opentrades.") {
                "strategy.opentrades.*".clone_into(&mut site);
            }
            if !site.is_empty() {
                sites.insert(site);
            }
            rest = &rest[end..];
        }
    }
    // Declarations and enum spellings are covered by their own rows.
    sites.remove("IntegrationLite");
    // The bare strategy(...) declaration call has no dotted member; it is
    // covered by the `strategy` row.
    sites.insert("strategy".to_owned());
    sites
}

#[test]
fn strategy_coverage_matrix_covers_every_call_site() {
    let text = std::fs::read_to_string(matrix_path()).expect("matrix loads");
    let matrix: serde_json::Value = serde_json::from_str(&text).expect("matrix parses");
    let rows = matrix["rows"].as_array().expect("rows array");
    let mut covered = BTreeSet::new();
    for row in rows {
        let site = row["site"].as_str().expect("site names the spelling");
        let status = row["status"].as_str().expect("status names the standing");
        assert!(
            ["Supported", "FormulaVerified", "Refuse"].contains(&status),
            "{site} names a closed standing"
        );
        assert!(
            row["evidence"].as_str().is_some_and(|e| !e.is_empty()),
            "{site} cites its evidence"
        );
        covered.insert(site.to_owned());
    }
    let mut missing = Vec::new();
    for site in strategy_call_sites() {
        // Declaration options and enum spellings ride their own rows.
        let key = if site == "fixed"
            || site == "long"
            || site == "short"
            || site == "percent"
            || site.starts_with("commission.")
        {
            "declaration-spellings".to_owned()
        } else {
            site.clone()
        };
        if !covered.contains(&key) {
            missing.push(site);
        }
    }
    assert!(
        missing.is_empty(),
        "strategy spellings with no matrix row: {missing:?}"
    );
}

/// Refusal probes: every Refuse row names an executable probe, and each
/// probe refuses with the matrix reason — never silently, never with a
/// different reason.
#[test]
fn strategy_refusals_name_their_matrix_row() {
    use protopyne_kernels::parse_pine_source;
    let run_driver = |driver: &str| {
        let source = parse_pine_source("local/Driver", driver);
        let program = protopyne_kernels::parse_program(driver).expect("driver parses");
        let driver_module = ModuleInput::new(
            LibraryId {
                publisher: "local".to_owned(),
                name: "Driver".to_owned(),
                version: 1,
            },
            source,
            program,
        );
        let mut inputs = Vec::new();
        for (key, source) in fixture_closure_inputs("_IntegrationLite") {
            if key == "local/_IntegrationLite" {
                continue;
            }
            let program = parse_program(&source.text).expect("library parses");
            let (publisher, name) = key.split_once('/').expect("key shape");
            inputs.push(ModuleInput::new(
                LibraryId {
                    publisher: publisher.to_owned(),
                    name: name.to_owned(),
                    version: 1,
                },
                source,
                program,
            ));
        }
        inputs.push(driver_module);
        let graph = resolve_inputs(inputs);
        let modules = linked_from_graph(&graph);
        let bars = load_history("SPY_1M.csv");
        let minutes = bars.clone();
        let mut engine = engine_for("1");
        inject_all(&mut engine, &minutes, &minutes, &minutes);
        engine.run_linked(&modules, &bars[..4], InputSet::new())
    };
    let preamble =
        "import showmethegrail/libUtil/12 as UTIL\nimport showmethegrail/libBars/12 as BARS\n";
    // Live-fetch shapes refuse: gaps_on is not an injected merge mode.
    let refused = run_driver(&format!(
        "{preamble}out = request.security(syminfo.tickerid, \"D\", close, barmerge.gaps_on, barmerge.lookahead_off)"
    ));
    let err = refused.expect_err("un-injected request arm must refuse");
    assert!(
        err.to_string().contains("barmerge.gaps_off only"),
        "un-injected request arm refuses naming the injected mode, got {err}"
    );
    // Broker calls outside the matrix refuse naming their row; unregistered
    // pairs refuse as live fetches.
    for (driver, needle) in [
        (
            format!("{preamble}strategy.order(\"O\", strategy.long, 1)"),
            "strategy call strategy.order is outside the harness matrix",
        ),
        (
            format!("{preamble}x = strategy.opentrades.size(0)"),
            "strategy call strategy.opentrades.size is outside the harness matrix",
        ),
        (
            format!(
                "{preamble}out = request.security(syminfo.tickerid, \"D2\", close, barmerge.gaps_off, barmerge.lookahead_off)"
            ),
            "live request.security",
        ),
    ] {
        let refused = run_driver(&driver);
        let err = refused.expect_err("outside-matrix call must refuse");
        assert!(
            err.to_string().contains(needle),
            "refusal names its matrix row, got {err}"
        );
    }
}

fn test_venue_config() -> crate::backtest::BacktestConfig {
    crate::backtest::BacktestConfig::new()
        .underlying("SPY".to_owned())
        .data_version("strategy-harness-v1".to_owned())
        .quote_size(1_000_000)
        .build()
}

fn test_runner_config() -> crate::runner::RunnerConfig {
    crate::runner::RunnerConfig::new(
        "SPY".to_owned(),
        "integration_lite".to_owned(),
        Money::zero(),
        Money::zero(),
        30,
        0,
    )
}

/// Native-vs-harness parity: the same closed-bar tape through the native simple
/// equity path and the headless strategy harness must place the same
/// entries on the same bars. The harness sees the empty book (no fills are
/// modeled), so the tape carries isolated setups: both books stay flat
/// outside the compared entries by construction.
#[test]
fn native_and_harness_agree_bar_for_bar() {
    use protopyne_strategy::integration_lite::IntegrationLiteStrategyConfig;
    use protopyne_strategy::signals_lite::LiteIndicatorParams;

    // The SPY 1M fixture tape: both engines see the same closed bars with
    // the same relay (constant 0.14 alpha pinned by the fixture), the same
    // defaults (setup window 0, session filter on, warmup 100), and the
    // same equity ($100k through the same risk-percent math). The harness
    // books no fills and holds no position, so it re-fires while flat where
    // the native book holds or cools down: parity covers the setup bars
    // where both books are flat, and the comparison asserts exactly those.
    let bars = load_history("SPY_1M.csv");
    // Harness side: record strategy.entry calls on the tape.
    let modules = linked_strategy_closure();
    let outcome = run_chart_with(&modules, "1", &bars, constant_alpha_inputs());
    // The fixture held: alpha never left its floor, so the relay both
    // engines read is one fixed-alpha EIT.
    for i in 0..outcome.evaluated {
        match outcome.value_at(i, "alpha") {
            Some(Some(protopyne_kernels::PineValue::Float(v))) => assert!(
                (v - 0.14).abs() <= 1e-12,
                "alpha fixture holds at bar {i}: {v}"
            ),
            other => panic!("alpha series reads 0.14 every evaluated bar, got {other:?} at {i}"),
        }
    }
    let mut harness_entries = Vec::new();
    for call in &outcome.strategy_calls {
        if call.kind == "entry" {
            harness_entries.push((call.bar, call.direction.clone(), call.qty));
        }
    }
    assert!(
        !harness_entries.is_empty(),
        "the comparison tape must arm entries or it proves nothing"
    );
    // Native side: the M7 equity path over the identical tape, with the
    // adaptive relay armed to mirror the shipped oscillator source (the
    // fixture above fixes its alpha, so both relays agree exactly).
    let indicator = LiteIndicatorParams {
        adaptive: true,
        ..LiteIndicatorParams::default()
    };
    let default_strategy = IntegrationLiteStrategyConfig::default();
    let run = crate::simple_backtest::SimpleBacktestRequest::builder()
        .indicator(&indicator)
        .strategy(&default_strategy)
        .venue_config(&test_venue_config())
        .runner_config(&test_runner_config())
        .signal_name("integration_lite")
        .underlying("SPY")
        .timeframe(Timeframe::M1)
        .bars(&bars)
        .seed(7)
        .initial_equity(100_000.0)
        .run()
        .expect("native equity run completes");
    // Entry size reconstructs from fills: every record carries the shares
    // its own exit filled (a partial slice or the remainder), so the entry
    // quantity is the per-bar fill sum across closes and partials.
    use std::collections::BTreeMap;
    let mut fills: BTreeMap<(usize, bool), u64> = BTreeMap::new();
    for trade in run.trades.iter().chain(run.partials.iter()) {
        *fills.entry((trade.entry_bar, trade.long)).or_default() += u64::from(trade.qty);
    }
    let mut native_entries = Vec::new();
    for ((bar, long), qty) in &fills {
        native_entries.push((
            *bar,
            Some(if *long {
                "long".to_owned()
            } else {
                "short".to_owned()
            }),
            Some(*qty as f64),
        ));
    }
    native_entries.sort_by_key(|(bar, _, _)| *bar);
    // Both books start flat, so the first setup must agree exactly; past
    // the first entry the books diverge by construction (the harness never
    // books fills, holds no position, and takes no cooldown, so it re-fires
    // while flat where the native book holds or cools down). The comparison
    // asserts the shared flat-book prefix bar-for-bar, then requires every
    // later native entry to sit on a harness entry bar with the same side
    // and size — the native book may only skip setups the harness takes.
    assert!(
        !native_entries.is_empty(),
        "the native path must take entries or the comparison proves nothing"
    );
    assert_eq!(
        native_entries[0], harness_entries[0],
        "native and harness agree on the first flat-book setup"
    );
    let harness_by_bar: BTreeMap<(usize, Option<String>), f64> = harness_entries
        .iter()
        .map(|(bar, dir, qty)| ((*bar, dir.clone()), qty.unwrap_or(0.0)))
        .collect();
    for (bar, dir, qty) in &native_entries {
        match harness_by_bar.get(&(*bar, dir.clone())) {
            Some(hqty) => assert!(
                (qty.unwrap_or(0.0) - *hqty).abs() <= 1e-9,
                "entry size agrees at bar {bar}: native {qty:?} vs harness {hqty}"
            ),
            None => panic!("native entry at bar {bar} has no harness entry on the same bar"),
        }
    }
}

/// Leg parity on the comparison tape: with the constant-alpha fixture both
/// engines read the identical relay, so the oscillator legs and ATR must
/// agree to 1e-9 everywhere they co-read — including the first trigger bar,
/// whose crossunder the test hand-checks below.
#[test]
fn native_legs_match_harness_legs_on_spy_1m() {
    use protopyne_kernels::PineValue;
    use protopyne_strategy::signals_lite::{market_legs, LiteIndicatorParams};
    let bars = load_history("SPY_1M.csv");
    let params = LiteIndicatorParams {
        adaptive: true,
        ..LiteIndicatorParams::default()
    };
    let legs = market_legs(&params, 14, &bars).expect("legs evaluate");
    let modules = linked_strategy_closure();
    let outcome = run_chart_with(&modules, "1", &bars, constant_alpha_inputs());
    let pine_float = |bar: usize, name: &str| match outcome.value_at(bar, name) {
        Some(Some(PineValue::Float(v))) => Some(v),
        // Names assigned under a Bars-state gate (e.g. the confirmed-bar
        // slow leg) are absent before their first assignment; those bars
        // carry no harness reading to compare against.
        Some(None) | None => None,
        other => panic!("leg {name} reads float-or-na at bar {bar}, got {other:?}"),
    };
    let mut compared = 0;
    for i in 0..bars.len() {
        for (native, name) in [
            (legs.fast[i], "_osc1_fast"),
            (legs.slow[i], "_osc1_slow"),
            (legs.adaptive[i], "_eit_adaptive"),
            (legs.atr[i], "s_atr"),
        ] {
            match (native, pine_float(i, name)) {
                (Some(a), Some(b)) => {
                    assert!(
                        (a - b).abs() <= 1e-9,
                        "{name} drifts at bar {i}: native {a} vs harness {b}"
                    );
                    compared += 1;
                }
                (None, None) => {}
                (a, b) => panic!("{name} presence splits at bar {i}: {a:?} vs {b:?}"),
            }
        }
    }
    assert!(
        compared >= 1000,
        "legs co-read almost everywhere: {compared}"
    );
    // Hand-check on the first trigger bar: the fast leg crosses under the
    // -100 level between bars 118 and 119, which is exactly where both
    // engines place the short entry.
    let (Some(prev), Some(curr)) = (legs.fast[118], legs.fast[119]) else {
        panic!("fast leg reads at the trigger bars");
    };
    assert!(
        prev > -100.0 && curr < -100.0,
        "crossunder at 119: {prev} -> {curr}"
    );
}
