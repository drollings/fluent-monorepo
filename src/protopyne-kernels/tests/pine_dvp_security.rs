//! Headless `request.security` injection proof for the fetch shape the
//! Delta Volume Profile indicator path evaluates: a fetch-shaped probe
//! driver runs over the trimmed SPY histories at all three chart
//! timeframes with zero refusals, served from injected datasets through
//! the exact `libApex.fetch` call shape (confirmed bars, `gaps_off` +
//! `lookahead_off`, tail `calc_bars_count`).
//!
//! No interpreted DVP run exists in this suite by human verdict
//! (2026-10-12: prune every PineScript DVP run, keep native Rust only):
//! the probes run the fetch-shaped raw call and the production
//! `libApex.fetch` driver, which together cover the injection path the
//! indicator evaluates.
//!
//! The suite calibrates the served subset before trusting it: a look-ahead
//! probe (a future sentinel bar stays invisible), a gap probe (a deleted
//! injected row carries the last confirmed value forward), and a must-be-
//! visible same-bar control (proving the probes can distinguish future from
//! current bars) run over three chart timeframes times three injected
//! timeframes. Non-served `request.*` spellings refuse with the live-fetch
//! reason; the per-closure call-site inventory pins which spellings exist.
//!
//! Test path note: this file rides the `pine::eval` unit module (injection
//! lives in `eval.rs`), so the runnable filter is `pine::eval`, not the
//! file stem.

use crate::{
    ClosureEntry, InputSet, LinkedModule, LtfDataset, PineEngine, PineRuntimeConfig,
    PineValue, REAL_LIBRARIES, TASK_INDICATORS, closure_data_root,
    load_listed_closure, parse_program, PineExpr, PineStmt,
};
use protopyne_types::Bar;
use std::collections::{BTreeMap, BTreeSet};

use crate::pine::test_fixture as fixture;

/// Chart timeframes under proof: engine period plus the history file that
/// supplies the chart bars.
const CHARTS: &[(&str, &str)] = &[
    ("1", "historical/SPY_1M.csv"),
    ("5", "historical/SPY_5M.csv"),
    ("15", "historical/SPY_15M.csv"),
];

/// Injected security timeframes under proof: pair name, bucket width in
/// milliseconds, and which chart file the rows come from (daily buckets are
/// resampled from the fifteen-minute tape).
const INJECTED: &[(&str, i64, &str)] = &[
    ("5", 5 * 60 * 1000, "historical/SPY_5M.csv"),
    ("15", 15 * 60 * 1000, "historical/SPY_15M.csv"),
    ("D", 24 * 60 * 60 * 1000, "historical/SPY_15M.csv"),
];

/// The exact expression list `libApex.fetch` passes to `request.security`
/// (bare legs, kernel-backed single-source `ta.*` calls, scalar operators):
/// the probes run this shape so the calibration covers the production call.
const FETCH_EXPRS: &str = "[open, high, low, close, volume, ta.sma(volume, 20), ta.alma(close, 8, 0.85, 6), ta.vwma(close, 12), ta.ema(close, 21), ta.atr(14), ta.atr(50), ta.cci(close, 14) * 3, (ta.rsi(close, 14) - 50) * 2, ta.stdev(close, 14)]";

/// One probe driver: the fetch-shaped `request.security` call against one
/// injected pair, re-evaluated every bar into `probe`.
fn probe_driver(timeframe: &str) -> String {
    format!(
        "probe = request.security(\"SPY\", \"{timeframe}\", {FETCH_EXPRS}, barmerge.gaps_off, barmerge.lookahead_off, calc_bars_count=30)\n"
    )
}

