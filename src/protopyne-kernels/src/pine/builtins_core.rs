//! Scalar and structural script namespaces: `math.*`, `str.*`, `input.*`,
//! `array.*`, `map.*`, `color.*`, and `timeframe.*`, plus the opaque display
//! constants (`display.*`, `size.*`, `position.*`, `line.style_*`).
//!
//! [`call`] is a stateless dispatcher — unlike the kernel bridge these
//! namespaces carry no per-bar stream state, so there is nothing to own:
//! every call is a pure function of its arguments (plus the input registry
//! for `input.*`). Scalar math delegates to the shared [`wq_comb`](crate::wq_comb)
//! combinators over single-element legs, so there is exactly one
//! implementation of each op; absence propagates (`na` in, `na` out) and
//! kind mismatches refuse instead of coercing. Crossing/series operators
//! (`change`, `crossover`, `highest`, …) already live behind the kernel
//! bridge and are not repeated here.
//!
//! Two deliberate simplifications, both pinned by test:
//!
//! * Collection operators are functional: `push`, `fill`, `sort`, `put`
//!   return new collections and never mutate in place. Values are owned per
//!   evaluation with no aliasing, so within one expression this is
//!   observationally identical to mutation; rebinding an outer variable from
//!   a statement-position mutation is a known later-stage gap.
//! * `input.source` stores nothing: series defaults are per-bar expressions,
//!   not stored scalars, so the default rides through untouched.

use std::collections::BTreeMap;
use std::collections::HashMap;

use super::syntax::PineType;
use crate::error::KernelError;
use crate::inputs::{InputDef, InputSet};
use crate::value::{ColorHandle, EnumHandle, MatrixValue, PineValue};

/// One evaluated call argument: the `name` from `name = value` when present,
/// else positional.
#[derive(Debug, Clone)]
pub struct CoreArg {
    /// Argument name for named calls, else positional.
    pub name: Option<String>,
    /// Evaluated value (`None` is script `na`).
    pub value: Option<PineValue>,
}

impl CoreArg {
    /// Pack a positional argument.
    #[must_use]
    pub fn pos(value: Option<PineValue>) -> Self {
        Self { name: None, value }
    }

    /// Pack a named argument.
    #[must_use]
    pub fn named(name: &str, value: Option<PineValue>) -> Self {
        Self {
            name: Some(name.to_owned()),
            value,
        }
    }
}

/// Dispatch one `namespace.method(args)` call. `site` identifies the textual
/// call site (stable across bars); only `input.*` uses it, as the
/// declaration key. `type_args` carries explicit generics
/// (`map.new<int, int>`); namespaces without generics ignore it.
pub fn call(
    namespace: &str,
    method: &str,
    site: &str,
    args: &[CoreArg],
    type_args: &[PineType],
    inputs: &mut InputSet,
) -> Result<Option<PineValue>, KernelError> {
    match namespace {
        "math" => math_call(method, args),
        "str" => str_call(method, args),
        "input" => input_call(site, method, args, inputs),
        "array" => array_call(method, args),
        "matrix" => matrix_call(method, args),
        "map" => map_call(method, args, type_args),
        "color" => color_call(method, args),
        "timeframe" => timeframe_call(method, args),
        _ => Err(refused(format!("unknown call '{namespace}.{method}'"))),
    }
}

/// Resolve a dotted display constant (`display.none`, `size.tiny`,
/// `position.top_right`, `color.white`, `line.style_dotted`,
/// `label.style_label_down`) to its opaque handle. Anything else
/// is `None`, so the engine falls through to its undefined-variable refusal.
/// Chart scope (`timeframe.period`) resolves dynamically from the runtime
/// config instead of here.
#[must_use]
pub fn resolve_constant(name: &str) -> Option<PineValue> {
    if let Some(packed) = named_color(name) {
        return Some(PineValue::ColorHandle(ColorHandle::new(packed)));
    }
    let (namespace, member) = name.split_once('.')?;
    let handle = match namespace {
        // Display flags lower as engine-local record tokens (integers), so
        // the documented Pine combining (`display.all - display.status_line`)
        // runs through ordinary arithmetic. Values are never compared or
        // branched on inside the engine — captures only record them — so
        // the numbering is a record convention, not Pine internals.
        "display" => {
            let flag = match member {
                "none" => 0,
                "data_window" => 1,
                "price_scale" => 2,
                "status_line" => 4,
                "pane" => 8,
                "all" => 15,
                _ => return None,
            };
            return Some(PineValue::Int(flag));
        }
        // Bar-merge modes for headless `request.*` injection: the engine
        // serves `gaps_off` + `lookahead_off` from registered datasets and
        // refuses every other mode with a reason at the call site.
        "barmerge" => match member {
            "gaps_on" | "gaps_off" | "lookahead_on" | "lookahead_off" => {
                EnumHandle::new(namespace, member)
            }
            _ => return None,
        },
        "size" => match member {
            "tiny" | "small" | "normal" | "large" | "huge" | "auto" => {
                EnumHandle::new(namespace, member)
            }
            _ => return None,
        },
        "position" => match member {
            "top_left" | "top_center" | "top_right" | "middle_left" | "middle_center"
            | "middle_right" | "bottom_left" | "bottom_center" | "bottom_right" => {
                EnumHandle::new(namespace, member)
            }
            _ => return None,
        },
        "line" => match member {
            "style_solid" | "style_dotted" | "style_dashed" => {
                EnumHandle::new(namespace, member)
            }
            _ => return None,
        },
        // Label marker styles observed in the shipped libraries; anything
        // else stays unknown and refuses at the variable read.
        "label" => match member {
            "style_label_up" | "style_label_down" | "style_label_left" => {
                EnumHandle::new(namespace, member)
            }
            _ => return None,
        },
        // Text alignment observed in the shipped drawing calls.
        "text" => match member {
            "align_left" | "align_center" | "align_right" => {
                EnumHandle::new(namespace, member)
            }
            _ => return None,
        },
        _ => return None,
    };
    Some(PineValue::EnumHandle(handle))
}

// ---------------------------------------------------------------------------
// Shared argument readers
// ---------------------------------------------------------------------------

/// Read a numeric argument with Pine auto-cast (`int` widens to `float`).
/// Absence (`None`, or a `NaN` float) propagates as `None`; anything else
/// refuses naming the function.
fn num(method: &str, value: Option<&PineValue>) -> Result<Option<f64>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::Float(v)) if v.is_nan() => Ok(None),
        Some(PineValue::Float(v)) => Ok(Some(*v)),
        Some(PineValue::Int(v)) => Ok(Some(*v as f64)),
        Some(other) => Err(refused(format!("{method} takes numbers, got {other}"))),
    }
}

/// Collect all-numeric arguments; any absence poisons the whole call.
fn nums(method: &str, args: &[CoreArg]) -> Result<Option<Vec<f64>>, KernelError> {
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        match num(method, arg.value.as_ref())? {
            Some(v) => out.push(v),
            None => return Ok(None),
        }
    }
    Ok(Some(out))
}

/// Read a whole-number argument: `int`s, or integral floats (literals arrive
/// as floats). Fractional or out-of-range values refuse.
fn whole(method: &str, value: Option<&PineValue>, what: &str) -> Result<Option<i64>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::Int(v)) => Ok(Some(*v)),
        Some(PineValue::Float(v))
            if v.is_finite()
                && v.fract() == 0.0
                && *v >= i64::MIN as f64
                && *v <= i64::MAX as f64 =>
        {
            #[allow(clippy::cast_possible_truncation)]
            Ok(Some(*v as i64))
        }
        Some(other) => Err(refused(format!("{method} takes an int {what}, got {other}"))),
    }
}

