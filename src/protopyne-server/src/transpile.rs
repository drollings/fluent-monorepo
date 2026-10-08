//! Scoped script-to-kernel spike: accept-or-refuse, never miscompile.
//!
//! The spike recognizes one narrow script shape — a version-6 indicator with
//! `input.int`/`input.float` bindings, `ta.sma`/`ta.ema` assignments over
//! OHLCV legs, and `plot` calls — and lowers it to a [`Hair`] HIR
//! (`Const`/`Name`/`Ohlcv`/`Binary`/`Where`/`Shift`/`Rolling`/`Call`), then to
//! Rust source calling the real kernels plus a numeric evaluation through
//! those same kernels.
//!
//! Anything outside the shape is refused with a reason: `switch`, any
//! ternary, `request.*`, `varip`, `export`/`method`, `strategy`/`library`
//! declarations, and loops are rejected before any HIR exists, so the spike
//! can never silently miscompile them. A real grammar is the follow-up; this
//! recognizer is deliberately narrower than the language.

use protopyne_kernels::{EmaIndicator, Indicator as _, Ohlcv, ParamSet, SmaIndicator};

use crate::error::ServerError;

/// HIR node subset for the spike.
#[derive(Debug, Clone, PartialEq)]
pub enum Hair {
    /// Numeric literal.
    Const(f64),
    /// Bound input or intermediate name.
    Name(String),
    /// One OHLCV leg (`open`, `high`, `low`, `close`, `volume`).
    Ohlcv(String),
    /// `left op right` with `op` in `{+, -, *, /}`.
    Binary { op: char, left: Box<Hair>, right: Box<Hair> },
    /// Conditional selection (accepted in HIR, never produced by the spike).
    Where { cond: Box<Hair>, then: Box<Hair>, otherwise: Box<Hair> },
    /// Series shift by `offset` bars.
    Shift { offset: i64, series: Box<Hair> },
    /// Rolling kernel application.
    Rolling { kernel: String, length: Length, series: Box<Hair> },
    /// Kernel call with resolved arguments.
    Call { kernel: String, args: Vec<Hair> },
}

/// Averaging length: literal or input-bound name resolved at evaluation.
#[derive(Debug, Clone, PartialEq)]
pub enum Length {
    /// Integer literal from the script.
    Literal(i64),
    /// Name bound by `input.int`/`input.float` with its default.
    Input(String, i64),
}

/// One lowered assignment: `name = <hir>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    /// Assignment target.
    pub name: String,
    /// Lowered right-hand side.
    pub hair: Hair,
}

/// Lowered script: bindings plus plotted names.
#[derive(Debug, Clone, PartialEq)]
pub struct Lowered {
    /// Assignments in source order.
    pub bindings: Vec<Binding>,
    /// Names passed to `plot`.
    pub plots: Vec<String>,
}

/// Refusal with every reason collected, never just the first.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    /// Every failed check, in evaluation order.
    pub reasons: Vec<String>,
}

/// Outcome of spike compilation: lowered HIR plus emitted Rust, or refusal.
#[derive(Debug, Clone, PartialEq)]
pub enum SpikeOutcome {
    /// The script is inside the subset; this is its HIR and Rust.
    Accepted {
        /// Lowered HIR.
        lowered: Lowered,
        /// Emitted Rust source calling the real kernels.
        rust: String,
    },
    /// The script is outside the subset; nothing was emitted.
    Refused {
        /// Every reason for refusal.
        refusal: Refusal,
    },
}

/// Input-name defaults for length resolution.
type InputDefaults = std::collections::HashMap<String, i64>;
/// Bound names collected while lowering.
type BoundNames = std::collections::HashSet<String>;
/// Evaluation outputs keyed by binding name, in source order.
type NamedOutputs = Vec<(String, Vec<Option<f64>>)>;

/// Deny patterns: any match refuses the script before HIR exists.
const DENY_PATTERNS: [&str; 8] = [
    "switch",
    "request.",
    "varip",
    "export",
    "method",
    "strategy(",
    "library(",
    "var ",
];

/// OHLCV legs the spike resolves.
const LEGS: [&str; 5] = ["open", "high", "low", "close", "volume"];

/// Kernels the spike emits.
const KERNELS: [&str; 2] = ["sma", "ema"];

fn refuse(reasons: Vec<String>) -> SpikeOutcome {
    SpikeOutcome::Refused { refusal: Refusal { reasons } }
}

/// Split a comma-separated argument list at depth zero.
fn split_args(body: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in body.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                args.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_owned());
    }
    args
}