/// Five shipped libraries plus one synthetic driver, linked
/// dependency-first (the same shape the `libApex` suites use).
fn linked_driver(driver: &str) -> Vec<LinkedModule> {
    fixture::linked_modules(vec![
        fixture::shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        fixture::shipped_module(
            "libraries/_libBuffers.pine",
            "showmethegrail",
            "libBuffers",
            6,
        ),
        fixture::shipped_module("libraries/_libBars.pine", "showmethegrail", "libBars", 12),
        fixture::shipped_module(
            "libraries/_libDrawing.pine",
            "showmethegrail",
            "libDrawing",
            61,
        ),
        fixture::shipped_module("libraries/_libApex.pine", "showmethegrail", "libApex", 88),
        fixture::driver_module(driver),
    ])
}

/// Engine for one chart timeframe: SPY scope with zero warmup bars, the
/// same evaluated-bar semantics the server headless proof runs under.
fn engine_for(period: &str) -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period(period.to_owned())
            .warmup_bars(0)
            .build(),
    )
}

/// One injected security dataset per timeframe under proof: minute grids
/// straight from their CSVs, daily resampled from the fifteen-minute tape.
fn injected_datasets() -> Vec<(String, LtfDataset)> {
    INJECTED
        .iter()
        .map(|(name, _, file)| {
            let bars = fixture::history_bars(file);
            let dataset = if *name == "D" {
                LtfDataset::of_bars(&fixture::daily_of(&bars))
            } else {
                LtfDataset::of_bars(&bars)
            };
            ((*name).to_owned(), dataset)
        })
        .collect()
}

/// Canonical encoding of one script value: Pine `na` (missing or NaN) reads
/// `na`, every other scalar reads its bit-exact text, so two runs are
/// byte-identical exactly when every observed value agrees.
fn canon_value(value: Option<&PineValue>) -> String {
    match value {
        None => "na".to_owned(),
        Some(PineValue::Float(value)) => {
            if value.is_nan() {
                "na".to_owned()
            } else {
                format!("{value:?}")
            }
        }
        Some(PineValue::Int(value)) => format!("int:{value}"),
        Some(PineValue::Bool(value)) => format!("bool:{value}"),
        Some(PineValue::Str(value)) => format!("str:{value}"),
        Some(PineValue::EnumHandle(handle)) => {
            format!("enum:{}.{}", handle.enum_name, handle.member)
        }
        Some(other) => format!("other:{other:?}"),
    }
}

/// Canonical encoding of one fetch-shaped result array.
fn canon_array(values: &[Option<PineValue>]) -> String {
    values
        .iter()
        .map(|value| canon_value(value.as_ref()))
        .collect::<Vec<_>>()
        .join(";")
}

/// The fourteen fetch values on one bar of one probe run.
fn probe_at(outcome: &crate::RunOutcome, bar: usize) -> Vec<Option<PineValue>> {
    match outcome.value_at(bar, "probe") {
        Some(Some(PineValue::Array(values))) => values,
        other => panic!("expected fourteen fetch values on bar {bar}, got {other:?}"),
    }
}

/// Run one probe cell: chart bars at `chart`, every dataset injected (the
/// probed pair mutated by `mutate`), and read the fetch array on `at_bar`
/// (`None` reads the last chart bar).
fn run_cell(
    chart: &str,
    chart_file: &str,
    pair: &str,
    datasets: &[(String, LtfDataset)],
    at_bar: Option<usize>,
    mut mutate: impl FnMut(&mut LtfDataset),
) -> Vec<Option<PineValue>> {
    let bars = fixture::history_bars(chart_file);
    let mut engine = engine_for(chart);
    for (name, dataset) in datasets {
        let mut dataset = dataset.clone();
        if *name == pair {
            mutate(&mut dataset);
        }
        engine
            .inject_security("SPY", name, dataset)
            .expect("probe dataset injects");
    }
    let modules = fixture::linked_modules(vec![fixture::driver_module(&probe_driver(pair))]);
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("probe run evaluates with zero refusals");
    let read_at = at_bar.unwrap_or(bars.len() - 1);
    assert!(
        outcome.value_at(bars.len(), "probe").is_none(),
        "snapshot per chart bar, nothing past the last"
    );
    probe_at(&outcome, read_at)
}