/// Read a string argument; absence propagates, anything else refuses.
fn text(method: &str, value: Option<&PineValue>) -> Result<Option<String>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::Str(v)) => Ok(Some(v.clone())),
        Some(other) => Err(refused(format!("{method} takes a string, got {other}"))),
    }
}

/// Named argument by name, else the positional fallback: the Nth *unnamed*
/// argument. Named arguments never occupy positional slots, so presentation
/// arguments (`title`, `inline`, `group`, ...) interleaved after the default
/// cannot be misread as options.
fn named_or<'a>(args: &'a [CoreArg], name: &str, positional: usize) -> Option<&'a Option<PineValue>> {
    args.iter()
        .find(|arg| arg.name.as_deref() == Some(name))
        .map(|arg| &arg.value)
        .or_else(|| {
            args.iter()
                .filter(|arg| arg.name.is_none())
                .nth(positional)
                .map(|arg| &arg.value)
        })
}

/// Optional whole-number option (`minval`/`maxval`): missing reads `None`,
/// present values go through [`whole`].
fn opt_whole(method: &str, args: &[CoreArg], name: &str, positional: usize) -> Result<Option<i64>, KernelError> {
    match named_or(args, name, positional) {
        None => Ok(None),
        Some(value) => whole(method, value.as_ref(), name),
    }
}

/// Optional numeric option: missing reads `None`, present values go through
/// [`num`] (so absence inside an option propagates as absence).
fn opt_num(method: &str, args: &[CoreArg], name: &str, positional: usize) -> Result<Option<f64>, KernelError> {
    match named_or(args, name, positional) {
        None => Ok(None),
        Some(value) => num(method, value.as_ref()),
    }
}

fn bad_arity(namespace: &str, method: &str, want: &str, got: usize) -> KernelError {
    refused(format!("{namespace}.{method} takes {want}, got {got} argument(s)"))
}

fn refused(message: impl Into<String>) -> KernelError {
    KernelError::Eval(message.into())
}

// ---------------------------------------------------------------------------
// math.* — one implementation via the shared slice combinators
// ---------------------------------------------------------------------------

/// Run one shared slice combinator over a single scalar, inheriting its
/// funnel contract instead of re-implementing it.
fn single(values: &[f64], f: impl Fn(&[f64]) -> Vec<Option<f64>>) -> Option<PineValue> {
    f(&[values[0]])[0].map(PineValue::Float)
}

/// Every op delegates to its [`wq_comb`](crate::wq_comb) combinator over
/// single-element legs, inheriting the funnel contract (out-of-domain reads
/// absence) instead of re-implementing it.
fn math_call(method: &str, args: &[CoreArg]) -> Result<Option<PineValue>, KernelError> {
    let name = format!("math.{method}");
    let Some(values) = nums(&name, args)? else {
        return Ok(None);
    };
    match method {
        "abs" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_abs)),
        "floor" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_floor)),
        "ceil" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_ceiling)),
        "sqrt" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_sqrt)),
        "log" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_log)),
        "log10" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_log10)),
        "exp" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_exp)),
        "sign" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_sign)),
        "cos" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_cos)),
        "sin" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_sin)),
        "tan" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_tan)),
        "asin" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_arc_sin)),
        "acos" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_arc_cos)),
        "atan" if values.len() == 1 => Ok(single(&values, crate::wq_comb::comb_arc_tan)),
        "pow" if values.len() == 2 => Ok(
            crate::wq_comb::comb_power(&[values[0]], &[values[1]])[0].map(PineValue::Float),
        ),
        "max" if values.len() >= 2 => {
            let legs: Vec<Vec<f64>> = values.iter().map(|v| vec![*v]).collect();
            let refs: Vec<&[f64]> = legs.iter().map(Vec::as_slice).collect();
            Ok(crate::wq_comb::comb_max(&refs)[0].map(PineValue::Float))
        }
        "min" if values.len() >= 2 => {
            let legs: Vec<Vec<f64>> = values.iter().map(|v| vec![*v]).collect();
            let refs: Vec<&[f64]> = legs.iter().map(Vec::as_slice).collect();
            Ok(crate::wq_comb::comb_min(&refs)[0].map(PineValue::Float))
        }
        "avg" if !values.is_empty() => {
            let legs: Vec<Vec<f64>> = values.iter().map(|v| vec![*v]).collect();
            let refs: Vec<&[f64]> = legs.iter().map(Vec::as_slice).collect();
            Ok(crate::wq_comb::comb_mean(&refs)[0].map(PineValue::Float))
        }
        "sum" if values.len() == 1 => Ok(Some(PineValue::Float(values[0]))),
        "sum" if values.len() >= 2 => {
            let legs: Vec<Vec<f64>> = values.iter().map(|v| vec![*v]).collect();
            let refs: Vec<&[f64]> = legs.iter().map(Vec::as_slice).collect();
            let (head, tail) = refs.split_at(2);
            Ok(crate::wq_comb::comb_add(head[0], head[1], tail)[0].map(PineValue::Float))
        }
        "round" if values.len() == 1 => Ok(single(&values, |v| crate::wq_comb::comb_round(v, 0))),
        "round" if values.len() == 2 => {
            let Some(precision) = whole(&name, args[1].value.as_ref(), "precision")? else {
                return Ok(None);
            };
            if precision < 0 {
                return Err(refused(format!(
                    "{name} takes a precision in 0.., got {precision}"
                )));
            }
            let decimals = u32::try_from(precision).map_err(|_| {
                refused(format!("{name} takes a precision in 0.., got {precision}"))
            })?;
            Ok(single(&values, |v| crate::wq_comb::comb_round(v, decimals)))
        }
        _ => Err(refused(format!("unknown call '{name}'"))),
    }
}

// ---------------------------------------------------------------------------
// str.* — pure string functions over owned values
// ---------------------------------------------------------------------------

