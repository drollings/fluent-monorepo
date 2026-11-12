//! Shared evaluation leaves both engines call.
//!
//! Arithmetic and casting helpers, branch truthiness, whole-number rules,
//! `fixnan`, tape and injected-request helpers, [`Legs`], and every shared
//! refusal constructor live here — one home so the tree interpreter and the
//! compiled loop can never drift apart.

use std::collections::HashMap;

use protopyne_types::Bar;
use protopyne_types::EpochMs;

use super::engine::{LtfDataset, MAX_CALL_DEPTH, MAX_LOOP_ITERATIONS};
use super::syntax::{BinaryOp, BuiltinVar, CallArg, PineExpr, PineStmt, SwitchStmt, UnaryOp};
use crate::context::RunCtx;
use crate::error::KernelError;
use crate::nan_hole;
use crate::value::PineValue;

/// Float equality tolerance, mirroring the reference comparator exactly.
const EPSILON: f64 = f64::EPSILON;

/// Concrete `f64` legs for one run, converted once up front (same
/// decimal-to-float reading as the kernel batch path).
#[cfg_attr(test, allow(dead_code))]
/// Concrete `f64` legs for one run, converted once up front (same
/// decimal-to-float reading as the kernel batch path).
pub(crate) struct Legs {
    open: Vec<f64>,
    high: Vec<f64>,
    low: Vec<f64>,
    close: Vec<f64>,
    volume: Vec<f64>,
    /// Millisecond stamps in tick mode (seconds otherwise); rewritten by
    /// the runner, never by user code.
    pub(crate) time: Vec<f64>,
    hl2: Vec<f64>,
    hlc3: Vec<f64>,
    ohlc4: Vec<f64>,
    wcl: Vec<f64>,
}

impl Legs {
    pub(crate) fn of(bars: &[Bar]) -> Self {
        let open: Vec<f64> = bars
            .iter()
            .map(|b| protopyne_types::bar_leg_as_f64(b.o))
            .collect();
        let high: Vec<f64> = bars
            .iter()
            .map(|b| protopyne_types::bar_leg_as_f64(b.h))
            .collect();
        let low: Vec<f64> = bars
            .iter()
            .map(|b| protopyne_types::bar_leg_as_f64(b.l))
            .collect();
        let close: Vec<f64> = bars
            .iter()
            .map(|b| protopyne_types::bar_leg_as_f64(b.c))
            .collect();
        let volume: Vec<f64> = bars.iter().map(|b| b.volume as f64).collect();
        let time: Vec<f64> = bars.iter().map(|b| b.ts.timestamp() as f64).collect();
        let tick_at = |i: usize| crate::indicator::BarTick {
            open: open[i],
            high: high[i],
            low: low[i],
            close: close[i],
            volume: volume[i],
        };
        let col = |source: crate::price_source::PriceSource| {
            (0..bars.len())
                .map(|i| source.compute_bar(tick_at(i)))
                .collect()
        };
        let hl2 = col(crate::price_source::PriceSource::Hl2);
        let hlc3 = col(crate::price_source::PriceSource::Hlc3);
        let ohlc4 = col(crate::price_source::PriceSource::Ohlc4);
        let wcl = col(crate::price_source::PriceSource::Wcl);
        Self {
            open,
            high,
            low,
            close,
            volume,
            time,
            hl2,
            hlc3,
            ohlc4,
            wcl,
        }
    }