/// One wild but finite sentinel row: ten-times prices, hundred-times
/// volume, so any observation of it cannot hide inside real magnitudes.
fn sentinel_row(time_ms: i64, close: f64, volume: f64) -> (i64, f64, f64, f64, f64, f64) {
    (
        time_ms,
        close * 10.0,
        close * 10.5,
        close * 9.5,
        close * 10.0,
        volume * 100.0,
    )
}

/// Append one row to a dataset in place (callers keep time order).
fn push_row(dataset: &mut LtfDataset, row: (i64, f64, f64, f64, f64, f64)) {
    insert_row_sorted(dataset, row.0, row.1, row.2, row.3, row.4, row.5);
}

/// Insert one row keeping timestamps sorted, after any existing row with
/// the same stamp: the engine reads the confirmed prefix in order, so a
/// same-stamp insert wins as the last confirmed value (last-wins), while a
/// past-the-end insert stays a future row the prefix never reaches.
fn insert_row_sorted(
    dataset: &mut LtfDataset,
    time_ms: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
) {
    let at = dataset.times.partition_point(|time| *time <= time_ms);
    dataset.times.insert(at, time_ms);
    dataset.opens.insert(at, open);
    dataset.highs.insert(at, high);
    dataset.lows.insert(at, low);
    dataset.closes.insert(at, close);
    dataset.volumes.insert(at, volume);
}

/// Every `request.<method>` call site per task closure: `(file, {method:
/// shapes})` in file order of the load. The shape names the merge modes
/// for `security` and the leg count for `security_lower_tf`. Shapes
/// collect per method, so a second same-method site with a new shape
/// cannot hide behind the first. The walker covers expressions, so
/// identifiers inside comments or strings cannot false-positive.
fn request_sites_per_closure(
    indicator: &ClosureEntry,
) -> Vec<(String, BTreeMap<String, BTreeSet<String>>)> {
    let root = closure_data_root(env!("CARGO_MANIFEST_DIR"));
    load_listed_closure(&root, REAL_LIBRARIES, indicator)
        .expect("task closure loads")
        .into_iter()
        .map(|(key, source)| {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("closure file {key} parses: {err}"));
            let mut sites = BTreeMap::new();
            walk_request_stmts(&program.statements, &mut sites);
            (key, sites)
        })
        .collect()
}

/// A dotted `barmerge.<mode>` spelling, or `?` when the argument is anything
/// else (which the engine refuses at the call site).
fn merge_mode(expr: &PineExpr) -> String {
    match expr {
        PineExpr::Variable(spelling) => spelling
            .strip_prefix("barmerge.")
            .unwrap_or("?")
            .to_owned(),
        _ => "?".to_owned(),
    }
}

fn walk_request_call_args(args: &[crate::CallArg], sink: &mut BTreeMap<String, BTreeSet<String>>) {
    for arg in args {
        walk_request_expr(&arg.value, sink);
    }
}

fn walk_request_switch(
    scrutinee: Option<&PineExpr>,
    arms: &[crate::SwitchArm],
    sink: &mut BTreeMap<String, BTreeSet<String>>,
) {
    if let Some(value) = scrutinee {
        walk_request_expr(value, sink);
    }
    for arm in arms {
        if let Some(pattern) = &arm.pattern {
            walk_request_expr(pattern, sink);
        }
        walk_request_stmts(&arm.body, sink);
    }
}