fn str_call(method: &str, args: &[CoreArg]) -> Result<Option<PineValue>, KernelError> {
    let name = format!("str.{method}");
    let arity = |want: &str| bad_arity("str", method, want, args.len());
    match method {
        "tostring" => {
            let (arg, format) = match args {
                [arg] => (arg, None),
                [arg, format] => (arg, Some(format)),
                _ => return Err(arity("one value and an optional format")),
            };
            let rendered = match &arg.value {
                None => "NaN".to_owned(),
                Some(PineValue::Str(v)) => v.clone(),
                Some(PineValue::Float(v)) if v.is_nan() => "NaN".to_owned(),
                Some(other) => other.to_string(),
            };
            let Some(format) = format else {
                return Ok(Some(PineValue::Str(rendered)));
            };
            let Some(PineValue::Str(pattern)) = &format.value else {
                return Err(refused(format!("{name} takes a string format")));
            };
            // Callers pre-round through `math.round`; the pattern only
            // caps the rendered decimals (`"#"` integer, `"#.##"` up to
            // two, trailing zeros trimmed). Anything else refuses naming
            // the subset instead of guessing a format.
            let text = match pattern.as_str() {
                "#" => match &arg.value {
                    None => "NaN".to_owned(),
                    Some(PineValue::Float(v)) => format!("{v:.0}"),
                    _ => rendered,
                },
                "#.##" => match &arg.value {
                    None => "NaN".to_owned(),
                    Some(PineValue::Float(v)) => {
                        let fixed = format!("{v:.2}");
                        fixed.trim_end_matches('0').trim_end_matches('.').to_owned()
                    }
                    _ => rendered,
                },
                _ => {
                    return Err(refused(format!(
                        "{name} takes a '#' or '#.##' format, got {pattern:?}"
                    )));
                }
            };
            Ok(Some(PineValue::Str(text)))
        }
        "contains" => {
            let [source, part] = args else { return Err(arity("source and substring")) };
            match (text(&name, source.value.as_ref())?, text(&name, part.value.as_ref())?) {
                (Some(haystack), Some(needle)) => Ok(Some(PineValue::Bool(haystack.contains(&needle)))),
                _ => Ok(None),
            }
        }
        "length" => {
            let [source] = args else { return Err(arity("one string")) };
            Ok(text(&name, source.value.as_ref())?.map(|v| PineValue::Int(v.chars().count() as i64)))
        }
        "replace" => {
            let [source, target, replacement] = args else {
                return Err(arity("source, target and replacement"));
            };
            match (
                text(&name, source.value.as_ref())?,
                text(&name, target.value.as_ref())?,
                text(&name, replacement.value.as_ref())?,
            ) {
                (Some(body), Some(old), Some(new)) => {
                    Ok(Some(PineValue::Str(body.replace(&old, &new))))
                }
                _ => Ok(None),
            }
        }
        "split" => {
            let [source, delimiter] = args else { return Err(arity("source and delimiter")) };
            match (text(&name, source.value.as_ref())?, text(&name, delimiter.value.as_ref())?) {
                (Some(body), Some(delim)) if !delim.is_empty() => Ok(Some(PineValue::Array(
                    body.split(&delim)
                        .map(|part| Some(PineValue::Str(part.to_owned())))
                        .collect(),
                ))),
                (Some(_), Some(_)) => Err(refused(format!("{name} takes a non-empty delimiter"))),
                _ => Ok(None),
            }
        }
        "tonumber" => {
            let [source] = args else { return Err(arity("one string")) };
            match text(&name, source.value.as_ref())? {
                Some(v) => Ok(v.trim().parse::<f64>().ok().map(PineValue::Float)),
                None => Ok(None),
            }
        }
        "upper" => {
            let [source] = args else { return Err(arity("one string")) };
            Ok(text(&name, source.value.as_ref())?.map(|v| PineValue::Str(v.to_uppercase())))
        }
        "lower" => {
            let [source] = args else { return Err(arity("one string")) };
            Ok(text(&name, source.value.as_ref())?.map(|v| PineValue::Str(v.to_lowercase())))
        }
        "startswith" => {
            let [source, prefix] = args else { return Err(arity("source and prefix")) };
            match (text(&name, source.value.as_ref())?, text(&name, prefix.value.as_ref())?) {
                (Some(body), Some(head)) => Ok(Some(PineValue::Bool(body.starts_with(&head)))),
                _ => Ok(None),
            }
        }
        "endswith" => {
            let [source, suffix] = args else { return Err(arity("source and suffix")) };
            match (text(&name, source.value.as_ref())?, text(&name, suffix.value.as_ref())?) {
                (Some(body), Some(tail)) => Ok(Some(PineValue::Bool(body.ends_with(&tail)))),
                _ => Ok(None),
            }
        }
        "substring" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(arity("source, start and optional end"));
            }
            let body = text(&name, args[0].value.as_ref())?;
            let start = whole(&name, args[1].value.as_ref(), "start")?;
            let end = if args.len() == 3 {
                whole(&name, args[2].value.as_ref(), "end")?
            } else {
                None
            };
            match (body, start) {
                (Some(text), Some(first)) => {
                    let chars: Vec<char> = text.chars().collect();
                    let len = chars.len() as i64;
                    if first < 0 {
                        return Err(refused(format!("{name} takes a start in 0..=len, got {first}")));
                    }
                    let last = end.unwrap_or(len);
                    if last < 0 || first > last {
                        return Err(refused(format!(
                            "{name} takes an end in start..=len, got {last}"
                        )));
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let (from, to) = (first.min(len) as usize, last.min(len) as usize);
                    Ok(Some(PineValue::Str(chars[from..to].iter().collect())))
                }
                _ => Ok(None),
            }
        }
        "trim" => {
            let [source] = args else { return Err(arity("one string")) };
            Ok(text(&name, source.value.as_ref())?.map(|v| PineValue::Str(v.trim().to_owned())))
        }
        _ => Err(refused(format!("unknown call '{name}'"))),
    }
}

// ---------------------------------------------------------------------------
// input.* — defaults declare into the registry; overrides read back
// ---------------------------------------------------------------------------

/// Declare-or-get: the first call at a site declares the input (defaults
/// drive numerics, presentation arguments stay inert); later calls return the
/// stored value so caller overrides win. A stored value that no longer
/// matches the declaration refuses instead of cross-reading.
fn declare_or_get(
    site: &str,
    def: InputDef,
    inputs: &mut InputSet,
) -> Result<Option<PineValue>, KernelError> {
    if let Some(current) = inputs.get(site) {
        def.validate().map_err(|error| KernelError::Eval(error.to_string()))?;
        current_value_matches(&def, &current)?;
        Ok(Some(current))
    } else {
        inputs.declare(site, def)?;
        Ok(inputs.get(site))
    }
}

/// The stored value must satisfy the declaration that claims it; a kind
/// change at a reused site refuses rather than returning a foreign value.
fn current_value_matches(def: &InputDef, current: &PineValue) -> Result<(), KernelError> {
    match (def, current) {
        (InputDef::Float { .. }, PineValue::Float(_)) |
        (InputDef::Int { .. }, PineValue::Int(_)) |
        (InputDef::Bool { .. }, PineValue::Bool(_)) |
        (InputDef::Text { .. } | InputDef::Session { .. }, PineValue::Str(_)) |
        (InputDef::Enum { .. }, PineValue::EnumHandle(_)) |
        (InputDef::Color { .. }, PineValue::ColorHandle(_)) => Ok(()),
        (InputDef::Source, PineValue::Float(_) | PineValue::Int(_) | PineValue::Bool(_)) => {
            Ok(())
        }
        _ => Err(KernelError::Eval(format!(
            "input site holds {current}, which no longer matches its declaration"
        ))),
    }
}

fn input_call(
    site: &str,
    method: &str,
    args: &[CoreArg],
    inputs: &mut InputSet,
) -> Result<Option<PineValue>, KernelError> {
    let name = format!("input.{method}");
    let defval = named_or(args, "defval", 0).cloned().flatten();
    match method {
        "int" => {
            let default = whole(&name, defval.as_ref(), "default")?.ok_or_else(|| {
                refused(format!("{name} takes a default int, got na"))
            })?;
            let min = opt_whole(&name, args, "minval", 2)?.unwrap_or(i64::MIN);
            let max = opt_whole(&name, args, "maxval", 3)?.unwrap_or(i64::MAX);
            declare_or_get(site, InputDef::Int { default, min, max }, inputs)
        }
        "float" => {
            let default = num(&name, defval.as_ref())?.ok_or_else(|| {
                refused(format!("{name} takes a default float, got na"))
            })?;
            let min = opt_num(&name, args, "minval", 2)?.unwrap_or(f64::MIN);
            let max = opt_num(&name, args, "maxval", 3)?.unwrap_or(f64::MAX);
            declare_or_get(site, InputDef::Float { default, min, max }, inputs)
        }
        "bool" => {
            let Some(PineValue::Bool(default)) = defval else {
                return Err(refused(format!("{name} takes a default bool")));
            };
            declare_or_get(site, InputDef::Bool { default }, inputs)
        }
        "string" => {
            let Some(PineValue::Str(default)) = defval else {
                return Err(refused(format!("{name} takes a default string")));
            };
            declare_or_get(site, InputDef::Text { default }, inputs)
        }
        "source" => {
            match defval {
                None | Some(PineValue::Float(_) | PineValue::Int(_) | PineValue::Bool(_)) => {}
                Some(other) => {
                    return Err(refused(format!(
                        "{name} takes a series int, float, or bool default, got {other}"
                    )));
                }
            }
            if inputs.is_source(site) {
                if let Some(current) = inputs.get(site) {
                    return Ok(Some(current));
                }
                return Ok(defval);
            }
            if inputs.get(site).is_some() {
                return Err(refused(format!(
                    "{name} site holds a different input kind"
                )));
            }
            inputs.declare(site, InputDef::Source)?;
            Ok(defval)
        }
        "enum" => {
            let Some(PineValue::EnumHandle(default)) = defval else {
                return Err(refused(format!("{name} takes a default enum member")));
            };
            if let Some(options) = named_or(args, "options", 2) {
                match options {
                    Some(PineValue::Array(members)) => {
                        let offered = members.iter().any(|member| {
                            matches!(member, Some(PineValue::EnumHandle(handle)) if handle == &default)
                        });
                        if !offered {
                            return Err(refused(format!(
                                "{name} default {}.{} is not among its options",
                                default.enum_name, default.member
                            )));
                        }
                    }
                    Some(other) => {
                        return Err(refused(format!("{name} takes an options array, got {other}")));
                    }
                    None => {}
                }
            }
            declare_or_get(site, InputDef::Enum { default }, inputs)
        }
        "color" => {
            let Some(PineValue::ColorHandle(default)) = defval else {
                return Err(refused(format!("{name} takes a default color")));
            };
            declare_or_get(site, InputDef::Color { default }, inputs)
        }
        "session" => {
            let Some(PineValue::Str(default)) = defval else {
                return Err(refused(format!("{name} takes a default session string")));
            };
            if default.trim().is_empty() {
                return Err(refused(format!("{name} takes a non-empty session string")));
            }
            declare_or_get(site, InputDef::Session { default }, inputs)
        }
        _ => Err(refused(format!("unknown call '{name}'"))),
    }
}