    /// Borrow one named leg (`open/high/low/close/volume/time/hl2/hlc3/ohlc4/wcl`).
    pub(crate) fn named(&self, name: &str) -> Option<&[f64]> {
        match name {
            "open" => Some(&self.open),
            "high" => Some(&self.high),
            "low" => Some(&self.low),
            "close" => Some(&self.close),
            "volume" => Some(&self.volume),
            "time" => Some(&self.time),
            "hl2" => Some(&self.hl2),
            "hlc3" => Some(&self.hlc3),
            "ohlc4" => Some(&self.ohlc4),
            "wcl" => Some(&self.wcl),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn named_for_test(&self, name: &str) -> Option<&[f64]> {
        self.named(name)
    }
}

#[cfg(test)]
pub(crate) fn legs_of_for_test(bars: &[Bar]) -> Legs {
    Legs::of(bars)
}

/// One refusal with a reason, fail-closed instead of miscompiled.
pub(crate) fn refused(message: impl Into<String>) -> KernelError {
    KernelError::Eval(message.into())
}

/// History-edge refusals both evaluators share: one wording per edge,
/// one constructor per wording. The edge table `offset_name` and the
/// loop's `history_read` both implement (representations differ per
/// engine; the refusal text never does):
///
/// 1. Negative offsets refuse, naming the offset (the loop renders its
///    `i32`, the tree its `i64` — same digits, same text).
/// 2. Leg reads serve the window instead (`na` past the left edge —
///    no refusal on either engine).
/// 3. Array reads past the end refuse, naming the index.
/// 4. Reads of never-recorded names refuse, naming the variable (the
///    loop renders its link-assigned column positionally when the
///    column carries no name).
/// 5. `back == 0` is the fast path: the current cell, never a refusal.
pub(crate) fn history_negative_offset(offset: impl std::fmt::Display) -> KernelError {
    refused(format!(
        "negative history offset {offset} is not a series read"
    ))
}

/// See [`history_negative_offset`] for the shared edge table.
pub(crate) fn array_index_out_of_bounds(index: usize) -> KernelError {
    refused(format!("array index {index} out of bounds"))
}

/// Value-position array index below zero: both evaluators refuse naming
/// the index, mirroring the history-edge rule above.
pub(crate) fn array_negative_index(index: impl std::fmt::Display) -> KernelError {
    refused(format!("negative array index {index}"))
}

/// See [`history_negative_offset`] for the shared edge table.
pub(crate) fn undefined_variable(name: &str) -> KernelError {
    refused(format!("undefined variable '{name}'"))
}

/// Tuple-unpack arity: both evaluators length-check before extracting,
/// so a short or long subject refuses naming both counts.
pub(crate) fn destructure_arity_mismatch(got: usize, want: usize) -> KernelError {
    refused(format!(
        "destructuring arity {got} does not match {want} targets"
    ))
}

/// Tuple-unpack subject kind: absence and non-arrays refuse naming what
/// arrived, mirroring the tree-walk's value-position checks.
pub(crate) fn destructure_non_array(value: Option<&PineValue>) -> KernelError {
    match value {
        None => refused("destructuring binds array values, got na"),
        Some(other) => refused(format!("destructuring binds array values, got {other}")),
    }
}

/// Rebind target with no binding: the tree refuses naming the target,
/// and the loop's journal resolves homes at run time with the identical
/// wording instead of failing link.
pub(crate) fn reassign_undeclared(name: &str) -> KernelError {
    refused(format!("':=' targets undeclared variable '{name}'"))
}

/// Whether a series name is engine-internal history storage rather than
/// user-visible state: history over a parenthesized expression evaluates
/// once per bar into a `\0`-prefixed name no Pine identifier can spell,
/// and snapshots on both engines omit these names so outcomes stay
/// user-visible-identical while histories still advance through them.
pub(crate) fn is_hidden_series(name: &str) -> bool {
    name.starts_with('\0')
}

/// Untitled-capture fallback both engines share: `{family}:{count}` at
/// first sight — the run-global counters on each side count the same
/// sights (the title fallback counts first sights, mirroring the
/// run-global counters on the loop side), so both name an untitled
/// call identically. The loop resolves a static title from its
/// link-time constants, the tree from the call's evaluated arguments;
/// only the fallback spelling is shared.
pub(crate) fn capture_fallback_title(family: &str, count: usize) -> String {
    format!("{family}:{count}")
}

/// Fill pairer refusals both engines share: a leg variable names the
/// variable when no plot tag was recorded for it, and a plot site names
/// itself when no capture was recorded this bar. The loop resolves tags
/// through its link-time columns, the tree through its plot-variable
/// map; per-engine capture stores keep their representations, but the
/// reasons never differ.
pub(crate) fn fill_variable_without_plot_tag(var: &str) -> KernelError {
    refused(format!(
        "fill names variable '{var}' with no plot tag this run"
    ))
}

/// Field-read refusals both engines share: the receiver evaluates once,
/// then an object reads its table, anything else refuses naming what
/// arrived, and absence refuses on its own. The loop resolves the
/// receiver through its frame slot, the tree through its namespace;
/// per-engine value stores keep their representations, but the reasons
/// never differ.
pub(crate) fn unknown_field_on(field: &str, type_name: &str) -> KernelError {
    refused(format!("unknown field '{field}' on '{type_name}'"))
}

/// See [`unknown_field_on`] for the shared field rules.
pub(crate) fn field_needs_object(field: &str, other: &PineValue) -> KernelError {
    refused(format!(
        "field '{field}' needs an object receiver, got {other}"
    ))
}

/// See [`unknown_field_on`] for the shared field rules.
pub(crate) fn field_read_on_na(field: &str) -> KernelError {
    refused(format!("field '{field}' read on na"))
}

/// Dotted-write refusals both engines share: the receiver takes out of
/// its home, and a missing field refuses naming the full path after
/// restoring the instance — exactly the tree-walk's take/put order.
pub(crate) fn unknown_dotted_field(head: &str, field: &str) -> KernelError {
    refused(format!("unknown field '{head}.{field}'"))
}

/// Method-dispatch refusals both engines share: the receiver evaluates
/// once, then a rvalue call dispatches on its runtime type while a
/// variable-held call takes the instance out of its home first. Unknown
/// types and methods refuse naming both; non-object receivers refuse
/// naming what arrived; missing arguments name the parameter. The loop
/// dispatches through its link-time method index, the tree through its
/// method registry; per-engine frames keep their representations, but
/// the reasons never differ.
pub(crate) fn unknown_method_on(type_name: &str, method: &str) -> KernelError {
    refused(format!("unknown method '{type_name}.{method}'"))
}

/// See [`unknown_method_on`] for the shared dispatch rules.
pub(crate) fn method_needs_object(method: &str, other: &PineValue) -> KernelError {
    refused(format!(
        "method '{method}' needs an object receiver, got {other}"
    ))
}

/// See [`unknown_method_on`] for the shared dispatch rules.
pub(crate) fn method_called_on_na(method: &str) -> KernelError {
    refused(format!("method '{method}' called on na"))
}

/// See [`unknown_method_on`] for the shared dispatch rules.
pub(crate) fn method_arg_missing(param: &str) -> KernelError {
    refused(format!("missing argument '{param}'"))
}

/// See [`unknown_method_on`] for the shared dispatch rules.
pub(crate) fn method_too_many_positionals() -> KernelError {
    refused(method_too_many_positionals_message())
}

/// Message form for compile-time (link-time) refusals that must fire only
/// when reached at run time; see [`unknown_function_message`].
#[must_use]
/// Message form for compile-time (link-time) refusals that must fire only
/// when reached at run time; see [`unknown_function_message`].
pub(crate) fn method_too_many_positionals_message() -> String {
    "too many positional arguments".to_owned()
}

/// See [`unknown_method_on`] for the shared dispatch rules.
pub(crate) fn method_unknown_argument(given: &str) -> KernelError {
    refused(method_unknown_argument_message(given))
}

/// Message form for compile-time (link-time) refusals that must fire only
/// when reached at run time; see [`unknown_function_message`].
#[must_use]
/// Message form for compile-time (link-time) refusals that must fire only
/// when reached at run time; see [`unknown_function_message`].
pub(crate) fn method_unknown_argument_message(given: &str) -> String {
    format!("unknown argument '{given}'")
}

/// See [`unknown_method_on`] for the shared dispatch rules.
pub(crate) fn this_unbound() -> KernelError {
    refused("'this' is only bound inside a method")
}

/// Receiver field paths refuse naming the full spelling, whether the
/// read hits the live slot or the write misses its field: the tree
/// serves `this.*` from the innermost slot and the loop mirrors it
/// exactly.
pub(crate) fn unknown_this_field(field: &str) -> KernelError {
    refused(format!("unknown field 'this.{field}'"))
}

/// Rebinding the receiver itself refuses: methods mutate through
/// fields, never by replacing the binding.
pub(crate) fn cannot_rebind_this() -> KernelError {
    refused("cannot rebind 'this'")
}

/// Bare calls name their callee on the way out: both engines refuse an
/// unknown name with this exact wording, the loop through
/// `Instruction::Refuse`.
pub(crate) fn unknown_function_message(name: &str) -> String {
    format!("unknown function '{name}'")
}

/// See [`unknown_function_message`].
pub(crate) fn unknown_function(name: &str) -> KernelError {
    refused(unknown_function_message(name))
}

/// Arity refusals both engines raise at the same program point (after the
/// arguments evaluated), so the loop can carry them as a typed refusal
/// instead of evicting the module.
pub(crate) const NA_ARITY_MESSAGE: &str = "'na' takes zero or one argument";
/// See [`NA_ARITY_MESSAGE`].
pub(crate) const NZ_ARITY_MESSAGE: &str = "'nz' takes one or two arguments";
/// A bare `input` without a default never declares anything.
pub(crate) const INPUT_MISSING_DEFAULT_MESSAGE: &str = "'input' takes a default value";
/// A `plot` without a series draws nothing.
pub(crate) const PLOT_MISSING_SERIES_MESSAGE: &str = "'plot' takes a series to draw";

/// See [`fill_variable_without_plot_tag`] for the shared pairing rules.
pub(crate) fn fill_plot_site_without_capture(site_key: &str) -> KernelError {
    refused(format!(
        "fill names a plot site with no capture this bar: {site_key}"
    ))
}

/// Loop-bound refusals both evaluators share: the `for`-range trip cap,
/// the `for ... in` shape and length caps, the `while` trip cap, and the
/// call-depth cap refuse with one wording each, whether the bound trips
/// on the tree-walk or in the loop. All six are refusal-cold (they
/// allocate only on abort); single-sourcing the templates keeps the
/// wordings from drifting.
pub(crate) fn for_range_over_limit(first: i64, last: i64) -> KernelError {
    refused(format!(
        "'for' range {first}..={last} exceeds {MAX_LOOP_ITERATIONS} iterations"
    ))
}

/// See [`for_range_over_limit`] for the shared bound rules.
pub(crate) fn for_step_non_positive(step: i64) -> KernelError {
    refused(format!("'for ... by' needs a positive step, got {step}"))
}

/// See [`for_range_over_limit`] for the shared bound rules.
pub(crate) fn for_in_not_array(other: &PineValue) -> KernelError {
    refused(format!("'for ... in' iterates arrays, got {other}"))
}

/// See [`for_range_over_limit`] for the shared bound rules.
pub(crate) fn for_in_absent() -> KernelError {
    refused("'for ... in' iterates arrays, got na")
}

/// See [`for_range_over_limit`] for the shared bound rules.
pub(crate) fn for_in_over_limit(len: usize) -> KernelError {
    refused(format!(
        "'for ... in' over {len} elements exceeds {MAX_LOOP_ITERATIONS} iterations"
    ))
}

/// See [`for_range_over_limit`] for the shared bound rules.
pub(crate) fn while_over_limit() -> KernelError {
    refused(format!("'while' exceeded {MAX_LOOP_ITERATIONS} iterations"))
}

/// See [`for_range_over_limit`] for the shared bound rules.
pub(crate) fn call_depth_over_limit() -> KernelError {
    refused(format!("call depth exceeds {MAX_CALL_DEPTH}"))
}

/// Collection calls whose return is the mutated collection: statement (and
/// trailing-value) position persists the return to a variable target,
/// giving Pine reference-type behavior. Read-only calls are excluded on
/// purpose — persisting e.g. a `size` answer would overwrite the
/// collection with one number.
pub(crate) fn writes_back_collection(namespace: &str, method: &str) -> bool {
    match namespace {
        // `shift` persists the shrunken array while yielding the removed
        // head; in statement position the yielded value is dropped, so
        // the call's stored value is the collection itself.
        "array" => matches!(
            method,
            "set" | "push" | "unshift" | "fill" | "clear" | "sort" | "reverse" | "shift" | "pop"
        ),
        "matrix" => matches!(method, "set"),
        // `remove` persists the shrunken map while yielding the removed
        // value; statement position drops that value, same as `shift`.
        "map" => matches!(method, "put" | "clear" | "remove"),
        _ => false,
    }
}

/// Shared `fixnan` step: keep the last present value per call-site
/// shape, replay it through gaps. Both engines key on the same
/// argument-shape string through this one function — the stores stay
/// per-run on each side (tree state vs loop state), never a second
/// memory within either engine.
pub(crate) fn fixnan_call(
    memory: &mut HashMap<String, Option<PineValue>>,
    site: &str,
    values: &[Option<PineValue>],
) -> Result<Option<PineValue>, KernelError> {
    if values.len() != 1 {
        return Err(refused("'fixnan' takes exactly one argument"));
    }
    let current = values[0].clone().filter(|v| !is_absent_value(v));
    if let Some(present) = current {
        memory.insert(site.to_owned(), Some(present.clone()));
        Ok(Some(present))
    } else {
        Ok(memory.get(site).cloned().unwrap_or(None))
    }
}

/// Session-time read shared by both engines: the chart-period gate,
/// the session-string rules, and the bar-time value are one function —
/// the tree-walk reads the leg itself, the loop passes its own leg
/// read, and every refusal names the same rule either way.
pub(crate) fn session_time_of(
    values: &[Option<PineValue>],
    chart_period: &str,
    time_value: Option<PineValue>,
) -> Result<Option<PineValue>, KernelError> {
    let period = match values.first() {
        Some(Some(PineValue::Str(period))) => period,
        Some(None) => return Ok(None),
        Some(Some(other)) => {
            return Err(refused(format!(
                "time takes a timeframe string, got {other}"
            )));
        }
        _ => return Err(refused("time takes a timeframe and an optional session")),
    };
    if period != chart_period {
        return Err(refused(format!(
            "time supports only the chart period '{chart_period}' headlessly, got {period:?}; inject that timeframe's bars as the chart run instead"
        )));
    }
    if values.len() > 2 {
        return Err(refused(
            "time timezone mapping is not supported headlessly; pass chart-period bars already selected for the session",
        ));
    }
    match values.get(1) {
        Some(Some(PineValue::Str(session))) => {
            if session.trim().is_empty() {
                return Err(refused("time takes a non-empty session string"));
            }
            Ok(time_value)
        }
        Some(None) => Ok(None),
        Some(Some(other)) => Err(refused(format!("time takes a session string, got {other}"))),
        None => Ok(time_value),
    }
}

/// Wall-clock parts for session math: `hour(t)`/`minute(t)` in UTC, or
/// in Eastern with an explicit `"America/New_York"` zone (the single
/// mapped zone — histories are US equities). Absent input reads absent;
/// non-numeric stamps and unmapped zones refuse naming the row.
/// Shared by both engines: the loop composes this directly, so the
/// parts and every refusal agree by construction.
pub(crate) fn call_wall_part(
    values: &[Option<PineValue>],
    part: u8,
) -> Result<Option<PineValue>, KernelError> {
    let stamp = match values.first() {
        Some(Some(PineValue::Float(v))) => *v,
        Some(Some(PineValue::Int(v))) => *v as f64,
        Some(None) => return Ok(None),
        _ => {
            return Err(refused(format!(
                "{} takes a timestamp, got {:?}",
                if part == 0 { "hour" } else { "minute" },
                values.first()
            )));
        }
    };
    if !stamp.is_finite() {
        return Ok(None);
    }
    let offset_secs: i64 = match values.get(1) {
        None | Some(None) => 0,
        Some(Some(PineValue::Str(zone))) if zone == "America/New_York" => {
            i64::from(eastern_offset_secs(stamp as i64))
        }
        Some(Some(other)) => {
            return Err(refused(format!(
                "wall-clock zones outside America/New_York are not mapped headlessly, got {other}"
            )));
        }
    };
    let wall = stamp as i64 + offset_secs;
    let value = if part == 0 {
        wall.div_euclid(3600).rem_euclid(24)
    } else {
        wall.div_euclid(60).rem_euclid(60)
    };
    Ok(Some(PineValue::Int(value)))
}

/// Trailing segment of a (possibly module-qualified) type name:
/// method registration and receiver checks compare `ManagedTable`, never
/// the `libApex.ManagedTable` spelling the declaration used. Shared with
/// the loop's method index so both engines key the same type.
pub(crate) fn trailing_type(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// One confirmed `request.security` window over the registered dataset: the
/// display tail the request returns plus the full confirmed prefix its kernels
/// must compute over. Both are views, never copies.
#[derive(Clone, Copy)]
/// One confirmed `request.security` window over the registered dataset: the
/// display tail the request returns plus the full confirmed prefix its kernels
/// must compute over. Both are views, never copies.
pub(crate) struct SecurityTail<'a> {
    dataset: &'a LtfDataset,
    skip: usize,
    end: usize,
    pub(crate) display_count: Option<usize>,
}

impl<'a> SecurityTail<'a> {
    /// Borrow the served display window `(skip, end)` of one dataset.
    pub(crate) fn of(
        dataset: &'a LtfDataset,
        (skip, end): (usize, usize),
        display_count: Option<usize>,
    ) -> Self {
        Self {
            dataset,
            skip,
            end,
            display_count,
        }
    }

    /// The display window's stamps, oldest first.
    fn times(&self) -> &'a [EpochMs] {
        &self.dataset.times[self.skip..self.end]
    }

    /// Borrow one leg column of the display window by chart-leg name.
    pub(crate) fn column(&self, name: &str) -> Option<&'a [f64]> {
        let column = match name {
            "open" => &self.dataset.opens,
            "high" => &self.dataset.highs,
            "low" => &self.dataset.lows,
            "close" => &self.dataset.closes,
            "volume" => &self.dataset.volumes,
            _ => return None,
        };
        Some(&column[self.skip..self.end])
    }