fn walk_request_expr(expr: &PineExpr, sink: &mut BTreeMap<String, BTreeSet<String>>) {
    match expr {
        PineExpr::Number(_)
        | PineExpr::StringLiteral(_)
        | PineExpr::Boolean(_)
        | PineExpr::Variable(_)
        | PineExpr::BuiltinVariable(_)
        | PineExpr::ColorLiteral(_) => {}
        PineExpr::Binary { left, right, .. } => {
            walk_request_expr(left, sink);
            walk_request_expr(right, sink);
        }
        PineExpr::Unary { expr, .. } => walk_request_expr(expr, sink),
        PineExpr::FunctionCall { args, .. } => walk_request_call_args(args, sink),
        PineExpr::MethodCall {
            namespace,
            method,
            args,
            ..
        } => {
            if namespace == "request" {
                let shape = if method == "security" {
                    // Positional merge modes; omitted modes read the
                    // engine's served default, matching the call-site
                    // behavior the probes execute.
                    let gaps = args
                        .get(3)
                        .map_or("gaps_off".to_owned(), |arg| merge_mode(&arg.value));
                    let lookahead = args
                        .get(4)
                        .map_or("lookahead_off".to_owned(), |arg| merge_mode(&arg.value));
                    format!("{gaps}+{lookahead}")
                } else if method == "security_lower_tf" {
                    match args.get(2) {
                        Some(arg) => match &arg.value {
                            PineExpr::ArrayLiteral(items) => format!("legs:{}", items.len()),
                            _ => "legs:?".to_owned(),
                        },
                        None => "legs:?".to_owned(),
                    }
                } else {
                    "other".to_owned()
                };
                sink.entry(method.clone()).or_default().insert(shape);
            }
            walk_request_call_args(args, sink);
        }
        PineExpr::InstanceCall { receiver, args, .. } => {
            walk_request_expr(receiver, sink);
            walk_request_call_args(args, sink);
        }
        PineExpr::FieldRead { receiver, .. } => walk_request_expr(receiver, sink),
        PineExpr::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_request_expr(condition, sink);
            walk_request_expr(then_expr, sink);
            walk_request_expr(else_expr, sink);
        }
        PineExpr::ArrayAccess { array, index } => {
            walk_request_expr(array, sink);
            walk_request_expr(index, sink);
        }
        PineExpr::SeriesOffset { series, .. } => walk_request_expr(series, sink),
        PineExpr::ArrayLiteral(items) => {
            for item in items {
                walk_request_expr(item, sink);
            }
        }
        PineExpr::Switch(switch) => {
            walk_request_switch(switch.scrutinee.as_deref(), &switch.arms, sink);
        }
    }
}

fn walk_request_stmts(stmts: &[PineStmt], sink: &mut BTreeMap<String, BTreeSet<String>>) {
    for stmt in stmts {
        match stmt {
            PineStmt::Import(_) | PineStmt::LibraryDecl { .. } | PineStmt::Break => {}
            PineStmt::IndicatorDecl { args } => walk_request_call_args(args, sink),
            PineStmt::ExportDecl(inner) => {
                walk_request_stmts(std::slice::from_ref(inner), sink);
            }
            PineStmt::FuncDecl { params, body, .. }
            | PineStmt::MethodDecl { params, body, .. } => {
                for param in params {
                    if let Some(default) = &param.default {
                        walk_request_expr(default, sink);
                    }
                }
                walk_request_stmts(body, sink);
            }
            PineStmt::ConstDecl { value, .. }
            | PineStmt::VarDeclaration { value, .. }
            | PineStmt::Declaration { value, .. }
            | PineStmt::Assignment { value, .. }
            | PineStmt::Reassign { value, .. }
            | PineStmt::DestructureAssign { value, .. } => walk_request_expr(value, sink),
            PineStmt::EnumDecl { members, .. } => {
                for member in members {
                    walk_request_expr(&member.value, sink);
                }
            }
            PineStmt::TypeDecl { fields, .. } => {
                for field in fields {
                    if let Some(default) = &field.default {
                        walk_request_expr(default, sink);
                    }
                }
            }
            PineStmt::MultiDeclaration { decls } => {
                for decl in decls {
                    if let Some(value) = &decl.value {
                        walk_request_expr(value, sink);
                    }
                }
            }
            PineStmt::MultiAssign { items } => {
                for item in items {
                    walk_request_expr(&item.value, sink);
                }
            }
            PineStmt::Expression(expr) => walk_request_expr(expr, sink),
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                walk_request_expr(condition, sink);
                walk_request_stmts(then_branch, sink);
                if let Some(branches) = else_branch {
                    walk_request_stmts(branches, sink);
                }
            }
            PineStmt::For {
                start, end, body, ..
            } => {
                walk_request_expr(start, sink);
                walk_request_expr(end, sink);
                walk_request_stmts(body, sink);
            }
            PineStmt::ForIn { iterable, body, .. } => {
                walk_request_expr(iterable, sink);
                walk_request_stmts(body, sink);
            }
            PineStmt::While { condition, body } => {
                walk_request_expr(condition, sink);
                walk_request_stmts(body, sink);
            }
            PineStmt::Switch(switch) => {
                walk_request_switch(switch.scrutinee.as_deref(), &switch.arms, sink);
            }
        }
    }
}