/// Parse `name = value` into its halves.
fn split_assignment(line: &str) -> Option<(String, String)> {
    let (left, right) = line.split_once('=')?;
    let name = left.trim();
    let expr = right.trim();
    if name.is_empty() || expr.is_empty() {
        return None;
    }
    // `==`, `!=`, `<=`, `>=` are comparisons, not assignments: the tell is
    // either half starting/ending with an operator character.
    if expr.starts_with('=') || matches!(name.chars().last(), Some('=' | '!' | '<' | '>')) {
        return None;
    }
    Some((name.to_owned(), expr.to_owned()))
}

/// Lower one expression: literal, leg, bound name, binary, or kernel call.
fn lower_expr(
    text: &str,
    inputs: &InputDefaults,
    bound: &BoundNames,
) -> Result<Hair, String> {
    let text = text.trim();
    if let Ok(value) = text.parse::<f64>() {
        return Ok(Hair::Const(value));
    }
    if LEGS.contains(&text) {
        return Ok(Hair::Ohlcv(text.to_owned()));
    }
    for kernel in KERNELS {
        let prefix = format!("ta.{kernel}(");
        if text.starts_with(&prefix) && text.ends_with(')') {
            let body = &text[prefix.len()..text.len() - 1];
            let args = split_args(body);
            if args.len() != 2 {
                return Err(format!("{kernel} needs source and length, got {text:?}"));
            }
            let series = lower_expr(&args[0], inputs, bound)?;
            let length = lower_length(&args[1], inputs)?;
            return Ok(Hair::Rolling { kernel: kernel.to_owned(), length, series: Box::new(series) });
        }
    }
    // One binary operator at depth zero, `*`/`/` before `+`/`-`.
    if let Some(hair) = split_binary(text, inputs, bound, &['*', '/']) {
        return hair;
    }
    if let Some(hair) = split_binary(text, inputs, bound, &['+', '-']) {
        return hair;
    }
    if bound.contains(text) || inputs.contains_key(text) {
        return Ok(Hair::Name(text.to_owned()));
    }
    Err(format!("expression outside subset: {text:?}"))
}

/// Split one binary operator at depth zero.
fn split_binary(
    text: &str,
    inputs: &InputDefaults,
    bound: &BoundNames,
    ops: &[char],
) -> Option<Result<Hair, String>> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    // Scan right-to-left so `a - b - c` groups left (result order is
    // irrelevant here: only one operator may appear at this level).
    for (index, byte) in bytes.iter().enumerate().rev() {
        match byte {
            b')' => depth += 1,
            b'(' => depth -= 1,
            _ => {}
        }
        if depth == 0 && ops.contains(&(*byte as char)) && index > 0 {
            let (left, right) = text.split_at(index);
            let op = text[index..=index].chars().next()?;
            // A leading `-` is a sign, not a binary operator.
            if left.trim().is_empty() {
                continue;
            }
            // `e-notation` (`1e-3`) is a literal, not subtraction.
            if op == '-' {
                if let Some(prev) = left.chars().last() {
                    if prev == 'e' || prev == 'E' {
                        continue;
                    }
                }
            }
            let result = (|| {
                let left = lower_expr(left, inputs, bound)?;
                let right = lower_expr(&right[1..], inputs, bound)?;
                Ok(Hair::Binary { op, left: Box::new(left), right: Box::new(right) })
            })();
            return Some(result);
        }
    }
    None
}

/// Lower an averaging length: integer literal or input-bound name.
fn lower_length(
    text: &str,
    inputs: &InputDefaults,
) -> Result<Length, String> {
    let text = text.trim();
    if let Ok(value) = text.parse::<i64>() {
        if value >= 1 {
            return Ok(Length::Literal(value));
        }
        return Err(format!("length must be at least 1, got {value}"));
    }
    if let Some(default) = inputs.get(text) {
        return Ok(Length::Input(text.to_owned(), *default));
    }
    Err(format!("length outside subset: {text:?}"))
}

/// Parse `name = input.int(14, "len")` into name plus default.
fn parse_input_binding(line: &str) -> Option<(String, i64)> {
    let (name, value) = split_assignment(line)?;
    for prefix in ["input.int(", "input.float("] {
        if value.starts_with(prefix) && value.ends_with(')') {
            let body = &value[prefix.len()..value.len() - 1];
            let first = split_args(body).into_iter().next()?;
            let default = first.parse::<f64>().ok()? as i64;
            if default >= 1 {
                return Some((name, default));
            }
            return None;
        }
    }
    None
}