    /// The same window widened back to the dataset's first confirmed row:
    /// kernels batch over this, never over the display tail, so a recursive
    /// arm seeds where the series starts and `calc_bars_count` stays a bound on
    /// returned rows.
    pub(crate) fn history(&self) -> Self {
        Self { skip: 0, ..*self }
    }

    /// Own one view as whole-series kernel input.
    pub(crate) fn ohlcv(&self) -> crate::indicator::OwnedOhlcv {
        crate::indicator::OwnedOhlcv {
            open: self.dataset.opens[self.skip..self.end].to_vec(),
            high: self.dataset.highs[self.skip..self.end].to_vec(),
            low: self.dataset.lows[self.skip..self.end].to_vec(),
            close: self.dataset.closes[self.skip..self.end].to_vec(),
            volume: self.dataset.volumes[self.skip..self.end].to_vec(),
        }
    }
}

/// Last confirmed value of one leg: floats map non-finite to absence,
/// `time` reads integer milliseconds like the pull registry's column.
pub(crate) fn security_leg(tail: &SecurityTail, name: &str) -> Option<PineValue> {
    if name == "time" {
        return tail
            .times()
            .last()
            .copied()
            .map(|stamp| PineValue::Int(stamp.get()));
    }
    tail.column(name)
        .and_then(|column| column.last().copied())
        .and_then(crate::indicator::fin_or_none)
        .map(PineValue::Float)
}

