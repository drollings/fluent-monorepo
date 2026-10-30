//! Closed-coverage probe over the discovered closures: every `ta.<name>`
//! call site across every linkable closure (fixture and live, indicator
//! plus the libraries it links) resolves to a bridged method or a
//! checked-in refusal; no closure destructures a `ta.*` result; and every
//! `input.*` decorator spelling the fixture files use parses and runs.
//!
//! `libTrendConsensus.<item>` sites are outside this probe's scope by
//! construction: the triage suite pins zero references, so there is
//! nothing to bridge and nothing to refuse-list. The probe walks parsed
//! ASTs (never text), so identifiers inside comments, strings, or longer
//! names (`DiagnosticData.new`) cannot false-positive.
//!
//! Exact per-script `ta.*` vocabularies are volatile file content and are
//! not pinned here: the property that every site is bridged-or-refused
//! survives any directory churn, while a vocabulary list would fail on
//! every added script. Per-file destructure counts pin over the
//! checked-in fixtures only.
//!
//! Test path note: this file rides the `pine::builtins_ta` unit module,
//! so the runnable filter is `pine::builtins_ta`, not the file stem.

use super::*;
use crate::pine::test_fixture as fixture;
use crate::{
    parse_program, ClosureOutcome, InputSet, PineEngine, PineExpr, PineRuntimeConfig, PineStmt,
};
use std::collections::{BTreeMap, BTreeSet};

/// Checked-in refuse-list: `(method, exact refusal string)` for closure
/// call sites the bridge knowingly refuses. Empty today: every `ta.*`
/// name the discovered closures touch is bridged. Any future entry must
/// carry its exact refusal string, and the probe below executes it.
const REFUSE_LIST: &[(&str, &str)] = &[];

fn closure_programs() -> Vec<(String, crate::PineProgram)> {
    let mut outcomes = fixture::fixture_outcomes();
    outcomes.extend(fixture::discovered_outcomes());
    let mut programs = Vec::new();
    for outcome in &outcomes {
        let ClosureOutcome::Linked(closure) = outcome else {
            continue;
        };
        for (key, source) in &closure.sources {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("closure file {key} parses: {err}"));
            programs.push((key.clone(), program));
        }
    }
    programs
}

fn walk_call_args(args: &[crate::CallArg], sink: &mut BTreeSet<String>) {
    for arg in args {
        walk_expr(&arg.value, sink);
    }
}

fn walk_switch(
    scrutinee: Option<&PineExpr>,
    arms: &[crate::SwitchArm],
    sink: &mut BTreeSet<String>,
) {
    if let Some(value) = scrutinee {
        walk_expr(value, sink);
    }
    for arm in arms {
        if let Some(pattern) = &arm.pattern {
            walk_expr(pattern, sink);
        }
        walk_stmts(&arm.body, sink);
    }
}

fn walk_expr(expr: &PineExpr, sink: &mut BTreeSet<String>) {
    match expr {
        PineExpr::Number(_)
        | PineExpr::StringLiteral(_)
        | PineExpr::Boolean(_)
        | PineExpr::Variable(_)
        | PineExpr::BuiltinVariable(_)
        | PineExpr::ColorLiteral(_) => {}
        PineExpr::Binary { left, right, .. } => {
            walk_expr(left, sink);
            walk_expr(right, sink);
        }
        PineExpr::Unary { expr, .. } => walk_expr(expr, sink),
        PineExpr::FunctionCall { args, .. } => walk_call_args(args, sink),
        PineExpr::MethodCall {
            namespace,
            method,
            args,
            ..
        } => {
            if namespace == "ta" {
                sink.insert(method.clone());
            }
            walk_call_args(args, sink);
        }
        PineExpr::InstanceCall { receiver, args, .. } => {
            walk_expr(receiver, sink);
            walk_call_args(args, sink);
        }
        PineExpr::FieldRead { receiver, .. } => walk_expr(receiver, sink),
        PineExpr::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            walk_expr(condition, sink);
            walk_expr(then_expr, sink);
            walk_expr(else_expr, sink);
        }
        PineExpr::ArrayAccess { array, index } => {
            walk_expr(array, sink);
            walk_expr(index, sink);
        }
        PineExpr::SeriesOffset { series, .. } => walk_expr(series, sink),
        PineExpr::ArrayLiteral(items) => {
            for item in items {
                walk_expr(item, sink);
            }
        }
        PineExpr::Switch(switch) => walk_switch(switch.scrutinee.as_deref(), &switch.arms, sink),
    }
}