#[test]
#[allow(clippy::type_complexity)]
fn request_sites_per_task_closure_are_pinned() {
    // Syntactic `request.*` inventory per closure (file order of the load):
    // every arm the closures can reach must stay inside the served subset
    // (`security` with the confirmed-bars merge modes, `security_lower_tf`
    // with bare-leg lists) or refuse with the live-fetch reason. A new
    // spelling fails this pin by name before it can run anywhere. Every
    // task closure loads the full seven-library set, so all three pin the
    // same triple: the candle fetch sits linked-but-unreached beside the
    // apex fetch no indicator calls multi-timeframe.
    let triple: &[(&str, &str, &str)] = &[
        ("showmethegrail/libBars", "security_lower_tf", "legs:6"),
        (
            "showmethegrail/libCandles",
            "security",
            "gaps_off+lookahead_off",
        ),
        ("showmethegrail/libApex", "security", "gaps_off+lookahead_off"),
    ];
    let expected: &[(&str, &[(&str, &str, &str)])] = &[
        ("DeltaVolumeProfileProtopyne", triple),
        ("ApexPPAS", triple),
        ("ApexRSIMD", triple),
    ];
    assert_eq!(TASK_INDICATORS.len(), expected.len(), "closures under proof");
    for (indicator, want) in TASK_INDICATORS.iter().zip(expected.iter()) {
        let (_, files) = want;
        let mut got = Vec::new();
        for (key, sites) in request_sites_per_closure(indicator) {
            for (method, shapes) in &sites {
                for shape in shapes {
                    got.push((key.clone(), method.clone(), shape.clone()));
                }
            }
        }
        let mut want_flat = Vec::new();
        for (file, method, shape) in *files {
            want_flat.push((
                (*file).to_owned(),
                (*method).to_owned(),
                (*shape).to_owned(),
            ));
        }
        assert_eq!(got, want_flat, "{} request sites pinned", indicator.name);
    }
}

#[test]
fn daily_resample_of_fifteen_minute_tape_is_pinned() {
    // The timeframe with no CSV of its own: three UTC-day buckets over the
    // trimmed fifteen-minute tape, carrying the bucket-start timestamps.
    let bars = fixture::history_bars("historical/SPY_15M.csv");
    assert_eq!(bars.len(), 116, "15M fixture row count pinned");
    let daily = fixture::daily_of(&bars);
    assert_eq!(daily.len(), 3, "three UTC days covered");
    assert_eq!(
        daily[0].ts.timestamp_millis(),
        1_767_830_400_000,
        "first bucket opens Jan 8 00:00 UTC"
    );
    assert_eq!(
        daily[1].ts.timestamp_millis(),
        1_767_916_800_000,
        "second bucket opens Jan 9 00:00 UTC"
    );
    assert_eq!(
        daily[2].ts.timestamp_millis(),
        1_768_003_200_000,
        "third bucket opens Jan 10 00:00 UTC"
    );
    assert!(
        daily.iter().all(|bar| bar.volume > 0),
        "every bucket carries volume"
    );
}

