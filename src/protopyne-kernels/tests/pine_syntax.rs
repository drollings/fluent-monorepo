//! Parser tests: transcribed egui round-trips plus target-file goldens.
//!
//! The egui-charts crate is reference-only and never a dependency, so its
//! parser unit tests are transcribed here as literal expectations. Two
//! deliberate additive divergences from egui shapes (never copied): call
//! arguments preserve names (`CallArg { name, value }` instead of bare
//! expressions) and `var` declarations carry an optional type. Everything
//! else on the overlap parses to identical shapes.

// Bit-exact float comparison is the point of the differential tests: the
// transcribed egui expectations assert identical `f64` literals.
#![allow(clippy::float_cmp)]

use super::*;
use std::path::PathBuf;

fn parse(text: &str) -> PineProgram {
    parse_program(text).expect("parses")
}

fn data_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs").join(name)
}

fn load(name: &str) -> PineProgram {
    let text = std::fs::read_to_string(data_path(name)).expect("target readable");
    parse_program(&text).expect("target parses losslessly")
}

// ---- Transcribed egui corpus: `test_parse_simple_assignment` ----

#[test]
fn roundtrip_assignment_matches_egui() {
    // egui input "length = 20": one `Assignment` statement.
    let program = parse("length = 20");
    assert_eq!(program.statements.len(), 1);
    match &program.statements[0] {
        PineStmt::Assignment { name, value } => {
            assert_eq!(name, "length");
            assert!(matches!(value.as_ref(), PineExpr::Number(n) if *n == 20.0));
        }
        other => panic!("expected assignment, got {other:?}"),
    }
}

// ---- Transcribed egui corpus: `test_parse_function_call` ----

#[test]
fn roundtrip_function_call_matches_egui() {
    // egui input "plot(close)": one expression statement calling `plot`
    // with a single close argument (ours wraps it in an unnamed `CallArg`).
    let program = parse("plot(close)");
    assert_eq!(program.statements.len(), 1);
    match &program.statements[0] {
        PineStmt::Expression(PineExpr::FunctionCall { name, args, .. }) => {
            assert_eq!(name, "plot");
            assert_eq!(args.len(), 1);
            assert_eq!(args[0].name, None);
            assert!(matches!(
                &args[0].value,
                PineExpr::BuiltinVariable(BuiltinVar::Close)
            ));
        }
        other => panic!("expected function call, got {other:?}"),
    }
}

// ---- Transcribed egui corpus: `test_parse_method_call` ----

#[test]
fn roundtrip_method_call_matches_egui() {
    // egui input "ta.sma(close, 20)": namespace `ta`, method `sma`.
    let program = parse("ta.sma(close, 20)");
    assert_eq!(program.statements.len(), 1);
    match &program.statements[0] {
        PineStmt::Expression(PineExpr::MethodCall {
            namespace,
            method,
            args,
            ..
        }) => {
            assert_eq!(namespace, "ta");
            assert_eq!(method, "sma");
            assert_eq!(args.len(), 2);
            assert!(matches!(
                &args[0].value,
                PineExpr::BuiltinVariable(BuiltinVar::Close)
            ));
            assert!(matches!(&args[1].value, PineExpr::Number(n) if *n == 20.0));
        }
        other => panic!("expected method call, got {other:?}"),
    }
}

// ---- Transcribed egui corpus: `test_parse_binary_expression` ----

#[test]
fn roundtrip_binary_matches_egui() {
    // egui input "close > open": greater comparison of two builtins.
    let program = parse("close > open");
    assert_eq!(program.statements.len(), 1);
    match &program.statements[0] {
        PineStmt::Expression(PineExpr::Binary { left, op, right }) => {
            assert_eq!(*op, BinaryOp::Greater);
            assert!(matches!(left.as_ref(), PineExpr::BuiltinVariable(BuiltinVar::Close)));
            assert!(matches!(right.as_ref(), PineExpr::BuiltinVariable(BuiltinVar::Open)));
        }
        other => panic!("expected binary expression, got {other:?}"),
    }
}

// ---- Overlap probes: precedence and shapes beyond egui's unit tests ----