/// Shared tail of every live-`request.*` refusal: the fetch alternative is
/// harness injection, never a network call. The head names the method.
pub(crate) const LIVE_REQUEST_TAIL: &str = "is refused headlessly; inject multi-timeframe bars and tick data through the run instead of a network fetch";

/// Boolean positions take `Bool` only and pass `na` through as "no branch":
/// `None` means the condition is absent (callers skip), numbers still
/// refuse — the reference truthiness is a correction, not a copy.
pub(crate) fn branch_of(
    what: &str,
    value: Option<&PineValue>,
) -> Result<Option<bool>, KernelError> {
    match value {
        Some(PineValue::Bool(flag)) => Ok(Some(*flag)),
        None => Ok(None),
        Some(other) => Err(refused(format!("'{what}' needs a bool, got {other}"))),
    }
}

/// `switch` arm matching: first-match wins across numbers (with the
/// comparator tolerance), strings, bools, and enum members. Absence never
/// matches, and mismatched kinds never match — unmatched switches yield
/// `na` through the default arm or the empty result.
pub(crate) fn switch_eq(first: Option<&PineValue>, second: Option<&PineValue>) -> bool {
    match (first, second) {
        (Some(PineValue::Int(a)), Some(PineValue::Int(b))) => a == b,
        (Some(PineValue::Float(a)), Some(PineValue::Float(b))) => (a - b).abs() < EPSILON,
        (Some(a), Some(b))
            if matches!(a, PineValue::Float(_) | PineValue::Int(_))
                && matches!(b, PineValue::Float(_) | PineValue::Int(_)) =>
        {
            match (a.as_float(), b.as_float()) {
                (Some(x), Some(y)) => (x - y).abs() < EPSILON,
                _ => false,
            }
        }
        (Some(PineValue::Str(a)), Some(PineValue::Str(b))) => a == b,
        (Some(PineValue::Bool(a)), Some(PineValue::Bool(b))) => a == b,
        (Some(PineValue::EnumHandle(a)), Some(PineValue::EnumHandle(b))) => {
            a.enum_name == b.enum_name && a.member == b.member
        }
        _ => false,
    }
}

/// Three-valued `and`/`or` for one absent side (both-present pairs keep the
/// strict bool table in [`binary`]): a failing `and` annihilates to `false`,
/// a passing `or` annihilates to `true`, everything else stays `na`.
/// Non-bool operands refuse — numbers are never truthy, even beside `na`.
pub(crate) fn kleene_absent(
    op: BinaryOp,
    left: Option<&PineValue>,
    right: Option<&PineValue>,
) -> Result<Option<PineValue>, KernelError> {
    let mut decided: Option<bool> = None;
    for side in [left, right] {
        match side {
            Some(PineValue::Bool(flag)) => {
                let annihilates =
                    matches!(op, BinaryOp::And) && !flag || matches!(op, BinaryOp::Or) && *flag;
                if annihilates {
                    decided = Some(*flag);
                }
            }
            Some(other) => {
                return Err(refused(format!("operator '{op}' needs bools, got {other}")));
            }
            None => {}
        }
    }
    Ok(decided.map(PineValue::Bool))
}

/// `for` bounds are whole numbers: `int`s, or integral floats. Fractional
/// bounds refuse (the reference truncation is not copied).
pub(crate) fn whole_number(what: &str, value: Option<&PineValue>) -> Result<i64, KernelError> {
    match value {
        Some(PineValue::Int(v)) => Ok(*v),
        // Integral and range-checked above the cast, so the conversion below
        // is exact; fractional or out-of-range floats refuse instead.
        #[allow(clippy::cast_possible_truncation)]
        Some(PineValue::Float(f))
            if f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 =>
        {
            Ok(*f as i64)
        }
        None => Err(refused(format!("'{what}' is na"))),
        Some(other) => Err(refused(format!("'{what}' needs an int, got {other}"))),
    }
}

/// Whole-number cast: truncation toward zero, absence stays absence;/// non-finite and out-of-range floats refuse, anything non-numeric refuses.
pub(crate) fn int_cast(value: Option<&PineValue>) -> Result<Option<PineValue>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::Int(v)) => Ok(Some(PineValue::Int(*v))),
        Some(PineValue::Float(v)) if v.is_nan() => Ok(None),
        Some(PineValue::Float(v)) if v.is_finite() => {
            // `2^63` is exactly representable; anything at or past it (or
            // at/below `-2^63`) cannot land in an `i64`.
            const LIMIT: f64 = 9_223_372_036_854_776_576.0;
            let truncated = v.trunc();
            if (-LIMIT..LIMIT).contains(&truncated) {
                #[allow(clippy::cast_possible_truncation)]
                Ok(Some(PineValue::Int(truncated as i64)))
            } else {
                Err(refused(format!("'int' overflows on {v}")))
            }
        }
        Some(other) => Err(refused(format!("'int' takes a number, got {other}"))),
    }
}

/// Float cast: integers convert, floats pass through, absence stays
/// absence; anything non-numeric refuses instead of inventing a value.
pub(crate) fn float_cast(value: Option<&PineValue>) -> Result<Option<PineValue>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::Int(v)) => Ok(Some(PineValue::Float(*v as f64))),
        Some(PineValue::Float(v)) if v.is_nan() => Ok(None),
        Some(PineValue::Float(v)) => Ok(Some(PineValue::Float(*v))),
        Some(other) => Err(refused(format!("'float' takes a number, got {other}"))),
    }
}

/// Bare `color(...)` cast: a single color passes through, absence stays
/// absence; anything else refuses naming what color takes. Shared with
/// the bytecode loop so both engines refuse identically; the loop is the
/// only caller today, hence the gate (the single copy stays here).
#[cfg(feature = "bytecode")]
/// Bare `color(...)` cast: a single color passes through, absence stays
/// absence; anything else refuses naming what color takes. Shared with
/// the bytecode loop so both engines refuse identically; the loop is the
/// only caller today, hence the gate (the single copy stays here).
pub(crate) fn color_cast(value: Option<&PineValue>) -> Result<Option<PineValue>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::ColorHandle(_)) => Ok(value.cloned()),
        Some(other) => Err(refused(format!("'color' takes a color, got {other}"))),
    }
}