#[test]
fn libapex_fetch_matches_raw_security_shape() {
    // The probes run the fetch-shaped raw call; this ties that shape to the
    // production `libApex.fetch` code path over the same injected dataset.
    let bars = fixture::history_bars("historical/SPY_1M.csv");
    let last = bars.len() - 1;
    let daily = LtfDataset::of_bars(&fixture::daily_of(&fixture::history_bars("historical/SPY_15M.csv")));
    let datasets = vec![("D".to_owned(), daily.clone())];
    let raw = run_cell("1", "historical/SPY_1M.csv", "D", &datasets, None, |_| {});
    let driver = "import showmethegrail/libUtil/12 as UTIL\n\
        import showmethegrail/libBuffers/6 as libBuffers\n\
        import showmethegrail/libBars/12 as BARS\n\
        import showmethegrail/libDrawing/61 as libDrawing\n\
        import showmethegrail/libApex/88 as libApex\n\
        var ctx = libApex.MarketContext.new()\n\
        if barstate.isfirst\n    \
        ctx.init()\n\
        ctx.fetch(UTIL.Timeframes.D1)\n";
    let modules = linked_driver(driver);
    let mut engine = engine_for("1");
    engine
        .inject_security("SPY", "D", daily)
        .expect("daily dataset injects");
    let outcome = engine
        .run_linked(&modules, &bars, InputSet::new())
        .expect("fetch driver evaluates");
    let state = match outcome.value_at(last, "ctx") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected MarketContext object on bar {last}, got {other:?}"),
    };
    let field = |name: &str| {
        let value = state
            .fields
            .get(name)
            .unwrap_or_else(|| panic!("context carries {name}"));
        canon_value(value.as_ref())
    };
    // Direct assignments read the raw positions; the derived metrics follow
    // the production recomposition exactly (the MTF branch applies its own
    // normalization on top of the already-normalized list values).
    assert_eq!(field("ma_primary"), canon_value(raw[6].as_ref()), "alma leg");
    assert_eq!(field("ma_signal"), canon_value(raw[7].as_ref()), "vwma leg");
    assert_eq!(field("ma_mean"), canon_value(raw[8].as_ref()), "ema leg");
    assert_eq!(field("atr"), canon_value(raw[9].as_ref()), "atr leg");
    assert_eq!(field("atr_long"), canon_value(raw[10].as_ref()), "long atr leg");
    assert_eq!(field("stdev"), canon_value(raw[13].as_ref()), "stdev leg");
    assert_eq!(field("vol"), canon_value(raw[4].as_ref()), "volume leg");
    assert_eq!(field("vol_sma"), canon_value(raw[5].as_ref()), "volume average leg");
    assert_eq!(field("tf"), "enum:Timeframes.D1", "context records the requested timeframe");
}