#[test]
fn precedence_mirrors_egui_climb() {
    // ternary < or < and < equality < comparison < add < mul < unary < postfix.
    let program = parse("a + b * -c > d and e or f == g ? h : i");
    match &program.statements[0] {
        PineStmt::Expression(PineExpr::Ternary { condition, .. }) => match condition.as_ref() {
            PineExpr::Binary { op, .. } => assert_eq!(*op, BinaryOp::Or),
            other => panic!("expected or at ternary head, got {other:?}"),
        },
        other => panic!("expected ternary, got {other:?}"),
    }
    let program = parse("not a and b");
    match &program.statements[0] {
        PineStmt::Expression(PineExpr::Binary { op, left, .. }) => {
            assert_eq!(*op, BinaryOp::And);
            assert!(matches!(left.as_ref(), PineExpr::Unary { .. }));
        }
        other => panic!("expected and, got {other:?}"),
    }
}

#[test]
fn series_offset_and_access_match_egui() {
    // egui rule: a bare numeric index is a series offset, anything else
    // is an array access.
    let program = parse("x = close[1]");
    match &program.statements[0] {
        PineStmt::Assignment { value, .. } => {
            assert!(matches!(
                value.as_ref(),
                PineExpr::SeriesOffset { offset: 1, .. }
            ));
        }
        other => panic!("expected assignment, got {other:?}"),
    }
    let program = parse("x = a[i + 1]");
    match &program.statements[0] {
        PineStmt::Assignment { value, .. } => {
            assert!(matches!(value.as_ref(), PineExpr::ArrayAccess { .. }));
        }
        other => panic!("expected assignment, got {other:?}"),
    }
}

#[test]
fn named_args_preserved_not_discarded() {
    // egui silently drops `title=...`; this parser keeps every name.
    let program = parse("x = input(close, title=\"T\")");
    match &program.statements[0] {
        PineStmt::Assignment { value, .. } => match value.as_ref() {
            PineExpr::FunctionCall { name, args, .. } => {
                assert_eq!(name, "input");
                assert_eq!(args.len(), 2);
                assert_eq!(args[0].name, None);
                assert_eq!(args[1].name.as_deref(), Some("title"));
                assert!(matches!(
                    &args[1].value,
                    PineExpr::StringLiteral(s) if s == "T"
                ));
            }
            other => panic!("expected call, got {other:?}"),
        },
        other => panic!("expected assignment, got {other:?}"),
    }
}

#[test]
fn reassign_parses_where_egui_refuses() {
    // egui has no `:=` rule and errors here; v6 needs the node.
    let program = parse("x := 1");
    match &program.statements[0] {
        PineStmt::Reassign { name, value } => {
            assert_eq!(name, "x");
            assert!(matches!(value.as_ref(), PineExpr::Number(n) if *n == 1.0));
        }
        other => panic!("expected reassign, got {other:?}"),
    }
}

#[test]
fn if_else_blocks_hold_many_statements() {
    let program = parse("if a\n b\n c\nelse\n d");
    match &program.statements[0] {
        PineStmt::If {
            then_branch,
            else_branch,
            ..
        } => {
            assert_eq!(then_branch.len(), 2);
            assert_eq!(else_branch.as_ref().map(Vec::len), Some(1));
        }
        other => panic!("expected if, got {other:?}"),
    }
}

#[test]
fn destructure_and_tuples_parse() {
    let program = parse("[a, b] = foo()");
    match &program.statements[0] {
        PineStmt::DestructureAssign { targets, value } => {
            assert_eq!(targets, &vec!["a".to_owned(), "b".to_owned()]);
            assert!(matches!(value.as_ref(), PineExpr::FunctionCall { .. }));
        }
        other => panic!("expected destructure, got {other:?}"),
    }
    let program = parse("x = [a, 1]");
    match &program.statements[0] {
        PineStmt::Assignment { value, .. } => {
            assert!(matches!(value.as_ref(), PineExpr::ArrayLiteral(items) if items.len() == 2));
        }
        other => panic!("expected assignment, got {other:?}"),
    }
}

#[test]
fn switch_with_default_arm_parses() {
    let program = parse("x = switch y\n 1 => a\n => b");
    match &program.statements[0] {
        PineStmt::Assignment { value, .. } => match value.as_ref() {
            PineExpr::Switch(block) => {
                assert_eq!(block.arms.len(), 2);
                assert!(block.arms[0].pattern.is_some());
                assert!(block.arms[1].pattern.is_none());
            }
            other => panic!("expected switch, got {other:?}"),
        },
        other => panic!("expected assignment, got {other:?}"),
    }
}