/// Legacy `iff(condition, then, else)`: the taken branch value passes
/// through, an absent condition reads absent, and a non-bool condition
/// refuses naming the call. All three arguments evaluate before the
/// choice (it is a call, not an operator). Shared with the bytecode loop
/// so both engines choose identically; the single copy stays here.
pub(crate) fn iff_of(values: &[Option<PineValue>]) -> Result<Option<PineValue>, KernelError> {
    let [condition, then_branch, else_branch] = values else {
        return Err(refused(format!(
            "'iff' takes a condition and two branches, got {} argument(s)",
            values.len()
        )));
    };
    match condition {
        Some(PineValue::Bool(true)) => Ok(then_branch.clone()),
        Some(PineValue::Bool(false)) => Ok(else_branch.clone()),
        None => Ok(None),
        Some(other) => Err(refused(format!("'iff' takes a bool condition, got {other}"))),
    }
}

/// Legacy bare `tr`: true range at the cursor through the shared
/// kernel helper (high-low on the first bar, the three-way max after).
/// Non-finite legs read absent; the loop serves the same value through
/// its own instruction, so values agree by construction.
pub(crate) fn true_range_at(legs: &Legs, cursor: usize) -> Option<PineValue> {
    let finite = |leg: Option<&[f64]>| {
        leg.and_then(|values| values.get(cursor).copied())
            .and_then(crate::indicator::fin_or_none)
    };
    let high = finite(legs.named("high"))?;
    let low = finite(legs.named("low"))?;
    let prev = cursor
        .checked_sub(1)
        .and_then(|back| legs.named("close").and_then(|values| values.get(back).copied()))
        .and_then(crate::indicator::fin_or_none);
    Some(PineValue::Float(crate::volatility::true_range(
        high, low, prev,
    )))
}

/// Absence on legs and in memory: `None`, or a `NaN` float.
pub(crate) fn is_absent(value: Option<&PineValue>) -> bool {    match value {
        None => true,
        Some(PineValue::Float(v)) => v.is_nan(),
        Some(_) => false,
    }
}

/// Absence inside a present slot (a `NaN` float).
fn is_absent_value(value: &PineValue) -> bool {
    matches!(value, PineValue::Float(v) if v.is_nan())
}

/// US/Eastern UTC offset in seconds for a timestamp: -4h inside daylight
/// time (second Sunday of March 07:00 UTC through first Sunday of November
/// 06:00 UTC), -5h otherwise. The harness maps exactly one zone; every
/// other spelling refuses at the call site.
fn eastern_offset_secs(ts_secs: i64) -> i32 {
    use chrono::{Datelike, NaiveDate};
    let date = chrono::DateTime::from_timestamp(ts_secs, 0).map_or_else(
        || NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch date"),
        |dt| dt.naive_utc().date(),
    );
    let year = date.year();
    let sunday_in = |month: u32, ordinal: u32| -> Option<NaiveDate> {
        let first = NaiveDate::from_ymd_opt(year, month, 1)?;
        let until_sunday = (7 - first.weekday().num_days_from_sunday()) % 7;
        NaiveDate::from_ymd_opt(year, month, 1 + until_sunday + (ordinal - 1) * 7)
    };
    let dst_start = sunday_in(3, 2)
        .and_then(|d| d.and_hms_opt(7, 0, 0))
        .map(|d| d.and_utc().timestamp());
    let dst_end = sunday_in(11, 1)
        .and_then(|d| d.and_hms_opt(6, 0, 0))
        .map(|d| d.and_utc().timestamp());
    match (dst_start, dst_end) {
        (Some(start), Some(end)) if ts_secs >= start && ts_secs < end => -4 * 3600,
        _ => -5 * 3600,
    }
}

/// One leg read at a data index; `NaN` legs are `na`.
pub(crate) fn read_leg_slice(leg: &[f64], index: usize) -> Option<PineValue> {
    leg.get(index)
        .and_then(|v| crate::indicator::fin_or_none(*v).map(PineValue::Float))
}

/// Closed-bar cursor flags. Every evaluated bar is a new confirmed history
/// bar; warmup bars never reach this table, so `isfirst` stays false when
/// the driver skips bar zero. Tick/realtime emulation instead follows the
/// `time` leg: `isnew` fires where the bar-open time changes (the first
/// tick of each bar), `isconfirmed` where the next update opens a new bar
/// (the last tick), and the run reports history-false/realtime-true,
/// mirroring a live feed over historical ticks.
/// One per-bar flag at an explicit cursor, shared by both engines: the
/// tree-walk calls it with its run context, the compiled loop with its
/// cursor, leg length, time leg, and tick mode — identical inputs, one
/// implementation, so closed-bar and tick runs can never diverge.
pub(crate) fn barstate_flag_at(
    member: &str,
    cursor: usize,
    bar_count: usize,
    time: &[f64],
    tick_mode: bool,
) -> Option<bool> {
    let last = bar_count.checked_sub(1)?;
    let time_at = |at: usize| time.get(at).copied();
    // Millisecond timestamps are exact integers in `f64`, so bitwise
    // equality is the correct bar-identity test here — no epsilon.
    let changed_since_prev = || {
        cursor
            .checked_sub(1)
            .and_then(|prev| match (time_at(prev), time_at(cursor)) {
                (Some(before), Some(now)) => Some(before.to_bits() != now.to_bits()),
                _ => None,
            })
            .unwrap_or(true)
    };
    let changes_before_next = || match (time_at(cursor), time_at(cursor + 1)) {
        (Some(now), Some(next)) => now.to_bits() != next.to_bits(),
        _ => true,
    };
    match member {
        "isfirst" => Some(cursor == 0),
        "isnew" => Some(if tick_mode {
            changed_since_prev()
        } else {
            true
        }),
        "isconfirmed" => Some(if tick_mode {
            changes_before_next()
        } else {
            true
        }),
        "ishistory" => Some(!tick_mode),
        "islast" => Some(cursor == last),
        "isrealtime" => Some(tick_mode),
        _ => None,
    }
}

pub(crate) fn barstate_flag(member: &str, ctx: &RunCtx<'_>, legs: &Legs, tick_mode: bool) -> Option<bool> {
    barstate_flag_at(member, ctx.cursor(), ctx.bar_count(), &legs.time, tick_mode)
}

/// One named-leg read at the cursor.
pub(crate) fn read_leg(legs: &Legs, name: &str, cursor: usize) -> Option<PineValue> {
    legs.named(name).and_then(|leg| read_leg_slice(leg, cursor))
}

/// One named-leg raw float at the cursor (`NaN` when absent).
pub(crate) fn leg_or_nan(legs: &Legs, name: &str, cursor: usize) -> f64 {
    nan_hole(legs.named(name).and_then(|leg| leg.get(cursor).copied()))
}