fn walk_stmts(stmts: &[PineStmt], sink: &mut BTreeSet<String>) {
    for stmt in stmts {
        match stmt {
            PineStmt::Import(_) | PineStmt::LibraryDecl { .. } | PineStmt::Break => {}
            PineStmt::IndicatorDecl { args } => walk_call_args(args, sink),
            PineStmt::ExportDecl(inner) => walk_stmts(std::slice::from_ref(inner), sink),
            PineStmt::FuncDecl { params, body, .. } | PineStmt::MethodDecl { params, body, .. } => {
                for param in params {
                    if let Some(default) = &param.default {
                        walk_expr(default, sink);
                    }
                }
                walk_stmts(body, sink);
            }
            PineStmt::ConstDecl { value, .. }
            | PineStmt::VarDeclaration { value, .. }
            | PineStmt::Declaration { value, .. }
            | PineStmt::Assignment { value, .. }
            | PineStmt::Reassign { value, .. }
            | PineStmt::DestructureAssign { value, .. } => walk_expr(value, sink),
            PineStmt::EnumDecl { members, .. } => {
                for member in members {
                    walk_expr(&member.value, sink);
                }
            }
            PineStmt::TypeDecl { fields, .. } => {
                for field in fields {
                    if let Some(default) = &field.default {
                        walk_expr(default, sink);
                    }
                }
            }
            PineStmt::MultiDeclaration { decls } => {
                for decl in decls {
                    if let Some(value) = &decl.value {
                        walk_expr(value, sink);
                    }
                }
            }
            PineStmt::MultiAssign { items } => {
                for item in items {
                    walk_expr(&item.value, sink);
                }
            }
            PineStmt::Expression(expr) => walk_expr(expr, sink),
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                walk_expr(condition, sink);
                walk_stmts(then_branch, sink);
                if let Some(branches) = else_branch {
                    walk_stmts(branches, sink);
                }
            }
            PineStmt::For {
                start, end, body, ..
            } => {
                walk_expr(start, sink);
                walk_expr(end, sink);
                walk_stmts(body, sink);
            }
            PineStmt::ForIn { iterable, body, .. } => {
                walk_expr(iterable, sink);
                walk_stmts(body, sink);
            }
            PineStmt::While { condition, body } => {
                walk_expr(condition, sink);
                walk_stmts(body, sink);
            }
            PineStmt::Switch(switch) => {
                walk_switch(switch.scrutinee.as_deref(), &switch.arms, sink);
            }
        }
    }
}

/// Every `ta.*` name per closure file, in file order of the discovery
/// load (libraries first, indicator root last).
fn ta_sites_per_closure() -> Vec<(String, BTreeSet<String>)> {
    closure_programs()
        .iter()
        .map(|(key, program)| {
            let mut sink = BTreeSet::new();
            walk_stmts(&program.statements, &mut sink);
            (key.clone(), sink)
        })
        .collect()
}

#[test]
fn task_closure_ta_calls_are_closed_over_bridge_or_refuse_list() {
    let supported: BTreeSet<&str> = TaBridge::supported_methods().iter().copied().collect();
    let refused: BTreeMap<&str, &str> = REFUSE_LIST.iter().copied().collect();
    // The refuse-list carries exact strings, so its shape is pinned here:
    // any entry must be deliberate, never a silent widening.
    assert_eq!(
        REFUSE_LIST,
        &[],
        "no closure call site is knowingly refused"
    );
    let mut uncovered = Vec::new();
    for (file, sites) in ta_sites_per_closure() {
        for method in &sites {
            if !supported.contains(method.as_str()) && !refused.contains_key(method.as_str()) {
                uncovered.push(format!("{file}: ta.{method}"));
            }
        }
    }
    assert!(
        uncovered.is_empty(),
        "closure ta.* calls outside the bridge and refuse-list: {uncovered:?}"
    );
}

#[test]
fn task_closure_ta_vocabulary_agrees_across_closures() {
    // One on-disk source per publisher/name serves every closure, so a
    // file's `ta.*` vocabulary is closure-independent — and the fixture
    // closures exercise the bridge (non-empty union). Exact vocabularies
    // are not pinned: they are volatile file content, and the probe above
    // already proves every site is bridged-or-refused.
    let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut union: BTreeSet<String> = BTreeSet::new();
    for (file, sites) in ta_sites_per_closure() {
        union.extend(sites.iter().cloned());
        match seen.get(&file) {
            Some(known) => assert_eq!(known, &sites, "{file} vocabulary is closure-independent"),
            None => {
                seen.insert(file, sites);
            }
        }
    }
    assert!(!union.is_empty(), "the discovered closures exercise ta.*");
}