#[test]
fn negative_arm_does_not_glue_to_previous_body() {
    // A `-` starting a line is a unary pattern, never a binary
    // continuation of the previous arm's body.
    let program = parse("x = switch y\n 1 => a\n -1 => b\n => c");
    match &program.statements[0] {
        PineStmt::Assignment { value, .. } => match value.as_ref() {
            PineExpr::Switch(block) => {
                assert_eq!(block.arms.len(), 3);
                assert!(matches!(
                    block.arms[0].body.as_slice(),
                    [PineStmt::Expression(PineExpr::Variable(name))] if name == "a"
                ));
                assert!(matches!(
                    block.arms[1].pattern,
                    Some(PineExpr::Unary { op: UnaryOp::Negate, .. })
                ));
            }
            other => panic!("expected switch, got {other:?}"),
        },
        other => panic!("expected assignment, got {other:?}"),
    }
}

#[test]
fn binary_continuation_needs_trailing_operator() {
    // Trailing operators continue; leading operators never glue.
    let program = parse("x = a +\n b * c");
    match &program.statements[0] {
        PineStmt::Assignment { value, .. } => match value.as_ref() {
            PineExpr::Binary { op, .. } => assert_eq!(*op, BinaryOp::Add),
            other => panic!("expected add, got {other:?}"),
        },
        other => panic!("expected assignment, got {other:?}"),
    }
    let program = parse("a\n[b]");
    assert_eq!(program.statements.len(), 2, "operand and tuple stay separate");
    assert!(matches!(
        &program.statements[1],
        PineStmt::Expression(PineExpr::ArrayLiteral(_))
    ));
}

#[test]
fn declarations_functions_and_types_parse() {
    let program = parse("export const int X = 1");
    assert!(matches!(
        &program.statements[0],
        PineStmt::ExportDecl(inner) if matches!(inner.as_ref(), PineStmt::ConstDecl { .. })
    ));
    let program = parse("export enum E\n A = \"a\"\n B = \"b\"");
    match &program.statements[0] {
        PineStmt::ExportDecl(inner) => match inner.as_ref() {
            PineStmt::EnumDecl { name, members } => {
                assert_eq!(name, "E");
                assert_eq!(members.len(), 2);
            }
            other => panic!("expected enum, got {other:?}"),
        },
        other => panic!("expected export, got {other:?}"),
    }
    let program = parse("export f(float x, simple int y=2) =>\n x + y");
    match &program.statements[0] {
        PineStmt::ExportDecl(inner) => match inner.as_ref() {
            PineStmt::FuncDecl { name, params, body } => {
                assert_eq!(name, "f");
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].type_name.as_ref().map(|t| t.name.as_str()), Some("float"));
                assert_eq!(params[1].qualifier.as_deref(), Some("simple"));
                assert!(params[1].default.is_some());
                assert_eq!(body.len(), 1);
            }
            other => panic!("expected func, got {other:?}"),
        },
        other => panic!("expected export, got {other:?}"),
    }
    let program = parse("export type T\n int a = na\n map<int, int> m = na");
    match &program.statements[0] {
        PineStmt::ExportDecl(inner) => match inner.as_ref() {
            PineStmt::TypeDecl { name, fields } => {
                assert_eq!(name, "T");
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[1].type_name.name, "map");
                assert_eq!(fields[1].type_name.args.len(), 2);
            }
            other => panic!("expected type, got {other:?}"),
        },
        other => panic!("expected export, got {other:?}"),
    }
    let program = parse("export method m(T this, int x) =>\n this");
    match &program.statements[0] {
        PineStmt::ExportDecl(inner) => match inner.as_ref() {
            PineStmt::MethodDecl { name, params, .. } => {
                assert_eq!(name, "m");
                assert_eq!(params.len(), 2);
            }
            other => panic!("expected method, got {other:?}"),
        },
        other => panic!("expected export, got {other:?}"),
    }
    let program = parse("library('L')");
    assert!(matches!(
        &program.statements[0],
        PineStmt::LibraryDecl { name } if name == "L"
    ));
    let program = parse("indicator(title=\"T\", overlay=true)");
    match &program.statements[0] {
        PineStmt::IndicatorDecl { args } => {
            assert_eq!(args.len(), 2);
            assert_eq!(args[0].name.as_deref(), Some("title"));
        }
        other => panic!("expected indicator, got {other:?}"),
    }
    let program = parse("import a/b/3 as B");
    match &program.statements[0] {
        PineStmt::Import(decl) => {
            assert_eq!(decl.alias, "B");
            assert_eq!(decl.library.version, 3);
        }
        other => panic!("expected import, got {other:?}"),
    }
}