/// Five synthetic closed bars: enough history for short windows, too short
/// for any kernel to warm, so refusal-shape tests read structure, never
/// tuning.
fn unit_bars() -> Vec<Bar> {
    (0i64..5)
        .map(|i| {
            let close = 100.0 + i as f64;
            let decimal = |value: f64| format!("{value:.2}").parse().expect("decimal leg");
            Bar::new()
                .ts(
                    chrono::TimeZone::timestamp_opt(&chrono::Utc, 1_700_000_000 + i * 60, 0)
                        .single()
                        .expect("bar time in range"),
                )
                .o(decimal(close))
                .h(decimal(close + 1.0))
                .l(decimal(close - 1.0))
                .c(decimal(close))
                .volume(1_000 + i as u64 * 10)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

/// Run one bare driver (no imports) over the unit bars, returning the
/// refusal text.
fn refuse_of(driver: &str) -> String {
    let modules = fixture::linked_modules(vec![fixture::driver_module(driver)]);
    let mut engine = engine_for("1");
    engine
        .run_linked(&modules, &unit_bars(), InputSet::new())
        .expect_err("driver refuses")
        .to_string()
}

#[test]
fn gaps_on_refuses() {
    let err = refuse_of(
        "probe = request.security(\"SPY\", \"5\", close, barmerge.gaps_on, barmerge.lookahead_off)\n",
    );
    assert_eq!(
        err,
        "pine evaluation refused: injected security serves barmerge.gaps_off only, got barmerge.gaps_on",
        "gap-fill mode refuses"
    );
}

#[test]
fn lookahead_on_refuses() {
    let err = refuse_of(
        "probe = request.security(\"SPY\", \"5\", close, barmerge.gaps_off, barmerge.lookahead_on)\n",
    );
    assert_eq!(
        err,
        "pine evaluation refused: injected security serves barmerge.lookahead_off only, got barmerge.lookahead_on",
        "look-ahead mode refuses"
    );
}

#[test]
fn unregistered_pair_refuses_naming_the_live_fetch() {
    let err = refuse_of(&format!(
        "probe = request.security(\"SPY\", \"5\", {FETCH_EXPRS}, barmerge.gaps_off, barmerge.lookahead_off, calc_bars_count=30)\n"
    ));
    assert_eq!(
        err,
        "pine evaluation refused: live request.security is refused headlessly; inject multi-timeframe bars and tick data through the run instead of a network fetch",
        "unregistered pair names the live fetch"
    );
}

#[test]
fn unknown_request_method_refuses() {
    let err = refuse_of("probe = request.dividends(\"SPY\", \"D\", close)\n");
    assert_eq!(
        err,
        "pine evaluation refused: live request.dividends is refused headlessly; inject multi-timeframe bars and tick data through the run instead of a network fetch",
        "unknown request arm names the live fetch"
    );
}

#[test]
fn security_history_leg_refuses_naming_the_subset() {
    // History-subscripted legs (the candle-library fetch shape) sit outside
    // the served subset: bare legs, single-source kernel calls, and scalar
    // operators only. The pair injects so the refusal under test is the
    // subset gate, not the unregistered-pair gate.
    let modules = fixture::linked_modules(vec![fixture::driver_module(
        "probe = request.security(\"SPY\", \"5\", [close, close[1]], barmerge.gaps_off, barmerge.lookahead_off)\n",
    )]);
    let mut engine = engine_for("1");
    engine
        .inject_security("SPY", "5", LtfDataset::of_bars(&unit_bars()))
        .expect("unit dataset injects");
    let err = engine
        .run_linked(&modules, &unit_bars(), InputSet::new())
        .expect_err("history legs refuse")
        .to_string();
    assert_eq!(
        err,
        "pine evaluation refused: injected security serves bare legs, kernel-backed single-source ta calls and scalar operators only",
        "history legs refuse"
    );
}

#[test]
fn lower_tf_non_leg_expression_refuses() {
    let err = refuse_of(
        "probe = request.security_lower_tf(\"SPY\", \"1\", [close, ta.sma(close, 20)])\n",
    );
    assert_eq!(
        err,
        "pine evaluation refused: injected lower-tf serves bare chart legs only, never computed expressions",
        "non-leg pull expression refuses"
    );
}

/// Calibration over three chart timeframes times three injected
/// timeframes: a future sentinel bar stays invisible in every cell, and a
/// same-bar sentinel stays visible (the control proving the probe can tell
/// future from current bars). All nine cells must hold before any golden
/// may be checked in over this path.
#[test]
fn lookahead_calibration_nine_cells_green() {
    let datasets = injected_datasets();
    let mut failures = Vec::new();
    for (chart, chart_file) in CHARTS.iter().copied() {
        let bars = fixture::history_bars(chart_file);
        let chart_last_ms = bars.last().expect("chart nonempty").ts.timestamp_millis();
        for (pair, step_ms, _) in INJECTED.iter().copied() {
            let base = datasets
                .iter()
                .find(|(name, _)| name == pair)
                .expect("dataset present")
                .1
                .clone();
            let base_last = base.times.last().copied().expect("dataset nonempty");
            let base_close = base.closes.last().copied().expect("dataset close");
            let base_volume = base.volumes.last().copied().expect("dataset volume");
            let cell = format!("chart={chart} injected={pair}");
            let plain = canon_array(&run_cell(chart, chart_file, pair, &datasets, None, |_| {}));
            let future_ms = chart_last_ms.max(base_last) + step_ms;
            let spiked =
                canon_array(&run_cell(chart, chart_file, pair, &datasets, None, |dataset| {
                    push_row(dataset, sentinel_row(future_ms, base_close, base_volume));
                }));
            if spiked != plain {
                failures.push(format!("{cell}: future sentinel leaked into the fetch"));
            }
            let same_ms = chart_last_ms.div_euclid(step_ms) * step_ms;
            let controlled =
                canon_array(&run_cell(chart, chart_file, pair, &datasets, None, |dataset| {
                    push_row(dataset, sentinel_row(same_ms, base_close, base_volume));
                }));
            if controlled == plain {
                failures.push(format!("{cell}: same-bar control stayed invisible"));
            }
        }
    }
    assert!(failures.is_empty(), "look-ahead calibration failures: {failures:?}");
}

/// Gap calibration over the same nine cells: deleting one injected row
/// carries the last confirmed value forward to the first chart bar past
/// the hole, and the carried value differs from the hole-free run (so the
/// probe is not vacuous). The deleted timestamp is recorded per cell:
/// visibility, not concealment.
#[test]
fn gap_calibration_nine_cells_green() {
    let datasets = injected_datasets();
    let mut failures = Vec::new();
    for (chart, chart_file) in CHARTS.iter().copied() {
        let bars = fixture::history_bars(chart_file);
        let chart_first_ms = bars.first().expect("chart nonempty").ts.timestamp_millis();
        for (pair, _, _) in INJECTED.iter().copied() {
            let base = datasets
                .iter()
                .find(|(name, _)| name == pair)
                .expect("dataset present")
                .1
                .clone();
            let cell = format!("chart={chart} injected={pair}");
            // The hole goes to the row nearest the chart midpoint that a
            // chart bar can actually observe (a bar opens inside
            // `[hole, next)`): otherwise the deletion is invisible by grid
            // geometry, not by carry-forward, and the cell proves nothing.
            let middle_ms = chart_first_ms
                + (bars.last().expect("chart nonempty").ts.timestamp_millis()
                    - chart_first_ms)
                    / 2;
            let mut candidates: Vec<usize> = (1..base.times.len()).collect();
            candidates.sort_by_key(|&index| (base.times[index] - middle_ms).abs());
            let found = candidates.into_iter().find_map(|index| {
                let hole_ms = base.times[index];
                let next_ms = base.times.get(index + 1).copied().unwrap_or(i64::MAX);
                let observing = bars.iter().any(|bar| {
                    let time = bar.ts.timestamp_millis();
                    time >= hole_ms && time < next_ms
                });
                observing.then_some((index, hole_ms, next_ms))
            });
            let Some((hole, hole_ms, next_ms)) = found else {
                failures.push(format!("{cell}: no observable hole on these grids"));
                continue;
            };
            let carried_close = base.closes[hole - 1];
            let carried_canon = canon_value(Some(&PineValue::Float(carried_close)));
            let probe_bar = bars
                .iter()
                .position(|bar| {
                    let time = bar.ts.timestamp_millis();
                    time >= hole_ms && time < next_ms
                })
                .expect("hole is observed");
            let plain = run_cell(chart, chart_file, pair, &datasets, Some(probe_bar), |_| {});
            let gapped = run_cell(chart, chart_file, pair, &datasets, Some(probe_bar), |dataset| {
                dataset.times.remove(hole);
                dataset.opens.remove(hole);
                dataset.highs.remove(hole);
                dataset.lows.remove(hole);
                dataset.closes.remove(hole);
                dataset.volumes.remove(hole);
            });
            // Bare `close` is the fourth fetch position.
            let gapped_close = canon_value(gapped[3].as_ref());
            let plain_close = canon_value(plain[3].as_ref());
            if gapped_close != carried_canon {
                failures.push(format!(
                    "{cell}: hole {hole_ms} carried {gapped_close}, want last confirmed {carried_canon}"
                ));
            }
            if gapped_close == plain_close {
                failures.push(format!("{cell}: hole {hole_ms} left no observable gap"));
            }
        }
    }
    assert!(failures.is_empty(), "gap calibration failures: {failures:?}");
}