/// How many leading rows of `dataset` have finished at `chart`.
///
/// A row is visible only once its own bucket has closed at or before the chart
/// instant; a row whose close is unrecorded is never visible. The count stops at
/// the first unconfirmed row, so an out-of-order close can only under-serve —
/// never leak a forming bucket. This is the single visibility predicate for
/// every injection arm.
pub(crate) fn confirmed_prefix(dataset: &LtfDataset, chart: EpochMs) -> usize {
    dataset
        .closes_at
        .iter()
        .take_while(|close| close.is_some_and(|close| close <= chart))
        .count()
}

/// The rows one injected dataset serves at `chart`: its confirmed prefix,
/// optionally trimmed to the last `count` rows.
///
/// A free function over a borrowed dataset, deliberately: no caller needs a
/// mutable engine (or a copy of the tape) to look at the window, and both
/// `request.*` arms share this one rule instead of each spelling out
/// prefix/skip/take.
pub(crate) fn serve_security(
    dataset: &LtfDataset,
    chart: EpochMs,
    count: Option<usize>,
) -> (usize, usize) {
    let end = confirmed_prefix(dataset, chart);
    let rows = count.map_or(end, |count| end.min(count));
    (end.saturating_sub(rows), end)
}

/// One column of a served window, named by chart leg. Absent values read as
/// `na`, exactly as the pull registry's columns do.
pub(crate) fn security_column(
    dataset: &LtfDataset,
    leg: &str,
    (skip, end): (usize, usize),
) -> Vec<Option<PineValue>> {
    if leg == "time" {
        return dataset.times[skip..end]
            .iter()
            .map(|stamp| Some(PineValue::Int(stamp.get())))
            .collect();
    }
    let column = match leg {
        "open" => &dataset.opens,
        "high" => &dataset.highs,
        "low" => &dataset.lows,
        "close" => &dataset.closes,
        _ => &dataset.volumes,
    };
    column[skip..end]
        .iter()
        .map(|value| crate::indicator::fin_or_none(*value).map(PineValue::Float))
        .collect()
}

/// Current chart-bar time for injection lookups: tick emulation legs already
/// run in Pine-native milliseconds, while closed-bar legs run in whole seconds
/// (pinned runtime-shim behavior). Both domains meet in [`EpochMs`], so the
/// unit conversion has one home instead of a bare scale factor at the
/// comparison site.
///
/// An absent clock reads as the epoch, which is earlier than every injected
/// stamp — the same "no rows" outcome the bounds always produced.
pub(crate) fn chart_ms(legs: &Legs, cursor: usize, tick_mode: bool) -> EpochMs {
    let stamp = leg_or_nan(legs, "time", cursor);
    if tick_mode {
        EpochMs::from_millis_f64(stamp)
    } else {
        EpochMs::from_seconds_f64(stamp)
    }
    .unwrap_or(EpochMs::UNIX_EPOCH)
}

/// Static name of one built-in series variable.
pub(crate) fn builtin_name(var: BuiltinVar) -> &'static str {
    match var {
        BuiltinVar::Open => "open",
        BuiltinVar::High => "high",
        BuiltinVar::Low => "low",
        BuiltinVar::Close => "close",
        BuiltinVar::Volume => "volume",
        BuiltinVar::Time => "time",
    }
}

/// Map one bytecode binary operator onto the shared evaluator's operator.
/// Both engines use this table, so an operator can never mean two things.
#[cfg(feature = "bytecode")]
/// Map one bytecode binary operator onto the shared evaluator's operator.
/// Both engines use this table, so an operator can never mean two things.
pub(crate) fn binary_op_of(op: super::bytecode::BinOp) -> BinaryOp {
    match op {
        super::bytecode::BinOp::Add => BinaryOp::Add,
        super::bytecode::BinOp::Subtract => BinaryOp::Subtract,
        super::bytecode::BinOp::Multiply => BinaryOp::Multiply,
        super::bytecode::BinOp::Divide => BinaryOp::Divide,
        super::bytecode::BinOp::Modulo => BinaryOp::Modulo,
        super::bytecode::BinOp::Equal => BinaryOp::Equal,
        super::bytecode::BinOp::NotEqual => BinaryOp::NotEqual,
        super::bytecode::BinOp::Less => BinaryOp::Less,
        super::bytecode::BinOp::LessEqual => BinaryOp::LessEqual,
        super::bytecode::BinOp::Greater => BinaryOp::Greater,
        super::bytecode::BinOp::GreaterEqual => BinaryOp::GreaterEqual,
        super::bytecode::BinOp::And => BinaryOp::And,
        super::bytecode::BinOp::Or => BinaryOp::Or,
    }
}

/// Binary evaluation: `na` propagates through arithmetic; comparisons are
/// two-valued (an absent side reads false, except `!=` which reads true —
/// the reference never returns `na` from a comparison, so `x == na` is
/// false and `na()` stays the only absence test); mixed kinds outside the
/// reference table refuse instead of coercing.
#[allow(clippy::too_many_lines)]
/// Binary evaluation: `na` propagates through arithmetic; comparisons are
/// two-valued (an absent side reads false, except `!=` which reads true —
/// the reference never returns `na` from a comparison, so `x == na` is
/// false and `na()` stays the only absence test); mixed kinds outside the
/// reference table refuse instead of coercing.
pub(crate) fn binary(
    op: BinaryOp,
    left: Option<&PineValue>,
    right: Option<&PineValue>,
) -> Result<Option<PineValue>, KernelError> {
    if left.is_none() || right.is_none() {
        // Three-valued connectives decide on one absent side; comparisons
        // answer without it; everything else propagates `na`.
        return match op {
            BinaryOp::And | BinaryOp::Or => kleene_absent(op, left, right),
            BinaryOp::Equal
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual => Ok(Some(PineValue::Bool(false))),
            BinaryOp::NotEqual => Ok(Some(PineValue::Bool(true))),
            _ => Ok(None),
        };
    }
    let (Some(first), Some(second)) = (left, right) else {
        return Ok(None);
    };
    match (first, second) {
        (PineValue::Float(a), PineValue::Float(b)) => float_binary(op, *a, *b),
        (PineValue::Int(a), PineValue::Int(b)) => int_binary(op, *a, *b),
        (mix_a, mix_b)
            if matches!(mix_a, PineValue::Float(_) | PineValue::Int(_))
                && matches!(mix_b, PineValue::Float(_) | PineValue::Int(_)) =>
        {
            let (Some(a), Some(b)) = (first.as_float(), second.as_float()) else {
                return Ok(None);
            };
            float_binary(op, a, b)
        }
        (PineValue::Bool(a), PineValue::Bool(b)) => Ok(Some(PineValue::Bool(match op {
            BinaryOp::And => *a && *b,
            BinaryOp::Or => *a || *b,
            BinaryOp::Equal => a == b,
            BinaryOp::NotEqual => a != b,
            _ => {
                return Err(refused(format!("operator '{op}' does not apply to bools")));
            }
        }))),
        (PineValue::Str(a), PineValue::Str(b)) => Ok(Some(match op {
            BinaryOp::Add => PineValue::Str(format!("{a}{b}")),
            BinaryOp::Equal => PineValue::Bool(a == b),
            BinaryOp::NotEqual => PineValue::Bool(a != b),
            BinaryOp::Less => PineValue::Bool(a < b),
            BinaryOp::LessEqual => PineValue::Bool(a <= b),
            BinaryOp::Greater => PineValue::Bool(a > b),
            BinaryOp::GreaterEqual => PineValue::Bool(a >= b),
            _ => {
                return Err(refused(format!(
                    "operator '{op}' does not apply to strings"
                )));
            }
        })),
        (PineValue::Str(text), other)
            if matches!(other, PineValue::Float(_) | PineValue::Int(_)) =>
        {
            string_number_concat(op, text, other)
        }
        (other, PineValue::Str(text))
            if matches!(other, PineValue::Float(_) | PineValue::Int(_)) =>
        {
            match op {
                BinaryOp::Add => Ok(Some(PineValue::Str(format!("{other}{text}")))),
                _ => Err(refused(format!(
                    "operator '{op}' does not apply to numbers and strings"
                ))),
            }
        }
        (PineValue::Bool(_), _) | (_, PineValue::Bool(_))
            if matches!(op, BinaryOp::And | BinaryOp::Or) =>
        {
            Err(refused(format!(
                "operator '{op}' needs bools on both sides, got {first} and {second}"
            )))
        }
        (PineValue::Drawing(a), PineValue::Drawing(b)) => Ok(Some(PineValue::Bool(match op {
            BinaryOp::Equal => a == b,
            BinaryOp::NotEqual => a != b,
            _ => {
                return Err(refused(format!(
                    "operator '{op}' does not apply to drawing objects"
                )));
            }
        }))),
        (PineValue::EnumHandle(a), PineValue::EnumHandle(b)) => {
            Ok(Some(PineValue::Bool(match op {
                BinaryOp::Equal => a.enum_name == b.enum_name && a.member == b.member,
                BinaryOp::NotEqual => a.enum_name != b.enum_name || a.member != b.member,
                _ => {
                    return Err(refused(format!(
                        "operator '{op}' does not apply to enum members"
                    )));
                }
            })))
        }
        _ => Err(refused(format!(
            "operator '{op}' does not apply to {first} and {second}"
        ))),
    }
}