// ---- Target-file goldens ----

fn summarize(program: &PineProgram) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "statements: {}", program.statements.len());
    for stmt in &program.statements {
        match stmt {
            PineStmt::Import(decl) => {
                let _ = writeln!(out, "import {} as {}", decl.library, decl.alias);
            }
            PineStmt::LibraryDecl { name } => {
                let _ = writeln!(out, "library {name}");
            }
            PineStmt::IndicatorDecl { args } => {
                let _ = writeln!(out, "indicator args={}", args.len());
            }
            PineStmt::ExportDecl(inner) => match inner.as_ref() {
                PineStmt::ConstDecl { type_name, name, .. } => {
                    let _ = writeln!(out, "export const {} {name}", type_name.name);
                }
                PineStmt::EnumDecl { name, members } => {
                    let _ = writeln!(out, "export enum {name} members={}", members.len());
                }
                PineStmt::FuncDecl { name, params, body } => {
                    let _ = writeln!(
                        out,
                        "export func {name} params={} body={}",
                        params.len(),
                        body.len()
                    );
                }
                PineStmt::MethodDecl { name, params, body } => {
                    let _ = writeln!(
                        out,
                        "export method {name} params={} body={}",
                        params.len(),
                        body.len()
                    );
                }
                PineStmt::TypeDecl { name, fields } => {
                    let _ = writeln!(out, "export type {name} fields={}", fields.len());
                }
                other => {
                    let _ = writeln!(out, "export other {other:?}");
                }
            },
            other => {
                let _ = writeln!(out, "other {}", stmt_kind(other));
            }
        }
    }
    out
}

fn stmt_kind(stmt: &PineStmt) -> &'static str {
    match stmt {
        PineStmt::Import(_) => "import",
        PineStmt::LibraryDecl { .. } => "library",
        PineStmt::IndicatorDecl { .. } => "indicator",
        PineStmt::ExportDecl(_) => "export",
        PineStmt::FuncDecl { .. } => "func",
        PineStmt::MethodDecl { .. } => "method",
        PineStmt::ConstDecl { .. } => "const",
        PineStmt::EnumDecl { .. } => "enum",
        PineStmt::TypeDecl { .. } => "type",
        PineStmt::VarDeclaration { .. } => "var",
        PineStmt::Declaration { .. } => "declaration",
        PineStmt::Assignment { .. } => "assignment",
        PineStmt::Reassign { .. } => "reassign",
        PineStmt::DestructureAssign { .. } => "destructure",
        PineStmt::Expression(_) => "expression",
        PineStmt::If { .. } => "if",
        PineStmt::For { .. } => "for",
        PineStmt::While { .. } => "while",
        PineStmt::Switch(_) => "switch",
    }
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("golden")
        .join(name)
}

fn assert_golden(source_file: &str, golden_file: &str) -> PineProgram {
    let program = load(source_file);
    let path = golden_path(golden_file);
    if std::env::var("UPDATE_PINE_SYNTAX_GOLDENS").is_ok() {
        std::fs::write(&path, summarize(&program)).expect("golden writable");
    }
    let frozen = std::fs::read_to_string(&path)
        .expect("golden frozen; rerun with UPDATE_PINE_SYNTAX_GOLDENS=1");
    assert_eq!(summarize(&program), frozen, "parse drift in {source_file}");
    program
}

fn count_switches(stmts: &[PineStmt]) -> usize {
    stmts
        .iter()
        .map(|stmt| match stmt {
            PineStmt::Switch(block) => {
                1 + block.arms.iter().map(|arm| count_switches(&arm.body)).sum::<usize>()
            }
            PineStmt::ExportDecl(inner) => count_switches(std::slice::from_ref(inner)),
            PineStmt::FuncDecl { body, .. }
            | PineStmt::MethodDecl { body, .. }
            | PineStmt::For { body, .. }
            | PineStmt::While { body, .. } => count_switches(body),
            PineStmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                count_switches(then_branch)
                    + else_branch.as_ref().map_or(0, |branch| count_switches(branch))
            }
            PineStmt::VarDeclaration { value, .. }
            | PineStmt::Declaration { value, .. }
            | PineStmt::Assignment { value, .. }
            | PineStmt::Reassign { value, .. }
            | PineStmt::DestructureAssign { value, .. }
            | PineStmt::ConstDecl { value, .. } => count_switch_expr(value),
            PineStmt::Expression(value) => count_switch_expr(value),
            _ => 0,
        })
        .sum()
}