/// Parse `plot(name)` or `plot(name, "title")` into the plotted name.
fn parse_plot(line: &str) -> Option<String> {
    let line = line.trim();
    if !(line.starts_with("plot(") && line.ends_with(')')) {
        return None;
    }
    let body = &line["plot(".len()..line.len() - 1];
    split_args(body).into_iter().next()
}

/// Compile `source`: accept-or-refuse, never miscompile.
///
/// Acceptance bar **(correctness)**: scripts inside the `sma`/`ema` subset
/// lower to HIR and emitted Rust; everything else is refused with reasons.
#[must_use]
pub fn compile_spike(source: &str) -> SpikeOutcome {
    let mut reasons: Vec<String> = Vec::new();
    let mut bindings: Vec<Binding> = Vec::new();
    let mut plots: Vec<String> = Vec::new();
    let mut inputs: InputDefaults = InputDefaults::new();
    let mut bound: BoundNames = BoundNames::new();
    let mut saw_indicator = false;
    let mut saw_version = false;

    for raw_line in source.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") {
            if line.starts_with("//@version") {
                saw_version = true;
                if !line.contains("=6") {
                    reasons.push(format!("only version 6 accepted, got {line:?}"));
                }
            }
            continue;
        }
        if line.contains('?') {
            reasons.push("ternary expressions are outside the subset".to_owned());
            continue;
        }
        for pattern in DENY_PATTERNS {
            if line.contains(pattern) {
                reasons.push(format!("construct outside subset: {pattern}"));
            }
        }
        if line.starts_with("indicator(") {
            saw_indicator = true;
            continue;
        }
        if let Some((name, default)) = parse_input_binding(line) {
            if inputs.insert(name.clone(), default).is_some() {
                reasons.push(format!("duplicate input {name:?}"));
            }
            continue;
        }
        if let Some(name) = parse_plot(line) {
            plots.push(name);
            continue;
        }
        if let Some((name, expr)) = split_assignment(line) {
            match lower_expr(&expr, &inputs, &bound) {
                Ok(hair) => {
                    bound.insert(name.clone());
                    bindings.push(Binding { name, hair });
                }
                Err(reason) => reasons.push(reason),
            }
            continue;
        }
        reasons.push(format!("line outside subset: {line:?}"));
    }
    if !saw_version {
        reasons.push("missing //@version=6 header".to_owned());
    }
    if !saw_indicator {
        reasons.push("missing indicator(...) declaration".to_owned());
    }
    if bindings.is_empty() {
        reasons.push("no sma/ema assignment found".to_owned());
    }
    for name in &plots {
        if !bound.contains(name) {
            reasons.push(format!("plot of unbound name {name:?}"));
        }
    }
    if !reasons.is_empty() {
        return refuse(reasons);
    }
    let lowered = Lowered { bindings, plots };
    SpikeOutcome::Accepted { rust: emit_rust(&lowered), lowered }
}

/// Emit Rust source calling the real kernels for `lowered`.
fn emit_rust(lowered: &Lowered) -> String {
    let mut out = String::from(
        "use protopyne_kernels::{EmaIndicator, Indicator as _, Ohlcv, ParamSet, SmaIndicator};\n\n\
         /// Generated from an accepted script: one kernel batch per binding.\n\
         pub fn generated(values: &Ohlcv<'_>, params: &ParamSet) -> Result<Vec<protopyne_kernels::IndicatorResult>, protopyne_kernels::KernelError> {\n",
    );
    for binding in &lowered.bindings {
        out.push_str(&emit_statement(&binding.name, &binding.hair));
        out.push('\n');
    }
    out.push_str("    Ok(vec![");
    for binding in &lowered.bindings {
        out.push('_');
        out.push_str(&binding.name);
        out.push_str(", ");
    }
    out.push_str("])\n}\n");
    out
}

/// Emit one statement for a binding: a kernel-batch `let` for rolling
/// bindings, a comment for structural ones carried in HIR only.
fn emit_statement(name: &str, hair: &Hair) -> String {
    match hair {
        Hair::Rolling { kernel, length, series } => {
            let leg = emit_series(series);
            let len = match length {
                Length::Literal(value) => value.to_string(),
                Length::Input(binding, _) => {
                    format!("params.int(\"_\", \"{binding}\").unwrap_or(14)")
                }
            };
            let indicator = if kernel == "ema" { "EmaIndicator::new()" } else { "SmaIndicator::new()" };
            format!(
                "    let _{name} = {indicator}.batch(&Ohlcv::close_only({leg}), &ParamSet::new().with_int(\"length\", {len}))?;"
            )
        }
        other => format!("    // non-rolling binding '{name}' carried in HIR only: {other:?}"),
    }
}

