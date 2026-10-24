//! Scoped script-to-kernel spike: accept-or-refuse, never miscompile.
//!
//! The spike recognizes one narrow script shape — a version-6 indicator with
//! `input.int`/`input.float` bindings, `ta.sma`/`ta.ema`/`ta.trima`/
//! `ta.midpoint` assignments over OHLCV legs, and `plot` calls — and lowers
//! it to a [`Hair`] HIR (`Const`/`Name`/`Ohlcv`/`Binary`/`Where`/`Shift`/
//! `Rolling`/`Call`), then to Rust source calling the real kernels plus a
//! numeric evaluation through those same kernels.
//!
//! Anything outside the shape is refused with a reason: `switch`, any
//! ternary, `request.*`, `varip`, `export`/`method`, `strategy`/`library`
//! declarations, and loops are rejected before any HIR exists, so the spike
//! can never silently miscompile them. A real grammar is the follow-up; this
//! recognizer is deliberately narrower than the language.

use protopyne_kernels::{
    EmaIndicator, Indicator as _, MidpointIndicator, Ohlcv, ParamSet, SmaIndicator, TrimaIndicator,
};

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
    Binary {
        op: char,
        left: Box<Hair>,
        right: Box<Hair>,
    },
    /// Conditional selection (accepted in HIR, never produced by the spike).
    Where {
        cond: Box<Hair>,
        then: Box<Hair>,
        otherwise: Box<Hair>,
    },
    /// Series shift by `offset` bars.
    Shift { offset: i64, series: Box<Hair> },
    /// Rolling kernel application.
    Rolling {
        kernel: String,
        length: Length,
        series: Box<Hair>,
    },
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

/// Kernels the spike emits: the two rolling averages plus the verified
/// single-output overlaps. Multi-leg kernels (`macd`: value/signal/hist) do
/// not fit single-series binding evaluation, nullary price kernels sit
/// outside the two-argument call grammar, and `midprice` needs high/low legs
/// together where a binding carries one series — all three stay refused.
const KERNELS: [&str; 4] = ["sma", "ema", "trima", "midpoint"];

fn refuse(reasons: Vec<String>) -> SpikeOutcome {
    SpikeOutcome::Refused {
        refusal: Refusal { reasons },
    }
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
fn lower_expr(text: &str, inputs: &InputDefaults, bound: &BoundNames) -> Result<Hair, String> {
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
            return Ok(Hair::Rolling {
                kernel: kernel.to_owned(),
                length,
                series: Box::new(series),
            });
        }
    }
    // One binary operator at depth zero, resolved in a single scan:
    // `+`/`-` bind loosest, `*`/`/` tightest, left-associative.
    if let Some(hair) = split_binary(text, inputs, bound) {
        return hair;
    }
    if bound.contains(text) || inputs.contains_key(text) {
        return Ok(Hair::Name(text.to_owned()));
    }
    Err(format!("expression outside subset: {text:?}"))
}