fn count_switch_expr(expr: &PineExpr) -> usize {
    match expr {
        PineExpr::Switch(block) => {
            1 + block
                .arms
                .iter()
                .map(|arm| count_switches(&arm.body))
                .sum::<usize>()
        }
        PineExpr::Binary { left, right, .. } => count_switch_expr(left) + count_switch_expr(right),
        PineExpr::Unary { expr: inner, .. } => count_switch_expr(inner),
        PineExpr::Ternary {
            condition,
            then_expr,
            else_expr,
        } => count_switch_expr(condition) + count_switch_expr(then_expr) + count_switch_expr(else_expr),
        PineExpr::FunctionCall { args, .. } | PineExpr::MethodCall { args, .. } => {
            args.iter().map(|arg| count_switch_expr(&arg.value)).sum()
        }
        PineExpr::ArrayAccess { array, index } => count_switch_expr(array) + count_switch_expr(index),
        PineExpr::SeriesOffset { series, .. } => count_switch_expr(series),
        PineExpr::ArrayLiteral(items) => items.iter().map(count_switch_expr).sum(),
        PineExpr::Number(_)
        | PineExpr::StringLiteral(_)
        | PineExpr::Boolean(_)
        | PineExpr::Variable(_)
        | PineExpr::BuiltinVariable(_) => 0,
    }
}

fn every_stmt<'a>(stmts: &'a [PineStmt], out: &mut Vec<&'a PineStmt>) {
    for stmt in stmts {
        out.push(stmt);
        match stmt {
            PineStmt::ExportDecl(inner) => every_stmt(std::slice::from_ref(inner), out),
            PineStmt::FuncDecl { body, .. }
            | PineStmt::MethodDecl { body, .. }
            | PineStmt::For { body, .. }
            | PineStmt::While { body, .. } => {
                every_stmt(body, out);
            }
            PineStmt::Switch(block) => {
                for arm in &block.arms {
                    every_stmt(&arm.body, out);
                }
            }
            PineStmt::VarDeclaration { value, .. }
            | PineStmt::Declaration { value, .. }
            | PineStmt::Assignment { value, .. }
            | PineStmt::Reassign { value, .. }
            | PineStmt::DestructureAssign { value, .. }
            | PineStmt::ConstDecl { value, .. } => {
                switch_bodies(value, out);
            }
            PineStmt::Expression(value) => {
                switch_bodies(value, out);
            }
            PineStmt::If {
                then_branch,
                else_branch,
                ..
            } => {
                every_stmt(then_branch, out);
                if let Some(branch) = else_branch {
                    every_stmt(branch, out);
                }
            }
            _ => {}
        }
    }
}

fn switch_bodies<'a>(expr: &'a PineExpr, out: &mut Vec<&'a PineStmt>) {
    match expr {
        PineExpr::Switch(block) => {
            for arm in &block.arms {
                every_stmt(&arm.body, out);
            }
        }
        PineExpr::Binary { left, right, .. } => {
            switch_bodies(left, out);
            switch_bodies(right, out);
        }
        PineExpr::Unary { expr: inner, .. } => switch_bodies(inner, out),
        PineExpr::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            switch_bodies(condition, out);
            switch_bodies(then_expr, out);
            switch_bodies(else_expr, out);
        }
        PineExpr::FunctionCall { args, .. } | PineExpr::MethodCall { args, .. } => {
            for arg in args {
                switch_bodies(&arg.value, out);
            }
        }
        PineExpr::ArrayAccess { array, index } => {
            switch_bodies(array, out);
            switch_bodies(index, out);
        }
        PineExpr::SeriesOffset { series, .. } => switch_bodies(series, out),
        PineExpr::ArrayLiteral(items) => {
            for item in items {
                switch_bodies(item, out);
            }
        }
        PineExpr::Number(_)
        | PineExpr::StringLiteral(_)
        | PineExpr::Boolean(_)
        | PineExpr::Variable(_)
        | PineExpr::BuiltinVariable(_) => {}
    }
}