// ---------------------------------------------------------------------------
// array.* — functional operators over owned element vectors
// ---------------------------------------------------------------------------

/// Read an array argument; absence propagates, anything else refuses.
fn arr(method: &str, value: Option<&PineValue>) -> Result<Option<Vec<Option<PineValue>>>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::Array(items)) => Ok(Some(items.clone())),
        Some(other) => Err(refused(format!("{method} takes an array, got {other}"))),
    }
}

/// Present number of one array slot (ints widen, `na` and non-numbers
/// drop); shared by every reduction so absence handling never forks.
fn present_number(slot: Option<&PineValue>) -> Option<f64> {
    match slot {
        Some(PineValue::Float(v)) if !v.is_nan() => Some(*v),
        Some(PineValue::Int(v)) => Some(*v as f64),
        _ => None,
    }
}

/// Present numbers of an array (ints widen); used by every reduction.
fn present_numbers(items: &[Option<PineValue>]) -> Vec<f64> {
    items.iter().filter_map(|slot| present_number(slot.as_ref())).collect()
}

/// Read the optional trailing `biased` flag (`true` when absent); absence
/// propagates, non-bool refuses — the shared `stdev` / `variance` /
/// `covariance` convention.
fn biased_flag(name: &str, args: &[CoreArg], at: usize) -> Result<Option<bool>, KernelError> {
    match args
        .get(at)
        .map_or(Some(PineValue::Bool(true)), |arg| arg.value.clone())
    {
        None => Ok(None),
        Some(PineValue::Bool(flag)) => Ok(Some(flag)),
        Some(other) => Err(refused(format!("{name} takes a bool biased flag, got {other}"))),
    }
}

/// Split collection call: the persisted remainder beside the yielded head.
type SplitResult<C> = Result<Option<(C, Option<PineValue>)>, KernelError>;

/// Split the head off one array argument: the remaining elements beside
/// the removed head (`None` on an empty array). `na` in reads `na` out;
/// kind mismatches refuse. Shared by the `shift` arm and the statement
/// write-back path, which must persist the shrunken array while the call
/// itself yields the head.
pub(crate) fn shift_parts(
    method: &str,
    value: Option<&PineValue>,
) -> SplitResult<Vec<Option<PineValue>>> {
    match arr(method, value)? {
        Some(mut items) => {
            let head = if items.is_empty() {
                None
            } else {
                items.remove(0)
            };
            Ok(Some((items, head)))
        }
        None => Ok(None),
    }
}

/// Split one map entry off by key: the remaining entries beside the
/// removed value (`None` on a missing key). `na` in reads `na` out; kind
/// mismatches refuse. Shared by the `remove` arm and the statement
/// write-back path, which must persist the shrunk map while the call
/// itself yields the removed value.
pub(crate) fn remove_parts(
    method: &str,
    map: Option<&PineValue>,
    key: Option<&PineValue>,
) -> SplitResult<HashMap<i64, i64>> {
    match (
        int_map(method, map)?,
        whole(method, key, "key")?,
    ) {
        (Some(mut entries), Some(found)) => {
            let old = entries.remove(&found).map(PineValue::Int);
            Ok(Some((entries, old)))
        }
        _ => Ok(None),
    }
}