/// Split one binary operator at depth zero in a single scan.
///
/// The pass tracks the rightmost lowest-precedence operator (`+`/`-` bind
/// loosest, `*`/`/` tightest), so `2 * 3 + 4` groups as `(2 * 3) + 4` and
/// `100 / 10 / 2` groups left-associatively. A leading `-` is a sign and
/// `e-notation` (`1e-3`) is a literal, never subtraction. Unbalanced
/// parentheses or an operator without operands refuses instead of guessing.
fn split_binary(
    text: &str,
    inputs: &InputDefaults,
    bound: &BoundNames,
) -> Option<Result<Hair, String>> {
    /// Binding looseness: lower splits first.
    fn looseness(op: u8) -> Option<u8> {
        match op {
            b'+' | b'-' => Some(0),
            b'*' | b'/' => Some(1),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut best: Option<(u8, usize)> = None;
    // Right-to-left so the first candidate at each level is the rightmost,
    // which is what left-associativity splits on.
    for (index, byte) in bytes.iter().enumerate().rev() {
        match byte {
            b')' => depth += 1,
            b'(' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return Some(Err(format!("unbalanced parentheses in {text:?}")));
        }
        if depth != 0 || index == 0 {
            continue;
        }
        let Some(rank) = looseness(*byte) else {
            continue;
        };
        // `e-notation` (`1e-3`) is a literal, not subtraction.
        if *byte == b'-' && matches!(bytes[index - 1], b'e' | b'E') {
            continue;
        }
        let tighter = match best {
            None => true,
            Some((best_rank, _)) => rank < best_rank,
        };
        if tighter {
            best = Some((rank, index));
        }
    }
    if depth != 0 {
        return Some(Err(format!("unbalanced parentheses in {text:?}")));
    }
    let (_, at) = best?;
    let (left, right) = text.split_at(at);
    let op = text[at..=at].chars().next()?;
    if left.trim().is_empty() || right[1..].trim().is_empty() {
        return Some(Err(format!("operator without operands in {text:?}")));
    }
    let result = (|| {
        let left = lower_expr(left, inputs, bound)?;
        let right = lower_expr(&right[1..], inputs, bound)?;
        Ok(Hair::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        })
    })();
    Some(result)
}

/// Lower an averaging length: integer literal or input-bound name.
fn lower_length(text: &str, inputs: &InputDefaults) -> Result<Length, String> {
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
            let default = first.parse::<f64>().ok()?;
            // A fractional default is refused, never truncated: lengths are
            // whole bars, and silent truncation would miscompile the intent.
            if default.fract() == 0.0 && default >= 1.0 {
                return Some((name, default as i64));
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
/// Acceptance bar **(task-value: grammar coverage, not confidence)**: scripts
/// inside the `sma`/`ema`/`trima`/`midpoint` subset lower to HIR and schematic
/// Rust; everything else is refused with reasons. "Compiles" here means the emission
/// syn-parses plus accepted scripts match kernel goldens numerically —
/// `rustc` compilation is never attempted, because tests stay hermetic (no
/// toolchain dependency) and the emission is schematic by design
/// (non-rolling bindings are comments; input accessors name their kernel and
/// input). Promoting the bar to `rustc`-verified emission would mean
/// compiling the emitted code against the real kernels in a sandbox, which is
/// explicitly out of scope for this spike.
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
            if let Some(number) = line.strip_prefix("//@version") {
                saw_version = true;
                let number = number.trim().trim_start_matches('=').trim();
                // Numeric parse, not substring: `=60` merely starts with 6.
                if number.parse::<u32>().ok() != Some(6) {
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
        reasons.push("no accepted-kernel assignment found".to_owned());
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
    match emit_rust(&lowered) {
        Ok(rust) => SpikeOutcome::Accepted { lowered, rust },
        Err(reason) => refuse(vec![reason]),
    }
}

/// Emit Rust source calling the real kernels for `lowered`, or the reason a
/// series cannot be emitted honestly.
fn emit_rust(lowered: &Lowered) -> Result<String, String> {
    let mut out = String::from(
        "use protopyne_kernels::{EmaIndicator, Indicator as _, MidpointIndicator, Ohlcv, ParamSet, SmaIndicator, TrimaIndicator};\n\n\
         /// Generated from an accepted script: one kernel batch per binding.\n\
         pub fn generated(values: &Ohlcv<'_>, params: &ParamSet) -> Result<Vec<protopyne_kernels::IndicatorResult>, protopyne_kernels::KernelError> {\n",
    );
    for binding in &lowered.bindings {
        out.push_str(&emit_statement(&binding.name, &binding.hair)?);
        out.push('\n');
    }
    out.push_str("    Ok(vec![");
    for binding in &lowered.bindings {
        out.push('_');
        out.push_str(&binding.name);
        out.push_str(", ");
    }
    out.push_str("])\n}\n");
    Ok(out)
}

/// Emit one statement for a binding: a kernel-batch `let` for rolling
/// bindings, a comment for structural ones carried in HIR only. Refuses when
/// the kernel series cannot be emitted honestly.
fn emit_statement(name: &str, hair: &Hair) -> Result<String, String> {
    match hair {
        Hair::Rolling {
            kernel,
            length,
            series,
        } => {
            let leg = emit_series(series)?;
            let len = match length {
                Length::Literal(value) => value.to_string(),
                Length::Input(binding, _) => {
                    format!("params.int(\"{kernel}\", \"{binding}\").unwrap_or(14)")
                }
            };
            let indicator = match kernel.as_str() {
                "ema" => "EmaIndicator::new()",
                "trima" => "TrimaIndicator::new()",
                "midpoint" => "MidpointIndicator::new()",
                _ => "SmaIndicator::new()",
            };
            Ok(format!(
                "    let _{name} = {indicator}.batch(&Ohlcv::close_only({leg}), &ParamSet::new().with_int(\"length\", {len}))?;"
            ))
        }
        other => Ok(format!(
            "    // non-rolling binding '{name}' carried in HIR only: {other:?}"
        )),
    }
}

/// Emit the series expression feeding a kernel. A plain leg emits against
/// the frame the generated code receives; anything else refuses — a bound
/// name would need binding resolution the spike does not do, and
/// substituting `values.close` would silently compute the wrong indicator.
fn emit_series(hair: &Hair) -> Result<String, String> {
    match hair {
        Hair::Ohlcv(leg) => Ok(format!("values.{leg}")),
        Hair::Name(name) => Err(format!(
            "bound-name series {name:?} outside the emission subset: binding resolution refused, never substituted"
        )),
        Hair::Const(value) => Err(format!(
            "constant series {value} outside the emission subset: refused, never substituted"
        )),
        unsupported => Err(format!(
            "{} series outside the emission subset: refused, never substituted",
            hair_kind(unsupported)
        )),
    }
}

/// Short kind name for a refusal reason.
fn hair_kind(hair: &Hair) -> &'static str {
    match hair {
        Hair::Const(_) => "constant",
        Hair::Name(_) => "bound-name",
        Hair::Ohlcv(_) => "leg",
        Hair::Binary { .. } => "binary",
        Hair::Where { .. } => "conditional",
        Hair::Shift { .. } => "shift",
        Hair::Rolling { .. } => "rolling",
        Hair::Call { .. } => "call",
    }
}

/// Evaluate `lowered` over `frame` through the real kernels.
///
/// Each rolling binding resolves its own series leg from the frame, so
/// `ta.sma(open, 10)` averages opens: the single-output kernels run over a
/// close-only view of that leg's values. Lengths resolve from `inputs`
/// defaults. Rolling bindings evaluate to per-bar outputs aligned with the
/// input (`None` inside warmup); structural bindings (binary overlays) are
/// carried in HIR only and skipped here. A rolling binding over anything but
/// a frame leg refuses — evaluating it over closes would silently compute
/// the wrong indicator. Returns `(binding name, values)` pairs in source
/// order.
pub fn evaluate_lowered(
    lowered: &Lowered,
    frame: &Ohlcv<'_>,
    inputs: &InputDefaults,
) -> Result<NamedOutputs, ServerError> {
    let mut outputs = Vec::new();
    for binding in &lowered.bindings {
        let Hair::Rolling {
            kernel,
            length,
            series,
        } = &binding.hair
        else {
            continue;
        };
        let leg = match series.as_ref() {
            Hair::Ohlcv(leg) => leg,
            other => {
                return Err(ServerError::Service(format!(
                    "cannot evaluate {} series: only frame legs resolve ({other:?} given)",
                    hair_kind(other),
                )));
            }
        };
        let values = match leg.as_str() {
            "open" => frame.open,
            "high" => frame.high,
            "low" => frame.low,
            "close" => frame.close,
            "volume" => frame.volume,
            unknown => {
                return Err(ServerError::Service(format!(
                    "unknown series leg {unknown:?}: only open/high/low/close/volume resolve"
                )));
            }
        };
        let leg_frame = Ohlcv::close_only(values);
        let resolved = match length {
            Length::Literal(value) => *value,
            // A name with no supplied value refuses: substituting the
            // parse-time default would silently evaluate a script the caller
            // never configured.
            Length::Input(name, _) => match inputs.get(name) {
                Some(value) => *value,
                None => {
                    return Err(ServerError::Service(format!(
                        "unknown input {name:?}: no value supplied"
                    )));
                }
            },
        };
        if resolved < 1 {
            return Err(ServerError::Service("length must be at least 1".to_owned()));
        }
        let params = ParamSet::new().with_int("length", resolved);
        let values = match kernel.as_str() {
            "sma" => SmaIndicator::new().batch(&leg_frame, &params),
            "ema" => EmaIndicator::new().batch(&leg_frame, &params),
            "trima" => TrimaIndicator::new().batch(&leg_frame, &params),
            "midpoint" => MidpointIndicator::new().batch(&leg_frame, &params),
            _ => return Err(ServerError::Service(format!("unknown kernel {kernel}"))),
        }
        .map_err(|e| ServerError::Service(format!("kernel failed: {e}")))?;
        outputs.push((binding.name.clone(), values.values));
    }
    Ok(outputs)
}

/// Calibration tally: accept rate on the must-transpile set and reject rate
/// on the must-REJECT set. Both must be `1.0` before spike output is cached
/// or persisted anywhere; nothing here caches or persists. The rates measure
/// grammar coverage against control sets (task-value), and acceptance itself
/// is parse-plus-numeric, never `rustc` (see `compile_spike`).
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