fn every_expr<'a>(stmts: &'a [PineStmt], out: &mut Vec<&'a PineExpr>) {
    for stmt in stmts {
        match stmt {
            PineStmt::VarDeclaration { value, .. }
            | PineStmt::Declaration { value, .. }
            | PineStmt::Assignment { value, .. }
            | PineStmt::Reassign { value, .. }
            | PineStmt::DestructureAssign { value, .. }
            | PineStmt::ConstDecl { value, .. } => {
                out.push(value);
                expr_children(value, out);
            }
            PineStmt::Expression(value) => {
                out.push(value);
                expr_children(value, out);
            }
            PineStmt::ExportDecl(inner) => every_expr(std::slice::from_ref(inner), out),
            PineStmt::FuncDecl { params, body, .. }
            | PineStmt::MethodDecl { params, body, .. } => {
                for param in params {
                    if let Some(default) = &param.default {
                        out.push(default);
                        expr_children(default, out);
                    }
                }
                every_expr(body, out);
            }
            PineStmt::EnumDecl { members, .. } => {
                for member in members {
                    out.push(&member.value);
                }
            }
            PineStmt::TypeDecl { fields, .. } => {
                for field in fields {
                    if let Some(default) = &field.default {
                        out.push(default);
                    }
                }
            }
            PineStmt::Switch(block) => {
                for arm in &block.arms {
                    if let Some(pattern) = &arm.pattern {
                        out.push(pattern);
                    }
                    every_expr(&arm.body, out);
                }
            }
            PineStmt::If {
                condition,
                then_branch,
                else_branch,
            } => {
                out.push(condition);
                expr_children(condition, out);
                every_expr(then_branch, out);
                if let Some(branch) = else_branch {
                    every_expr(branch, out);
                }
            }
            PineStmt::For { start, end, body, .. } => {
                out.push(start);
                out.push(end);
                every_expr(body, out);
            }
            PineStmt::While { condition, body } => {
                out.push(condition);
                every_expr(body, out);
            }
            PineStmt::Import(_)
            | PineStmt::LibraryDecl { .. }
            | PineStmt::IndicatorDecl { .. } => {}
        }
    }
}

fn expr_children<'a>(expr: &'a PineExpr, out: &mut Vec<&'a PineExpr>) {
    match expr {
        PineExpr::Binary { left, right, .. } => {
            out.push(left);
            out.push(right);
            expr_children(left, out);
            expr_children(right, out);
        }
        PineExpr::Unary { expr: inner, .. } => {
            out.push(inner);
            expr_children(inner, out);
        }
        PineExpr::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            out.push(condition);
            out.push(then_expr);
            out.push(else_expr);
            expr_children(condition, out);
            expr_children(then_expr, out);
            expr_children(else_expr, out);
        }
        PineExpr::FunctionCall { args, .. } | PineExpr::MethodCall { args, .. } => {
            for arg in args {
                out.push(&arg.value);
                expr_children(&arg.value, out);
            }
        }
        PineExpr::ArrayAccess { array, index } => {
            out.push(array);
            out.push(index);
            expr_children(array, out);
            expr_children(index, out);
        }
        PineExpr::SeriesOffset { series, .. } => {
            out.push(series);
            expr_children(series, out);
        }
        PineExpr::ArrayLiteral(items) => {
            for item in items {
                out.push(item);
                expr_children(item, out);
            }
        }
        PineExpr::Switch(block) => {
            for arm in &block.arms {
                if let Some(pattern) = &arm.pattern {
                    out.push(pattern);
                }
                every_expr(&arm.body, out);
            }
        }
        PineExpr::Number(_)
        | PineExpr::StringLiteral(_)
        | PineExpr::Boolean(_)
        | PineExpr::Variable(_)
        | PineExpr::BuiltinVariable(_) => {}
    }
}