/// `number + string` in either order, mirroring the reference concat.
fn string_number_concat(
    op: BinaryOp,
    text: &str,
    second: &PineValue,
) -> Result<Option<PineValue>, KernelError> {
    match op {
        BinaryOp::Add => Ok(Some(PineValue::Str(format!("{text}{second}")))),
        _ => Err(refused(format!(
            "operator '{op}' does not apply to strings and numbers"
        ))),
    }
}

/// Float arithmetic and comparisons; equality keeps the reference tolerance.
/// Boolean connectives refuse floats (the reference truthiness is a
/// correction, not a copy).
pub(crate) fn float_binary(op: BinaryOp, a: f64, b: f64) -> Result<Option<PineValue>, KernelError> {
    Ok(Some(match op {
        BinaryOp::Add => PineValue::Float(a + b),
        BinaryOp::Subtract => PineValue::Float(a - b),
        BinaryOp::Multiply => PineValue::Float(a * b),
        // A zero denominator reads absence, not a refusal: shipped code
        // divides zero-initialized fields before their first fetch (the
        // market-context `atr` on bar 0), and Pine answers `na`/`inf`
        // there, never an abort. Absence propagates through the existing
        // `na`/`nz` guards; infinities never enter the engine, so no
        // `inf` contagion audit is owed downstream.
        BinaryOp::Divide => {
            if b == 0.0 {
                return Ok(None);
            }
            PineValue::Float(a / b)
        }
        BinaryOp::Modulo => {
            if b == 0.0 {
                return Ok(None);
            }
            PineValue::Float(a % b)
        }
        BinaryOp::Equal => PineValue::Bool((a - b).abs() < EPSILON),
        BinaryOp::NotEqual => PineValue::Bool((a - b).abs() >= EPSILON),
        BinaryOp::Less => PineValue::Bool(a < b),
        BinaryOp::LessEqual => PineValue::Bool(a <= b),
        BinaryOp::Greater => PineValue::Bool(a > b),
        BinaryOp::GreaterEqual => PineValue::Bool(a >= b),
        BinaryOp::And | BinaryOp::Or => {
            return Err(refused(format!("operator '{op}' needs bools, got numbers")));
        }
    }))
}

/// Integer arithmetic (`/` widens to float, as in Pine); connectives refuse.
pub(crate) fn int_binary(op: BinaryOp, a: i64, b: i64) -> Result<Option<PineValue>, KernelError> {
    Ok(Some(match op {
        BinaryOp::Add => PineValue::Int(
            a.checked_add(b)
                .ok_or_else(|| refused("integer addition overflowed"))?,
        ),
        BinaryOp::Subtract => PineValue::Int(
            a.checked_sub(b)
                .ok_or_else(|| refused("integer subtraction overflowed"))?,
        ),
        BinaryOp::Multiply => PineValue::Int(
            a.checked_mul(b)
                .ok_or_else(|| refused("integer multiplication overflowed"))?,
        ),
        BinaryOp::Divide => {
            if b == 0 {
                return Ok(None);
            }
            PineValue::Float(a as f64 / b as f64)
        }
        BinaryOp::Modulo => {
            if b == 0 {
                return Ok(None);
            }
            PineValue::Int(
                a.checked_rem(b)
                    .ok_or_else(|| refused("integer remainder overflowed"))?,
            )
        }
        BinaryOp::Equal => PineValue::Bool(a == b),
        BinaryOp::NotEqual => PineValue::Bool(a != b),
        BinaryOp::Less => PineValue::Bool(a < b),
        BinaryOp::LessEqual => PineValue::Bool(a <= b),
        BinaryOp::Greater => PineValue::Bool(a > b),
        BinaryOp::GreaterEqual => PineValue::Bool(a >= b),
        BinaryOp::And | BinaryOp::Or => {
            return Err(refused(format!("operator '{op}' needs bools, got numbers")));
        }
    }))
}

/// Unary evaluation: negation on numbers, `not` on bools only.
pub(crate) fn unary(
    op: UnaryOp,
    value: Option<&PineValue>,
) -> Result<Option<PineValue>, KernelError> {
    if value.is_none() {
        return Ok(None);
    }
    let Some(present) = value else {
        return Ok(None);
    };
    match (op, present) {
        (UnaryOp::Negate, PineValue::Float(v)) => Ok(Some(PineValue::Float(-*v))),
        (UnaryOp::Negate, PineValue::Int(v)) => Ok(Some(PineValue::Int(
            v.checked_neg()
                .ok_or_else(|| refused("integer negation overflowed"))?,
        ))),
        (UnaryOp::Not, PineValue::Bool(flag)) => Ok(Some(PineValue::Bool(!*flag))),
        (UnaryOp::Not, _) => Err(refused(format!(
            "operator '{op}' needs a bool, got {present}"
        ))),
        _ => Err(refused(format!(
            "operator '{op}' does not apply to {present}"
        ))),
    }
}


/// Visitor verdict at one AST node: descend into children, skip the
/// subtree, or stop the whole walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Visitor verdict at one AST node: descend into children, skip the
/// subtree, or stop the whole walk.
pub(crate) enum WalkVisit {
    /// Descend into child nodes with the same state.
    Recurse,
    /// Skip this subtree, continue with siblings.
    Prune,
    /// Stop the entire walk immediately.
    Stop,
}