fn array_call(method: &str, args: &[CoreArg]) -> Result<Option<PineValue>, KernelError> {
    let name = format!("array.{method}");
    let arity = |want: &str| bad_arity("array", method, want, args.len());
    match method {
        "new_float" | "new" | "new_int" | "new_bool" | "new_string" => {
            if args.len() > 2 {
                return Err(arity("optional size and initial value"));
            }
            let size = if args.is_empty() {
                0
            } else {
                whole(&name, args[0].value.as_ref(), "size")?.unwrap_or(0)
            };
            if size < 0 {
                return Err(refused(format!("{name} takes a size in 0.., got {size}")));
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let size = size as usize;
            let initial = args.get(1).map(|arg| arg.value.clone()).unwrap_or_default();
            let element: Option<PineValue> = match initial {
                None => None,
                Some(PineValue::Bool(v)) if method == "new_bool" => Some(PineValue::Bool(v)),
                Some(PineValue::Str(v)) if method == "new_string" => Some(PineValue::Str(v)),
                Some(PineValue::Int(v)) if method == "new_int" => Some(PineValue::Int(v)),
                Some(PineValue::Float(v)) if matches!(method, "new_float" | "new") => {
                    Some(PineValue::Float(v))
                }
                Some(PineValue::Int(v)) if matches!(method, "new_float" | "new") => {
                    Some(PineValue::Float(v as f64))
                }
                Some(other) => {
                    return Err(refused(format!("{name} takes a matching initial value, got {other}")));
                }
            };
            Ok(Some(PineValue::Array(vec![element; size])))
        }
        "from" => Ok(Some(PineValue::Array(args.iter().map(|arg| arg.value.clone()).collect()))),
        "size" => {
            let [array] = args else { return Err(arity("one array")) };
            Ok(arr(&name, array.value.as_ref())?.map(|items| PineValue::Int(items.len() as i64)))
        }
        "get" => {
            let [array, index] = args else { return Err(arity("array and index")) };
            match (arr(&name, array.value.as_ref())?, whole(&name, index.value.as_ref(), "index")?) {
                (Some(items), Some(at)) => index_of(&name, &items, at),
                _ => Ok(None),
            }
        }
        "set" => {
            let [array, index, value] = args else { return Err(arity("array, index and value")) };
            match (arr(&name, array.value.as_ref())?, whole(&name, index.value.as_ref(), "index")?) {
                (Some(mut items), Some(at)) => {
                    let slot = array_index(&name, items.len(), at)?;
                    items[slot].clone_from(&value.value);
                    Ok(Some(PineValue::Array(items)))
                }
                _ => Ok(None),
            }
        }
        "push" => {
            let [array, value] = args else { return Err(arity("array and value")) };
            match arr(&name, array.value.as_ref())? {
                Some(mut items) => {
                    items.push(value.value.clone());
                    Ok(Some(PineValue::Array(items)))
                }
                None => Ok(None),
            }
        }
        "first" => {
            let [array] = args else { return Err(arity("one array")) };
            match arr(&name, array.value.as_ref())? {
                Some(items) => Ok(items.first().cloned().unwrap_or(None)),
                None => Ok(None),
            }
        }
        "pop" | "last" => {
            let [array] = args else { return Err(arity("one array")) };
            match arr(&name, array.value.as_ref())? {
                Some(items) => Ok(items.last().cloned().unwrap_or(None)),
                None => Ok(None),
            }
        }
        "copy" => {
            let [array] = args else { return Err(arity("one array")) };
            Ok(arr(&name, array.value.as_ref())?.map(PineValue::Array))
        }
        "clear" => {
            let [array] = args else { return Err(arity("one array")) };
            match arr(&name, array.value.as_ref())? {
                Some(_) => Ok(Some(PineValue::Array(Vec::new()))),
                None => Ok(None),
            }
        }
        "includes" | "indexof" => {
            let [array, value] = args else { return Err(arity("array and value")) };
            match arr(&name, array.value.as_ref())? {
                Some(items) => {
                    let found = items.iter().position(|slot| slot == &value.value);
                    if method == "includes" {
                        Ok(Some(PineValue::Bool(found.is_some())))
                    } else {
                        Ok(Some(PineValue::Int(found.map_or(-1, |at| at as i64))))
                    }
                }
                None => Ok(None),
            }
        }
        "sum" | "avg" | "max" | "min" => {
            let [array] = args else { return Err(arity("one array")) };
            match arr(&name, array.value.as_ref())? {
                Some(items) => {
                    let present = present_numbers(&items);
                    if present.is_empty() {
                        return Ok(None);
                    }
                    let value = match method {
                        "sum" => present.iter().sum(),
                        "avg" => present.iter().sum::<f64>() / present.len() as f64,
                        "max" => present.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                        _ => present.iter().copied().fold(f64::INFINITY, f64::min),
                    };
                    Ok(crate::indicator::fin_or_none(value).map(PineValue::Float))
                }
                None => Ok(None),
            }
        }
        "stdev" => {
            if args.is_empty() || args.len() > 2 {
                return Err(arity("array and optional biased flag"));
            }
            let items = arr(&name, args[0].value.as_ref())?;
            match (items, biased_flag(&name, args, 1)?) {
                (Some(items), Some(biased)) => {
                    let present = present_numbers(&items);
                    if present.is_empty() {
                        return Ok(None);
                    }
                    let deviation = if biased {
                        crate::math::std_pop(&present)
                    } else {
                        crate::math::std_sample(&present)
                    };
                    Ok(crate::indicator::fin_or_none(deviation).map(PineValue::Float))
                }
                _ => Ok(None),
            }
        }
        "fill" => {
            if args.len() < 2 || args.len() > 4 {
                return Err(arity("array, value and optional from/to"));
            }
            match arr(&name, args[0].value.as_ref())? {
                Some(mut items) => {
                    let (from, to) = fill_range(&name, &items, args.get(2), args.get(3))?;
                    for slot in items.iter_mut().skip(from).take(to - from) {
                        slot.clone_from(&args[1].value);
                    }
                    Ok(Some(PineValue::Array(items)))
                }
                None => Ok(None),
            }
        }
        "slice" => {
            let [array, from, to] = args else { return Err(arity("array, from and to")) };
            match arr(&name, array.value.as_ref())? {
                Some(items) => {
                    let (first, last) = fill_range(&name, &items, Some(from), Some(to))?;
                    Ok(Some(PineValue::Array(items[first..last].to_vec())))
                }
                None => Ok(None),
            }
        }
        "reverse" => {
            let [array] = args else { return Err(arity("one array")) };
            Ok(arr(&name, array.value.as_ref())?.map(|mut items| {
                items.reverse();
                PineValue::Array(items)
            }))
        }
        "sort" => {
            let [array] = args else { return Err(arity("one array")) };
            match arr(&name, array.value.as_ref())? {
                Some(mut items) => {
                    sort_items(&name, &mut items)?;
                    Ok(Some(PineValue::Array(items)))
                }
                None => Ok(None),
            }
        }
        "binary_search_leftmost" => {
            let [array, value] = args else { return Err(arity("array and value")) };
            match (arr(&name, array.value.as_ref())?, num(&name, value.value.as_ref())?) {
                (Some(items), Some(target)) => {
                    let present = present_numbers(&items);
                    let mut at = present.len();
                    for (index, candidate) in present.iter().enumerate() {
                        if *candidate >= target {
                            at = index;
                            break;
                        }
                    }
                    #[allow(clippy::cast_possible_wrap)]
                    Ok(Some(PineValue::Int(at as i64)))
                }
                _ => Ok(None),
            }
        }
        "concat" => {
            let [first, second] = args else { return Err(arity("two arrays")) };
            match (
                arr(&name, first.value.as_ref())?,
                arr(&name, second.value.as_ref())?,
            ) {
                (Some(mut head), Some(tail)) => {
                    head.extend(tail);
                    Ok(Some(PineValue::Array(head)))
                }
                _ => Ok(None),
            }
        }
        "shift" => {
            let [array] = args else { return Err(arity("one array")) };
            match shift_parts(&name, array.value.as_ref())? {
                Some((_, head)) => Ok(head),
                None => Ok(None),
            }
        }
        "variance" => {
            if args.is_empty() || args.len() > 2 {
                return Err(arity("array and optional biased flag"));
            }
            let items = arr(&name, args[0].value.as_ref())?;
            match (items, biased_flag(&name, args, 1)?) {
                (Some(items), Some(biased)) => {
                    let present = present_numbers(&items);
                    if present.is_empty() {
                        return Ok(None);
                    }
                    let spread = if biased {
                        crate::math::variance_pop(&present)
                    } else {
                        crate::math::variance_sample(&present)
                    };
                    Ok(crate::indicator::fin_or_none(spread).map(PineValue::Float))
                }
                _ => Ok(None),
            }
        }
        "median" => {
            let [array] = args else { return Err(arity("one array")) };
            match arr(&name, array.value.as_ref())? {
                Some(items) => {
                    Ok(crate::math::median_of(&present_numbers(&items)).map(PineValue::Float))
                }
                None => Ok(None),
            }
        }
        "mode" => {
            let [array] = args else { return Err(arity("one array")) };
            match arr(&name, array.value.as_ref())? {
                Some(items) => {
                    Ok(crate::math::mode_of(&present_numbers(&items)).map(PineValue::Float))
                }
                None => Ok(None),
            }
        }
        "standardize" => {
            let [array] = args else { return Err(arity("one array")) };
            match arr(&name, array.value.as_ref())? {
                Some(items) => {
                    let scores = crate::math::standardize_scores(&present_numbers(&items));
                    Ok(Some(PineValue::Array(
                        scores
                            .into_iter()
                            .map(|slot| slot.map(PineValue::Float))
                            .collect(),
                    )))
                }
                None => Ok(None),
            }
        }
        "percentile_linear_interpolation" => {
            let [array, percentage] = args else { return Err(arity("array and percentage")) };
            match (
                arr(&name, array.value.as_ref())?,
                num(&name, percentage.value.as_ref())?,
            ) {
                (Some(items), Some(rank)) => {
                    if !(0.0..=100.0).contains(&rank) {
                        return Err(refused(format!(
                            "{name} takes a percentage in 0..=100, got {rank}"
                        )));
                    }
                    let present = present_numbers(&items);
                    if present.is_empty() {
                        return Ok(None);
                    }
                    Ok(crate::math::percentile_linear(&present, rank).map(PineValue::Float))
                }
                _ => Ok(None),
            }
        }
        "covariance" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(arity("two arrays and optional biased flag"));
            }
            match (
                arr(&name, args[0].value.as_ref())?,
                arr(&name, args[1].value.as_ref())?,
                biased_flag(&name, args, 2)?,
            ) {
                (Some(first), Some(second), Some(biased)) => {
                    if first.len() != second.len() {
                        return Err(refused(format!(
                            "{name} takes equal-length arrays, got {} and {}",
                            first.len(),
                            second.len()
                        )));
                    }
                    // Pairwise-present legs: a hole on either side drops
                    // the pair, matching the reductions' absence rule.
                    let (mut xs, mut ys) = (Vec::new(), Vec::new());
                    for (left, right) in first.iter().zip(second.iter()) {
                        if let (Some(x), Some(y)) =
                            (present_number(left.as_ref()), present_number(right.as_ref()))
                        {
                            xs.push(x);
                            ys.push(y);
                        }
                    }
                    let joint = if biased {
                        crate::math::covariance_pop(&xs, &ys)
                    } else {
                        crate::math::covariance_sample(&xs, &ys)
                    };
                    Ok(joint.map(PineValue::Float))
                }
                _ => Ok(None),
            }
        }
        _ => Err(refused(format!("unknown call '{name}'"))),
    }
}