#[test]
fn golden_lib_util_structure() {
    let program = assert_golden("libraries/_libUtil.pine", "parse_libUtil.txt");
    let mut stmts = Vec::new();
    every_stmt(&program.statements, &mut stmts);
    let enums: Vec<&PineStmt> = stmts
        .iter()
        .filter(|stmt| matches!(stmt, PineStmt::EnumDecl { .. }))
        .copied()
        .collect();
    assert_eq!(enums.len(), 12, "libUtil enum count");
    let names: Vec<&str> = enums
        .iter()
        .map(|stmt| match stmt {
            PineStmt::EnumDecl { name, .. } => name.as_str(),
            _ => unreachable!(),
        })
        .collect();
    for name in [
        "Timeframes",
        "MarketRegimes",
        "Smoothings",
        "Oscillators",
        "PatternTypes",
        "CandlePatterns",
        "Sizes",
        "LineStyles",
        "Positions",
        "LineType",
        "ExtensionType",
        "Pivots",
    ] {
        assert!(names.contains(&name), "enum {name} present");
    }
    // Every switch in the library parses (statement and expression forms).
    assert!(count_switches(&program.statements) >= 10, "switch census");
    // The Scheduler type with its two fields and four methods.
    let mut types = stmts.iter().filter_map(|stmt| match stmt {
        PineStmt::TypeDecl { name, fields } => Some((name.as_str(), fields.len())),
        _ => None,
    });
    assert_eq!(
        types.next(),
        Some(("Scheduler", 2)),
        "Scheduler type with two fields"
    );
    let methods: Vec<&str> = stmts
        .iter()
        .filter_map(|stmt| match stmt {
            PineStmt::MethodDecl { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    for name in ["init", "register", "updateEarliestDue", "every"] {
        assert!(methods.contains(&name), "Scheduler method {name} present");
    }
}

#[test]
fn golden_lib_mas_structure() {
    let program = assert_golden("libraries/_libMAs.pine", "parse_libMAs.txt");
    let mut stmts = Vec::new();
    every_stmt(&program.statements, &mut stmts);
    // The 11-arm factorySmoothing switch plus its default arm.
    let factory = stmts
        .iter()
        .find_map(|stmt| match stmt {
            PineStmt::FuncDecl { name, body, .. } if name == "factorySmoothing" => Some(body),
            _ => None,
        })
        .expect("factorySmoothing present");
    let mut switches = Vec::new();
    every_stmt(factory, &mut switches);
    let arms: Vec<usize> = switches
        .iter()
        .filter_map(|stmt| match stmt {
            PineStmt::Switch(block) => Some(block.arms.len()),
            PineStmt::Expression(PineExpr::Switch(block)) => Some(block.arms.len()),
            _ => None,
        })
        .collect();
    assert!(
        arms.contains(&12),
        "11 arms plus default, got {arms:?}"
    );
    // Tuple returns and tuple destructuring across the library.
    let mut exprs = Vec::new();
    every_expr(&program.statements, &mut exprs);
    let tuples = exprs
        .iter()
        .filter(|expr| matches!(expr, PineExpr::ArrayLiteral(items) if items.len() >= 2))
        .count();
    assert!(tuples >= 5, "tuple returns, got {tuples}");
    let destructures = stmts
        .iter()
        .filter(|stmt| matches!(stmt, PineStmt::DestructureAssign { .. }))
        .count();
    // Line 259 is the only `[...] =` destructuring; line 97's
    // `[superTrend, _direction]` is a bare tuple return, counted above.
    assert_eq!(destructures, 1, "tuple destructurings");
}

#[test]
fn golden_fast_mas_structure() {
    let program = assert_golden("indicators/_FastMAs.pine", "parse_FastMAs.txt");
    let imports: Vec<&PineStmt> = program
        .statements
        .iter()
        .filter(|stmt| matches!(stmt, PineStmt::Import(_)))
        .collect();
    assert_eq!(imports.len(), 2, "two imports");
    let mut exprs = Vec::new();
    every_expr(&program.statements, &mut exprs);
    let calls = |namespace: &str, method: &str| {
        exprs
            .iter()
            .filter(|expr| {
                matches!(
                    expr,
                    PineExpr::MethodCall { namespace: ns, method: m, .. }
                    if ns == namespace && m == method
                )
            })
            .count()
    };
    let inputs = calls("input", "bool")
        + calls("input", "int")
        + calls("input", "float")
        + calls("input", "string")
        + calls("input", "enum")
        + calls("input", "source")
        + exprs
            .iter()
            .filter(|expr| {
                matches!(
                    expr,
                    PineExpr::FunctionCall { name, .. } if name == "input"
                )
            })
            .count();
    assert!(inputs >= 15, "input.* calls, got {inputs}");
    assert_eq!(calls("libMAs", "factorySmoothing"), 6, "factory calls");
    assert_eq!(calls("ta", "sma"), 3, "ta.sma calls");
    let plots = exprs
        .iter()
        .filter(|expr| {
            matches!(
                expr,
                PineExpr::FunctionCall { name, .. } if name == "plot"
            )
        })
        .count();
    assert_eq!(plots, 9, "plot calls");
}