#[test]
fn no_task_closure_destructures_a_ta_result() {
    // Tuple-unpacking verdict over every discovered closure: destructuring
    // hits user-function and method results, never a `ta.*` call — with
    // one deliberate exception: `ta.dmi` returns its Wilder triple
    // ([+DI, -DI, ADX]) as an array, and the one closure spelling that
    // triple unpacks it. Any other `ta.*` destructuring fails this test
    // by name instead of silently binding a primary series.
    fn destructure_ta_rhs(stmts: &[PineStmt], hits: &mut Vec<String>) {
        for stmt in stmts {
            match stmt {
                PineStmt::DestructureAssign { targets, value } => {
                    if matches!(
                        value.as_ref(),
                        PineExpr::MethodCall { namespace, method, .. }
                            if namespace == "ta" && method != "dmi"
                    ) {
                        hits.push(format!("destructures ta.* into {} targets", targets.len()));
                    }
                }
                PineStmt::FuncDecl { body, .. } | PineStmt::MethodDecl { body, .. } => {
                    destructure_ta_rhs(body, hits);
                }
                PineStmt::If {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    destructure_ta_rhs(then_branch, hits);
                    if let Some(branches) = else_branch {
                        destructure_ta_rhs(branches, hits);
                    }
                }
                PineStmt::For { body, .. }
                | PineStmt::ForIn { body, .. }
                | PineStmt::While { body, .. } => destructure_ta_rhs(body, hits),
                PineStmt::Switch(switch) => {
                    for arm in &switch.arms {
                        destructure_ta_rhs(&arm.body, hits);
                    }
                }
                PineStmt::ExportDecl(inner) => {
                    destructure_ta_rhs(std::slice::from_ref(inner), hits);
                }
                _ => {}
            }
        }
    }
    let mut hits = Vec::new();
    // The no-destructuring verdict holds over every discovered closure —
    // fixture and live; the exact per-file counts pin over the hermetic
    // fixture closures only, where file bytes are checked in.
    for (file, program) in closure_programs() {
        let mut file_hits = Vec::new();
        destructure_ta_rhs(&program.statements, &mut file_hits);
        for hit in file_hits {
            hits.push(format!("{file}: {hit}"));
        }
    }
    assert!(hits.is_empty(), "ta.* destructuring sites: {hits:?}");
    // Count all destructure sites per file for the drift pin below.
    fn count_sites(stmts: &[PineStmt], count: &mut usize) {
        for stmt in stmts {
            match stmt {
                PineStmt::DestructureAssign { .. } => *count += 1,
                PineStmt::FuncDecl { body, .. }
                | PineStmt::MethodDecl { body, .. }
                | PineStmt::For { body, .. }
                | PineStmt::ForIn { body, .. }
                | PineStmt::While { body, .. } => count_sites(body, count),
                PineStmt::If {
                    then_branch,
                    else_branch,
                    ..
                } => {
                    count_sites(then_branch, count);
                    if let Some(branches) = else_branch {
                        count_sites(branches, count);
                    }
                }
                PineStmt::Switch(switch) => {
                    for arm in &switch.arms {
                        count_sites(&arm.body, count);
                    }
                }
                PineStmt::ExportDecl(inner) => {
                    count_sites(std::slice::from_ref(inner), count);
                }
                _ => {}
            }
        }
    }
    // Exact per-file destructure counts pin over the hermetic fixture
    // closures only: file bytes are checked in, so drift fails loudly
    // here — never over the volatile live root.
    let mut fixture_counts: BTreeMap<String, usize> = BTreeMap::new();
    for outcome in fixture::fixture_outcomes() {
        let ClosureOutcome::Linked(closure) = &outcome else {
            continue;
        };
        for (file, source) in &closure.sources {
            let program = parse_program(&source.text)
                .unwrap_or_else(|err| panic!("fixture file {file} parses: {err}"));
            let mut count = 0_usize;
            count_sites(&program.statements, &mut count);
            if count > 0 {
                match fixture_counts.get(file) {
                    Some(known) => assert_eq!(
                        *known, count,
                        "{file} destructure count is closure-independent"
                    ),
                    None => {
                        fixture_counts.insert(file.clone(), count);
                    }
                }
            }
        }
    }
    assert_eq!(
        fixture_counts,
        BTreeMap::from([
            ("local/_DeltaVolumeProfileProtopyne".to_owned(), 1_usize),
            ("local/_IntegrationLite".to_owned(), 2_usize),
            ("showmethegrail/libApex".to_owned(), 2_usize),
            ("showmethegrail/libBars".to_owned(), 1_usize),
            ("showmethegrail/libBuffers".to_owned(), 2_usize),
            ("showmethegrail/libCandles".to_owned(), 3_usize),
            ("showmethegrail/libDrawing".to_owned(), 1_usize),
        ]),
        "per-file destructure counts pinned (drift fails loudly)",
    );
}