/// Single recursion skeleton shared by every mixed-partition query over
/// one program: statements in source order (bodies included), every
/// expression in evaluation order. A visitor observes nodes; the fold
/// owns traversal, so all four queries visit the same nodes in the same
/// order. Returning `Prune` skips a subtree (the names query uses it
/// exactly once: expression-switch bodies drop their writes while the
/// scrutinee, patterns, and statement-switch bodies report normally);
/// returning `Stop` unwinds the whole walk (the `any` queries).
pub(crate) trait PartitionVisit {
    /// Fold-carried accumulator, owned by the caller.
    type State;
    /// Observe one statement before its children.
    fn stmt(&mut self, stmt: &PineStmt, state: &mut Self::State) -> WalkVisit;
    /// Observe one expression before its children.
    fn expr(&mut self, expr: &PineExpr, state: &mut Self::State) -> WalkVisit;
}

/// Walk one statement and, unless pruned or stopped, its children in
/// evaluation order: conditions and bounds before bodies, `if` branches
/// before `else` branches. Function and method bodies walk; parameter
/// defaults never do (their binding timing is unknowable statically —
/// the declaration check owns that rule, not the partition).
pub(crate) fn walk_stmt<V: PartitionVisit>(
    stmt: &PineStmt,
    visit: &mut V,
    state: &mut V::State,
) -> WalkVisit {
    match visit.stmt(stmt, state) {
        WalkVisit::Stop => return WalkVisit::Stop,
        WalkVisit::Prune => return WalkVisit::Recurse,
        WalkVisit::Recurse => {}
    }
    match stmt {
        PineStmt::ExportDecl(inner) => walk_stmt(inner, visit, state),
        PineStmt::FuncDecl { body, .. } | PineStmt::MethodDecl { body, .. } => {
            walk_stmts(body, visit, state)
        }
        // Single-expression statements: the visitor already saw the
        // node; the fold descends into the one child value.
        PineStmt::Declaration { value, .. }
        | PineStmt::Assignment { value, .. }
        | PineStmt::ConstDecl { value, .. }
        | PineStmt::VarDeclaration { value, .. }
        | PineStmt::Reassign { value, .. }
        | PineStmt::DestructureAssign { value, .. } => walk_expr(value, visit, state),
        PineStmt::MultiDeclaration { decls } => {
            for decl in decls {
                if let Some(value) = &decl.value {
                    if walk_expr(value, visit, state) == WalkVisit::Stop {
                        return WalkVisit::Stop;
                    }
                }
            }
            WalkVisit::Recurse
        }
        PineStmt::MultiAssign { items } => {
            for item in items {
                if walk_expr(&item.value, visit, state) == WalkVisit::Stop {
                    return WalkVisit::Stop;
                }
            }
            WalkVisit::Recurse
        }
        PineStmt::Expression(expr) => walk_expr(expr, visit, state),
        PineStmt::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if walk_expr(condition, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            if walk_stmts(then_branch, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            if let Some(branch) = else_branch {
                if walk_stmts(branch, visit, state) == WalkVisit::Stop {
                    return WalkVisit::Stop;
                }
            }
            WalkVisit::Recurse
        }
        PineStmt::For {
            start, end, step, body, ..
        } => {
            if walk_expr(start, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            if walk_expr(end, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            if let Some(step) = step {
                if walk_expr(step, visit, state) == WalkVisit::Stop {
                    return WalkVisit::Stop;
                }
            }
            walk_stmts(body, visit, state)
        }
        PineStmt::ForIn { iterable, body, .. } => {
            if walk_expr(iterable, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            walk_stmts(body, visit, state)
        }
        PineStmt::While { condition, body } => {
            if walk_expr(condition, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            walk_stmts(body, visit, state)
        }
        PineStmt::Switch(switch) => walk_switch(switch, visit, state),
        // Headers, enums, types, loop exits, and anything unlisted carry
        // no walkable expressions: parameters bind in the declaration
        // check, enum members evaluate at registration, and headers are
        // loader metadata. Skipped exactly like the walkers this fold
        // replaces.
        _ => WalkVisit::Recurse,
    }
}

/// Walk one statement list in order, stopping early on `Stop`.
pub(crate) fn walk_stmts<V: PartitionVisit>(
    stmts: &[PineStmt],
    visit: &mut V,
    state: &mut V::State,
) -> WalkVisit {
    for stmt in stmts {
        if walk_stmt(stmt, visit, state) == WalkVisit::Stop {
            return WalkVisit::Stop;
        }
    }
    WalkVisit::Recurse
}

/// Walk one `switch`: scrutinee, then each arm's pattern, then each
/// arm's body. The calls family used to inline this loop; sharing it
/// here is the one traversal every query agrees on.
pub(crate) fn walk_switch<V: PartitionVisit>(
    switch: &SwitchStmt,
    visit: &mut V,
    state: &mut V::State,
) -> WalkVisit {
    if let Some(target) = &switch.scrutinee {
        if walk_expr(target, visit, state) == WalkVisit::Stop {
            return WalkVisit::Stop;
        }
    }
    for arm in &switch.arms {
        if let Some(pattern) = &arm.pattern {
            if walk_expr(pattern, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
        }
        if walk_stmts(&arm.body, visit, state) == WalkVisit::Stop {
            return WalkVisit::Stop;
        }
    }
    WalkVisit::Recurse
}

/// Walk one expression and, unless pruned or stopped, its children in
/// evaluation order: receivers before arguments, conditions before
/// branches, arrays before indices.
pub(crate) fn walk_expr<V: PartitionVisit>(expr: &PineExpr, visit: &mut V, state: &mut V::State) -> WalkVisit {
    match visit.expr(expr, state) {
        WalkVisit::Stop => return WalkVisit::Stop,
        WalkVisit::Prune => return WalkVisit::Recurse,
        WalkVisit::Recurse => {}
    }
    match expr {
        PineExpr::Binary { left, right, .. } => {
            if walk_expr(left, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            walk_expr(right, visit, state)
        }
        PineExpr::Unary { expr, .. } => walk_expr(expr, visit, state),
        PineExpr::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            if walk_expr(condition, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            if walk_expr(then_expr, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            walk_expr(else_expr, visit, state)
        }
        PineExpr::FunctionCall { args, .. } | PineExpr::MethodCall { args, .. } => {
            walk_args(args, visit, state)
        }
        PineExpr::InstanceCall { receiver, args, .. } => {
            if walk_expr(receiver, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            walk_args(args, visit, state)
        }
        PineExpr::FieldRead { receiver, .. } => walk_expr(receiver, visit, state),
        PineExpr::ArrayAccess { array, index } => {
            if walk_expr(array, visit, state) == WalkVisit::Stop {
                return WalkVisit::Stop;
            }
            walk_expr(index, visit, state)
        }
        PineExpr::SeriesOffset { series, .. } => walk_expr(series, visit, state),
        PineExpr::ArrayLiteral(items) => {
            for item in items {
                if walk_expr(item, visit, state) == WalkVisit::Stop {
                    return WalkVisit::Stop;
                }
            }
            WalkVisit::Recurse
        }
        PineExpr::Switch(switch) => walk_switch(switch, visit, state),
        // Literals, variables, legs, and colors are leaves: the visitor
        // already saw them.
        _ => WalkVisit::Recurse,
    }
}

/// Walk call arguments left to right (tree-walk evaluation order).
pub(crate) fn walk_args<V: PartitionVisit>(
    args: &[CallArg],
    visit: &mut V,
    state: &mut V::State,
) -> WalkVisit {
    for arg in args {
        if walk_expr(&arg.value, visit, state) == WalkVisit::Stop {
            return WalkVisit::Stop;
        }
    }
    WalkVisit::Recurse
}