/// Borrow one matrix argument (`na` in, `na` out); kind mismatches refuse
/// instead of coercing, mirroring [`arr`].
fn grid(method: &str, value: Option<&PineValue>) -> Result<Option<MatrixValue>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::Matrix(cells)) => Ok(Some(cells.clone())),
        Some(other) => Err(refused(format!("{method} takes a matrix, got {other}"))),
    }
}

/// Resolve one whole row/column index against a grid shape; negative and
/// past-end indexes refuse (the script runtime errors), mirroring
/// [`array_index`].
fn grid_index(name: &str, len: usize, at: i64) -> Result<usize, KernelError> {
    if at < 0 {
        return Err(refused(format!("{name} takes an index in 0..size, got {at}")));
    }
    let at = usize::try_from(at)
        .map_err(|_| refused(format!("{name} takes an index in 0..size, got {at}")))?;
    if at >= len {
        return Err(refused(format!("{name} index {at} is past size {len}")));
    }
    Ok(at)
}

fn matrix_call(method: &str, args: &[CoreArg]) -> Result<Option<PineValue>, KernelError> {
    let name = format!("matrix.{method}");
    let arity = |want: &str| bad_arity("matrix", method, want, args.len());
    match method {
        "new" => {
            let (rows, columns, initial) = match args {
                [rows, columns] => (rows, columns, None),
                [rows, columns, initial] => (rows, columns, Some(initial)),
                _ => return Err(arity("rows, columns and optional initial value")),
            };
            match (
                whole(&name, rows.value.as_ref(), "rows")?,
                whole(&name, columns.value.as_ref(), "columns")?,
            ) {
                (Some(rows), Some(columns)) => {
                    if rows < 0 || columns < 0 {
                        return Err(refused(format!(
                            "{name} takes a shape in 0.., got {rows}x{columns}"
                        )));
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let (rows, columns) = (rows as usize, columns as usize);
                    let initial = initial.and_then(|arg| arg.value.clone());
                    Ok(Some(PineValue::Matrix(MatrixValue::filled(
                        rows, columns, initial,
                    ))))
                }
                _ => Ok(None),
            }
        }
        "rows" => {
            let [matrix] = args else { return Err(arity("one matrix")) };
            #[allow(clippy::cast_possible_wrap)]
            Ok(grid(&name, matrix.value.as_ref())?.map(|cells| PineValue::Int(cells.rows as i64)))
        }
        "columns" => {
            let [matrix] = args else { return Err(arity("one matrix")) };
            #[allow(clippy::cast_possible_wrap)]
            Ok(grid(&name, matrix.value.as_ref())?.map(|cells| PineValue::Int(cells.cols as i64)))
        }
        "get" => {
            let [matrix, row, column] = args else { return Err(arity("matrix, row and column")) };
            match (
                grid(&name, matrix.value.as_ref())?,
                whole(&name, row.value.as_ref(), "row")?,
                whole(&name, column.value.as_ref(), "column")?,
            ) {
                (Some(cells), Some(row), Some(column)) => {
                    let row = grid_index(&name, cells.rows, row)?;
                    let column = grid_index(&name, cells.cols, column)?;
                    Ok(cells.get(row, column).unwrap_or(None))
                }
                _ => Ok(None),
            }
        }
        "set" => {
            let [matrix, row, column, value] = args else {
                return Err(arity("matrix, row, column and value"));
            };
            match (
                grid(&name, matrix.value.as_ref())?,
                whole(&name, row.value.as_ref(), "row")?,
                whole(&name, column.value.as_ref(), "column")?,
            ) {
                (Some(mut cells), Some(row), Some(column)) => {
                    let row = grid_index(&name, cells.rows, row)?;
                    let column = grid_index(&name, cells.cols, column)?;
                    cells.set(row, column, value.value.clone());
                    Ok(Some(PineValue::Matrix(cells)))
                }
                _ => Ok(None),
            }
        }
        _ => Err(refused(format!("unknown call '{name}'"))),
    }
}

/// Bounds-checked index into an array; out-of-range refuses (the script
/// runtime errors), negative refuses.
fn index_of(
    name: &str,
    items: &[Option<PineValue>],
    at: i64,
) -> Result<Option<PineValue>, KernelError> {
    let slot = array_index(name, items.len(), at)?;
    Ok(items[slot].clone())
}

/// Resolve one whole index against a length; out-of-range refuses (the
/// script runtime errors), negative refuses. Shared by `get` and `set`.
fn array_index(name: &str, len: usize, at: i64) -> Result<usize, KernelError> {
    if at < 0 {
        return Err(refused(format!("{name} takes an index in 0..size, got {at}")));
    }
    let at = usize::try_from(at)
        .map_err(|_| refused(format!("{name} takes an index in 0..size, got {at}")))?;
    if at >= len {
        return Err(refused(format!("{name} index {at} is past size {len}")));
    }
    Ok(at)
}

/// Clamped `[from, to)` range over an array for `fill`/`slice`; negative
/// bounds refuse, over-long ends clamp.
fn fill_range(
    name: &str,
    items: &[Option<PineValue>],
    from: Option<&CoreArg>,
    to: Option<&CoreArg>,
) -> Result<(usize, usize), KernelError> {
    let len = items.len() as i64;
    let first = match from {
        Some(arg) => whole(name, arg.value.as_ref(), "from")?.unwrap_or(0),
        None => 0,
    };
    let last = match to {
        Some(arg) => whole(name, arg.value.as_ref(), "to")?.unwrap_or(len),
        None => len,
    };
    if first < 0 || last < 0 || first > last {
        return Err(refused(format!("{name} takes 0 <= from <= to, got {first}..{last}")));
    }
    let from = usize::try_from(first.min(len))
        .map_err(|_| refused(format!("{name} takes 0 <= from <= to, got {first}..{last}")))?;
    let to = usize::try_from(last.min(len))
        .map_err(|_| refused(format!("{name} takes 0 <= from <= to, got {first}..{last}")))?;
    Ok((from, to))
}

/// Order array elements in place: numbers ascending (`int` widens),
/// strings lexicographic, absence last. Mixed kinds refuse — sorting has no
/// script meaning across types.
fn sort_items(name: &str, items: &mut [Option<PineValue>]) -> Result<(), KernelError> {
    let numeric = items.iter().all(|slot| {
        matches!(slot, None | Some(PineValue::Float(_) | PineValue::Int(_)))
    });
    let textual = items.iter().all(|slot| matches!(slot, None | Some(PineValue::Str(_))));
    if numeric {
        items.sort_by(|left, right| match (left, right) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, _) => std::cmp::Ordering::Greater,
            (_, None) => std::cmp::Ordering::Less,
            (Some(left), Some(right)) => {
                let (x, y) = (scalar_number(left), scalar_number(right));
                x.total_cmp(&y)
            }
        });
        Ok(())
    } else if textual {
        items.sort_by(|left, right| match (left, right) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, _) => std::cmp::Ordering::Greater,
            (_, None) => std::cmp::Ordering::Less,
            (Some(PineValue::Str(x)), Some(PineValue::Str(y))) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        });
        Ok(())
    } else {
        Err(refused(format!("{name} sorts numbers or strings, not mixed values")))
    }
}

