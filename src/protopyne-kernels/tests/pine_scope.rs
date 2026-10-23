//! Persistent-binding scoping: a `var`/`varip` cell belongs to the call
//! instance that declares it. Two declarations of one spelling — in sibling
//! functions or in linked modules — are separate cells, and two call
//! expressions of one function are two instances of the same declaration.
//!
//! The control run pins that a script whose declarations never collide keeps
//! producing exactly the snapshots it produced before cells were scoped.

use super::*;
use crate::{inputs::InputSet, parse_program, PineRuntimeConfig};
use chrono::TimeZone as _;
use protopyne_types::Bar;

fn bar(close: &str, ts: i64) -> Bar {
    Bar::new()
        .ts(chrono::Utc
            .timestamp_opt(ts, 0)
            .single()
            .expect("valid test timestamp"))
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c(close.parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn bars() -> Vec<Bar> {
    vec![
        bar("10.00", 1_000_000),
        bar("11.00", 1_000_060),
        bar("12.00", 1_000_120),
    ]
}

fn run(script: &str) -> RunOutcome {
    let program = parse_program(script).expect("script parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine
        .run(&program, &bars(), InputSet::new())
        .expect("script runs")
}

/// Numeric reading of one snapshot cell (`int` auto-casts, like the engine).
fn num(outcome: &RunOutcome, at_bar: usize, name: &str) -> Option<f64> {
    match outcome.value_at(at_bar, name)? {
        Some(PineValue::Float(v)) => Some(v),
        Some(PineValue::Int(v)) => Some(v as f64),
        _ => None,
    }
}

/// Every snapshot rendered as `bar: name=value, ...` with names sorted, so
/// one assertion covers the whole evaluated namespace.
fn canon(outcome: &RunOutcome) -> String {
    let mut lines = Vec::with_capacity(outcome.snapshots.len());
    for (index, snapshot) in outcome.snapshots.iter().enumerate() {
        let mut cells: Vec<String> = snapshot
            .values
            .iter()
            .map(|(name, value)| format!("{name}={value:?}"))
            .collect();
        cells.sort();
        lines.push(format!("{index}: {}", cells.join(", ")));
    }
    lines.join("\n")
}

#[test]
fn two_functions_declaring_one_spelling_keep_separate_cells() {
    let outcome = run("\
f() =>
    var x = 0.0
    x := x + 1.0
    x

g() =>
    var x = 100.0
    x := x + 1.0
    x

fv = f()
gv = g()
");
    assert_eq!(num(&outcome, 0, "fv"), Some(1.0));
    assert_eq!(num(&outcome, 0, "gv"), Some(101.0));
    assert_eq!(num(&outcome, 2, "fv"), Some(3.0));
    assert_eq!(num(&outcome, 2, "gv"), Some(103.0));
}

#[test]
fn two_calls_of_one_function_in_a_bar_are_two_instances() {
    let outcome = run("\
f() =>
    var x = 0.0
    x := x + 1.0
    x

a = f()
b = f()
");
    assert_eq!(num(&outcome, 0, "a"), Some(1.0));
    assert_eq!(num(&outcome, 0, "b"), Some(1.0));
    assert_eq!(num(&outcome, 2, "a"), Some(3.0));
    assert_eq!(num(&outcome, 2, "b"), Some(3.0));
}

#[test]
fn two_modules_declaring_one_spelling_keep_separate_cells() {
    let left = parse_program("var x = 10.0\nx := x + 1.0\nleft_out = x\n").expect("parses");
    let right = parse_program("var x = 100.0\nx := x + 1.0\nright_out = x\n").expect("parses");
    let modules = vec![
        LinkedModule::new("lib/left", left, HashMap::new()),
        LinkedModule::new("lib/right", right, HashMap::new()),
    ];
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    let outcome = engine
        .run_linked(&modules, &bars(), InputSet::new())
        .expect("run");
    assert_eq!(num(&outcome, 0, "left_out"), Some(11.0));
    assert_eq!(num(&outcome, 0, "right_out"), Some(101.0));
    assert_eq!(num(&outcome, 2, "left_out"), Some(13.0));
    assert_eq!(num(&outcome, 2, "right_out"), Some(103.0));
}

#[test]
fn a_script_without_colliding_declarations_keeps_its_snapshots() {
    let outcome = run("\
var total = 0.0
varip ticks = 0.0
total := total + close
ticks := ticks + 1.0

bump(float step) =>
    var running = 0.0
    running := running + step
    running

bumped = bump(2.0)
total
");
    assert_eq!(
        canon(&outcome),
        "\
0: bumped=Some(Float(2.0)), running=Some(Float(2.0)), ticks=Some(Float(1.0)), total=Some(Float(10.0))
1: bumped=Some(Float(4.0)), running=Some(Float(4.0)), total=Some(Float(21.0))
2: bumped=Some(Float(6.0)), running=Some(Float(6.0)), total=Some(Float(33.0))"
    );
}