fn audit_bars() -> Vec<protopyne_types::Bar> {
    use chrono::TimeZone as _;
    (0..3)
        .map(|i| {
            protopyne_types::Bar::new()
                .ts(chrono::Utc
                    .timestamp_opt(1_700_000_000 + i * 60, 0)
                    .single()
                    .expect("valid test timestamp"))
                .o("100.00".parse().expect("decimal"))
                .h("101.00".parse().expect("decimal"))
                .l("99.00".parse().expect("decimal"))
                .c(format!("{}.00", 100 + i).parse().expect("decimal"))
                .volume(1_000_u64)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn run_audit(script: &str) -> crate::RunOutcome {
    let program = parse_program(script).expect("audit probe parses");
    let mut engine = PineEngine::new(PineRuntimeConfig::new().build());
    engine
        .run(&program, &audit_bars(), InputSet::new())
        .expect("audit probe runs")
}

fn last_text(outcome: &crate::RunOutcome, name: &str) -> String {
    let at = outcome.snapshots.len() - 1;
    match outcome.value_at(at, name).flatten() {
        Some(crate::PineValue::Str(text)) => text,
        other => panic!("{name} reads text, got {other:?}"),
    }
}

#[test]
fn input_decorators_parse_and_run_with_defaults() {
    // Every `input.*` decorator spelling the task files use is either
    // honored (`InputDef`/`InputSet`: defaults, bounds, options shape) or
    // explicitly inert (presentation: group/inline/display/tooltip ride
    // along without touching numerics). Each probe returns its default.
    let outcome = run_audit(
        "x = input.int(14, \"L\", group = \"g\", inline = \"i\", tooltip = \"t\", minval = 1, maxval = 100, step = 1, display = display.all)",
    );
    let at = outcome.snapshots.len() - 1;
    assert_eq!(
        outcome.value_at(at, "x").flatten(),
        Some(crate::PineValue::Int(14)),
        "input.int default reads back through all decorators"
    );
    let outcome = run_audit(
        "x = input.float(2.0, \"F\", group = \"g\", inline = \"i\", tooltip = \"t\", minval = 0.5, maxval = 5.0, step = 0.5, display = display.all)",
    );
    let at = outcome.snapshots.len() - 1;
    assert_eq!(
        outcome.value_at(at, "x").flatten(),
        Some(crate::PineValue::Float(2.0)),
        "input.float default reads back through all decorators"
    );
    let outcome = run_audit(
        "x = input.bool(true, \"B\", group = \"g\", inline = \"i\", tooltip = \"t\", display = display.all)",
    );
    let at = outcome.snapshots.len() - 1;
    assert_eq!(
        outcome.value_at(at, "x").flatten(),
        Some(crate::PineValue::Bool(true)),
        "input.bool default reads back through all decorators"
    );
    let outcome = run_audit(
        "x = input.string(\"Disabled\", \"S\", options = [\"Enabled\", \"Disabled\"], group = \"g\", inline = \"i\", tooltip = \"t\", display = display.all)",
    );
    assert_eq!(
        last_text(&outcome, "x"),
        "Disabled",
        "input.string default reads back through options and presentation decorators"
    );
    let outcome = run_audit(
        "x = input.source(close, \"Src\", group = \"g\", inline = \"i\", tooltip = \"t\", display = display.all)",
    );
    let at = outcome.snapshots.len() - 1;
    assert_eq!(
        outcome.value_at(at, "x").flatten(),
        Some(crate::PineValue::Float(102.0)),
        "input.source default reads the close leg"
    );
    let outcome = run_audit(
        "enum Kind\n    A = \"A\"\n    B = \"B\"\nx = input.enum(Kind.A, \"K\", options = [Kind.A, Kind.B], group = \"g\", inline = \"i\", tooltip = \"t\", display = display.all)",
    );
    let at = outcome.snapshots.len() - 1;
    assert!(
        matches!(
            outcome.value_at(at, "x").flatten(),
            Some(crate::PineValue::EnumHandle(_))
        ),
        "input.enum default reads back through options and presentation decorators"
    );
    let outcome = run_audit(
        "x = input.color(#089981, \"C\", group = \"g\", inline = \"i\", tooltip = \"t\", display = display.all)",
    );
    let at = outcome.snapshots.len() - 1;
    assert!(
        matches!(
            outcome.value_at(at, "x").flatten(),
            Some(crate::PineValue::ColorHandle(_))
        ),
        "input.color default reads back through presentation decorators"
    );
    // `input.timeframe` answers its period string so the Pine-level
    // `UTIL.getTimeframe` switch resolves it; the string is validated
    // against the chart-period parser, never passed through unchecked.
    let outcome = run_audit(
        "x = input.timeframe(\"D\", \"HTF\", group = \"g\", inline = \"i\", tooltip = \"t\")",
    );
    assert_eq!(
        last_text(&outcome, "x"),
        "D",
        "input.timeframe default reads back its period string"
    );
}