/// Numeric key of a scalar already classified numeric by the caller.
fn scalar_number(value: &PineValue) -> f64 {
    match value {
        PineValue::Float(v) => *v,
        PineValue::Int(v) => *v as f64,
        _ => f64::NAN,
    }
}

// ---------------------------------------------------------------------------
// map.* — int-to-int maps, the scheduler-state shape
// ---------------------------------------------------------------------------

/// Read a map argument; absence propagates, anything else refuses.
fn int_map(method: &str, value: Option<&PineValue>) -> Result<Option<HashMap<i64, i64>>, KernelError> {
    match value {
        None => Ok(None),
        Some(PineValue::IntMap(entries)) => Ok(Some(entries.clone())),
        Some(other) => Err(refused(format!("{method} takes a map, got {other}"))),
    }
}

/// Declared key types a general map accepts: the scalar shapes plus dotted
/// (enum or otherwise qualified) names. Collections and user objects are
/// never keys; anything else refuses at construction.
fn is_scalar_key(name: &str) -> bool {
    matches!(name, "int" | "float" | "bool" | "string" | "color") || name.contains('.')
}

fn map_call(method: &str, args: &[CoreArg], type_args: &[PineType]) -> Result<Option<PineValue>, KernelError> {    let name = format!("map.{method}");
    let arity = |want: &str| bad_arity("map", method, want, args.len());
    match method {
        "new" => {
            if !args.is_empty() {
                return Err(arity("no arguments"));
            }
            let [key, value] = type_args else {
                return Err(refused(format!("{name} takes explicit key and value types")));
            };
            if key.name == "int" && value.name == "int" && key.args.is_empty() && value.args.is_empty() {
                Ok(Some(PineValue::IntMap(HashMap::new())))
            } else if is_scalar_key(&key.name) {
                Ok(Some(PineValue::Map(BTreeMap::new())))
            } else {
                Err(refused(format!(
                    "map.new takes a scalar key type (int, float, bool, string, color, or an enum), got {}",
                    key.name
                )))
            }
        }
        "put" => {
            let [map, key, value] = args else { return Err(arity("map, key and value")) };
            match map.value.as_ref() {
                None => Ok(None),
                Some(PineValue::IntMap(entries)) => {
                    match (
                        whole(&name, key.value.as_ref(), "key")?,
                        whole(&name, value.value.as_ref(), "value")?,
                    ) {
                        (Some(k), Some(v)) => {
                            let mut stored = entries.clone();
                            stored.insert(k, v);
                            Ok(Some(PineValue::IntMap(stored)))
                        }
                        _ => Ok(None),
                    }
                }
                Some(PineValue::Map(entries)) => match key.value.as_ref() {
                    Some(key) => {
                        let Some(encoded) = crate::value::map_key_of(key) else {
                            return Err(refused(format!("{name} takes a scalar key, got {key}")));
                        };
                        let mut stored = entries.clone();
                        stored.insert(encoded, (key.clone(), value.value.clone()));
                        Ok(Some(PineValue::Map(stored)))
                    }
                    None => Ok(None),
                },
                Some(other) => Err(refused(format!("{name} takes a map, got {other}"))),
            }
        }
        "get" => {
            let [map, key] = args else { return Err(arity("map and key")) };
            match map.value.as_ref() {
                None => Ok(None),
                Some(PineValue::IntMap(entries)) => {
                    match whole(&name, key.value.as_ref(), "key")? {
                        Some(k) => Ok(entries.get(&k).map(|v| PineValue::Int(*v))),
                        None => Ok(None),
                    }
                }
                Some(PineValue::Map(entries)) => match key.value.as_ref() {
                    Some(key) => {
                        let Some(encoded) = crate::value::map_key_of(key) else {
                            return Err(refused(format!("{name} takes a scalar key, got {key}")));
                        };
                        Ok(entries.get(&encoded).and_then(|(_, value)| value.clone()))
                    }
                    None => Ok(None),
                },
                Some(other) => Err(refused(format!("{name} takes a map, got {other}"))),
            }
        }
        "contains" => {
            let [map, key] = args else { return Err(arity("map and key")) };
            match map.value.as_ref() {
                None => Ok(None),
                Some(PineValue::IntMap(entries)) => {
                    match whole(&name, key.value.as_ref(), "key")? {
                        Some(k) => Ok(Some(PineValue::Bool(entries.contains_key(&k)))),
                        None => Ok(None),
                    }
                }
                Some(PineValue::Map(entries)) => match key.value.as_ref() {
                    Some(key) => {
                        let Some(encoded) = crate::value::map_key_of(key) else {
                            return Err(refused(format!("{name} takes a scalar key, got {key}")));
                        };
                        Ok(Some(PineValue::Bool(entries.contains_key(&encoded))))
                    }
                    None => Ok(None),
                },
                Some(other) => Err(refused(format!("{name} takes a map, got {other}"))),
            }
        }
        "remove" => {
            let [map, key] = args else { return Err(arity("map and key")) };
            if matches!(map.value, Some(PineValue::Map(_))) {
                return Err(refused(format!(
                    "{name} on general maps is not bridged; rebuild the map without the key"
                )));
            }
            match remove_parts(&name, map.value.as_ref(), key.value.as_ref())? {
                Some((_, old)) => Ok(old),
                None => Ok(None),
            }
        }
        "size" => {
            let [map] = args else { return Err(arity("one map")) };
            match map.value.as_ref() {
                None => Ok(None),
                Some(PineValue::IntMap(entries)) => {
                    Ok(Some(PineValue::Int(entries.len() as i64)))
                }
                Some(PineValue::Map(entries)) => Ok(Some(PineValue::Int(entries.len() as i64))),
                Some(other) => Err(refused(format!("{name} takes a map, got {other}"))),
            }
        }
        "keys" => {
            let [map] = args else { return Err(arity("one map")) };
            match map.value.as_ref() {
                None => Ok(None),
                Some(PineValue::IntMap(entries)) => {
                    let mut keys: Vec<i64> = entries.keys().copied().collect();
                    keys.sort_unstable();
                    Ok(Some(PineValue::Array(
                        keys.into_iter().map(|k| Some(PineValue::Int(k))).collect(),
                    )))
                }
                Some(PineValue::Map(entries)) => Ok(Some(PineValue::Array(
                    entries.values().map(|(key, _)| Some(key.clone())).collect(),
                ))),
                Some(other) => Err(refused(format!("{name} takes a map, got {other}"))),
            }
        }
        "clear" => {
            let [map] = args else { return Err(arity("one map")) };
            match map.value.as_ref() {
                None => Ok(None),
                Some(PineValue::IntMap(_)) => Ok(Some(PineValue::IntMap(HashMap::new()))),
                Some(PineValue::Map(_)) => Ok(Some(PineValue::Map(BTreeMap::new()))),
                Some(other) => Err(refused(format!("{name} takes a map, got {other}"))),
            }
        }
        _ => Err(refused(format!("unknown call '{name}'"))),
    }
}

// ---------------------------------------------------------------------------
// color.* — packed opaque handles; display math only
// ---------------------------------------------------------------------------