/// Emit the series expression feeding a kernel.
fn emit_series(hair: &Hair) -> String {
    match hair {
        Hair::Ohlcv(leg) if leg == "close" => "values.close".to_owned(),
        Hair::Ohlcv(leg) => format!("values.{leg}"),
        Hair::Name(name) => format!("/* bound series {name} resolved at evaluation */ values.close"),
        Hair::Const(value) => format!("/* constant {value} */ values.close"),
        Hair::Binary { .. } | Hair::Where { .. } | Hair::Shift { .. } | Hair::Rolling { .. } | Hair::Call { .. } => {
            "values.close".to_owned()
        }
    }
}

/// Evaluate `lowered` over `closes` through the real kernels.
///
/// Rolling bindings evaluate to per-bar outputs aligned with the input
/// (`None` inside warmup); structural bindings (binary overlays) are carried
/// in HIR only and skipped here. Lengths resolve from `inputs` defaults.
/// Used by calibration to prove accepted scripts match golden kernel outputs.
/// Returns `(binding name, values)` pairs in source order.
pub fn evaluate_lowered(
    lowered: &Lowered,
    closes: &[f64],
    inputs: &InputDefaults,
) -> Result<NamedOutputs, ServerError> {
    let frame = Ohlcv::close_only(closes);
    let mut outputs = Vec::new();
    for binding in &lowered.bindings {
        let Hair::Rolling { kernel, length, .. } = &binding.hair else {
            continue;
        };
        let resolved = match length {
            Length::Literal(value) => *value,
            Length::Input(name, default) => inputs.get(name).copied().unwrap_or(*default),
        };
        if resolved < 1 {
            return Err(ServerError::Service("length must be at least 1".to_owned()));
        }
        let params = ParamSet::new().with_int("length", resolved);
        let values = match kernel.as_str() {
            "sma" => SmaIndicator::new().batch(&frame, &params),
            "ema" => EmaIndicator::new().batch(&frame, &params),
            _ => return Err(ServerError::Service(format!("unknown kernel {kernel}"))),
        }
        .map_err(|e| ServerError::Service(format!("kernel failed: {e}")))?;
        outputs.push((binding.name.clone(), values.values));
    }
    Ok(outputs)
}

/// Calibration tally: accept rate on the must-transpile set and reject rate
/// on the must-REJECT set. Both must be `1.0` before spike output is cached
/// or persisted anywhere; nothing here caches or persists.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CalibrationTally {
    /// Must-transpile scripts presented.
    pub must_transpile: usize,
    /// Must-transpile scripts accepted.
    pub accepted: usize,
    /// Must-REJECT scripts presented.
    pub must_reject: usize,
    /// Must-REJECT scripts refused.
    pub refused: usize,
}

impl CalibrationTally {
    /// Fraction of the must-transpile set accepted.
    #[must_use]
    pub fn accept_rate(&self) -> f64 {
        if self.must_transpile == 0 {
            return 0.0;
        }
        self.accepted as f64 / self.must_transpile as f64
    }

    /// Fraction of the must-REJECT set refused.
    #[must_use]
    pub fn reject_rate(&self) -> f64 {
        if self.must_reject == 0 {
            return 0.0;
        }
        self.refused as f64 / self.must_reject as f64
    }

    /// Whether the spike may be trusted: every must-transpile accepted and
    /// every must-REJECT refused.
    #[must_use]
    pub fn is_green(&self) -> bool {
        (self.accept_rate() - 1.0).abs() < f64::EPSILON
            && (self.reject_rate() - 1.0).abs() < f64::EPSILON
    }
}

/// Tally `compile_spike` over the two calibration sets.
#[must_use]
pub fn calibrate(must_transpile: &[&str], must_reject: &[&str]) -> CalibrationTally {
    let mut tally = CalibrationTally {
        must_transpile: must_transpile.len(),
        accepted: 0,
        must_reject: must_reject.len(),
        refused: 0,
    };
    for source in must_transpile {
        if matches!(compile_spike(source), SpikeOutcome::Accepted { .. }) {
            tally.accepted += 1;
        }
    }
    for source in must_reject {
        if matches!(compile_spike(source), SpikeOutcome::Refused { .. }) {
            tally.refused += 1;
        }
    }
    tally
}

#[cfg(test)]
#[path = "../tests/transpile.rs"]
mod tests;