/// Standard script palette, packed `0xRRGGBB00` (opaque until `new` tints).
fn named_color(name: &str) -> Option<u32> {
    match name {
        "color.white" => Some(0xFF_FF_FF_00),
        "color.black" => Some(0x00_00_00_00),
        "color.red" => Some(0xFF_00_00_00),
        "color.green" => Some(0x00_80_00_00),
        "color.blue" => Some(0x00_00_FF_00),
        "color.orange" => Some(0xFF_A0_00_00),
        "color.teal" => Some(0x00_80_80_00),
        "color.aqua" => Some(0x00_FF_FF_00),
        "color.gray" => Some(0x78_78_78_00),
        "color.lime" => Some(0x00_FF_00_00),
        "color.yellow" => Some(0xFF_FF_00_00),
        _ => None,
    }
}

/// Clamp a channel into `0..=255` (display-only values clamp, never refuse).
/// The float-to-int cast is exact: the value is rounded and clamped finite
/// first, so no truncation or sign loss can occur.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn channel(value: f64) -> u32 {
    value.round().clamp(0.0, 255.0) as u32
}

/// Clamp transparency into `0..=100`, same exact-cast contract as [`channel`].
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn transparency(value: f64) -> u32 {
    value.round().clamp(0.0, 100.0) as u32
}

fn pack(red: u32, green: u32, blue: u32, transp: u32) -> u32 {
    (red << 24) | (green << 16) | (blue << 8) | transp
}

/// Pack a `#rrggbb[aa]` literal into a color handle: six digits are opaque,
/// eight digits scale the alpha byte (`00` opaque .. `ff` fully transparent)
/// into the `0..=100` transparency lane. Anything else is not a literal.
#[must_use]
pub fn color_of_hex(digits: &str) -> Option<u32> {
    let channel = |pair: &str| u32::from_str_radix(pair, 16).ok();
    if digits.len() == 6 {
        Some(pack(
            channel(&digits[0..2])?,
            channel(&digits[2..4])?,
            channel(&digits[4..6])?,
            0,
        ))
    } else if digits.len() == 8 {
        let alpha = f64::from(channel(&digits[6..8])?);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let transp = (alpha * 100.0 / 255.0).round().clamp(0.0, 100.0) as u32;
        Some(pack(
            channel(&digits[0..2])?,
            channel(&digits[2..4])?,
            channel(&digits[4..6])?,
            transp,
        ))
    } else {
        None
    }
}

fn unpack(handle: u32) -> (f64, f64, f64, f64) {
    (
        f64::from((handle >> 24) & 0xFF),
        f64::from((handle >> 16) & 0xFF),
        f64::from((handle >> 8) & 0xFF),
        f64::from(handle & 0xFF),
    )
}

fn color_call(method: &str, args: &[CoreArg]) -> Result<Option<PineValue>, KernelError> {
    let name = format!("color.{method}");
    let arity = |want: &str| bad_arity("color", method, want, args.len());
    match method {
        "rgb" => {
            if args.len() < 3 || args.len() > 4 {
                return Err(arity("red, green, blue and optional transparency"));
            }
            let channels: Vec<Option<f64>> = args
                .iter()
                .map(|arg| num(&name, arg.value.as_ref()))
                .collect::<Result<_, _>>()?;
            match &channels[..] {
                [Some(r), Some(g), Some(b)] => {
                    Ok(Some(PineValue::ColorHandle(ColorHandle::new(pack(channel(*r), channel(*g), channel(*b), 0)))))
                }
                [Some(r), Some(g), Some(b), Some(t)] => {
                    Ok(Some(PineValue::ColorHandle(ColorHandle::new(pack(
                        channel(*r),
                        channel(*g),
                        channel(*b),
                        transparency(*t),
                    )))))
                }
                _ => Ok(None),
            }
        }
        "new" => {
            let [color, transp] = args else { return Err(arity("color and transparency")) };
            match (&color.value, num(&name, transp.value.as_ref())?) {
                (Some(PineValue::ColorHandle(handle)), Some(t)) => {
                    let (r, g, b, _) = unpack(handle.raw());
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let (r, g, b) = (r as u32, g as u32, b as u32);
                    Ok(Some(PineValue::ColorHandle(ColorHandle::new(pack(r, g, b, transparency(t))))))
                }
                (Some(other), _) => Err(refused(format!("{name} takes a color, got {other}"))),
                _ => Ok(None),
            }
        }
        "from_gradient" => {
            let [value, bottom, top, start, end] = args else {
                return Err(arity("value, bottom, top, start and end colors"));
            };
            match (
                num(&name, value.value.as_ref())?,
                num(&name, bottom.value.as_ref())?,
                num(&name, top.value.as_ref())?,
            ) {
                (Some(v), Some(lo), Some(hi)) => {
                    // Exact equality is the degenerate-range check here, not a
                    // tolerance: a zero range has no gradient to walk.
                    #[allow(clippy::float_cmp)]
                    if lo == hi {
                        return Err(refused(format!("{name} takes bottom != top")));
                    }
                    let (Some(PineValue::ColorHandle(first)), Some(PineValue::ColorHandle(last))) =
                        (&start.value, &end.value)
                    else {
                        return Err(refused(format!("{name} takes start and end colors")));
                    };
                    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                    let (r0, g0, b0, t0) = unpack(first.raw());
                    let (r1, g1, b1, t1) = unpack(last.raw());
                    let lerp = |a: f64, b: f64| a + (b - a) * t;
                    Ok(Some(PineValue::ColorHandle(ColorHandle::new(pack(
                        channel(lerp(r0, r1)),
                        channel(lerp(g0, g1)),
                        channel(lerp(b0, b1)),
                        transparency(lerp(t0, t1)),
                    )))))
                }
                _ => Ok(None),
            }
        }
        _ => Err(refused(format!("unknown call '{name}'"))),
    }
}

// ---------------------------------------------------------------------------
// timeframe.*
// ---------------------------------------------------------------------------

fn timeframe_call(method: &str, args: &[CoreArg]) -> Result<Option<PineValue>, KernelError> {
    let name = format!("timeframe.{method}");
    match method {
        "in_seconds" => {
            let [period] = args else {
                return Err(bad_arity("timeframe", method, "one period string", args.len()));
            };
            match text(&name, period.value.as_ref())? {
                Some(raw) => period_seconds(&name, &raw).map(|secs| Some(PineValue::Int(secs))),
                None => Ok(None),
            }
        }
        _ => Err(refused(format!("unknown call '{name}'"))),
    }
}

/// Check a chart period string with the same parser as
/// `timeframe.in_seconds`, so runtime scope and the bridge never drift.
pub(crate) fn check_period(raw: &str) -> Result<(), String> {
    period_seconds("chart period", raw)
        .map(|_| ())
        .map_err(|reason| reason.to_string())
}

/// Parse a period string (`"15S"`, `"5"`, `"D"`, `"W"`, `"M"`) into seconds;
/// anything else refuses with the offending text.
fn period_seconds(name: &str, raw: &str) -> Result<i64, KernelError> {    let body = raw.trim();
    let (digits, factor) = if let Some(seconds) = body.strip_suffix('S') {
        (seconds, 1)
    } else if let Some(days) = body.strip_suffix('D') {
        (days, 86_400)
    } else if let Some(weeks) = body.strip_suffix('W') {
        (weeks, 604_800)
    } else if let Some(months) = body.strip_suffix('M') {
        (months, 2_592_000)
    } else {
        (body, 60)
    };
    match digits.parse::<i64>() {
        Ok(count) if count > 0 => count
            .checked_mul(factor)
            .map_or_else(|| Err(refused(format!("{name} period {raw:?} overflows"))), Ok),
        // A bare unit (`"D"`, `"W"`) means one of it.
        Err(_) if digits.is_empty() => Ok(factor),
        _ => Err(refused(format!("{name} takes a period like \"5\" or \"D\", got {raw:?}"))),
    }
}

#[cfg(test)]
#[path = "../../tests/pine_builtins_core.rs"]
mod tests;
