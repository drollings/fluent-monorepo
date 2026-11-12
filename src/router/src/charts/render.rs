//! Chart template rendering — minijinja over a strict, sanitized context.
//!
//! A chart target's `template` is a minijinja source that may reference:
//!
//! - `request` — the user's message
//! - `deps.<name>` — the entities bound to a dependency name
//! - `upstream.<stage_id>.output` — a prior target's structured output.
//!   Attribute drills (`{{ upstream.<id>.output.plan }}`) read it
//!   structured; whole-value pastes MUST use the `tojson` filter
//!   (`{{ upstream.<id>.output | tojson }}`) — bare interpolation renders
//!   the engine's Python-repr scalars (`True`/`None`), not JSON.
//! - `chart` — the chart name (provenance)
//!
//! Nothing else is exposed. The environment is strict (unknown attributes
//! are errors, not silent `undefined`), and every bound-entity string is
//! sanitized at the placeholder boundary before rendering.

use std::collections::HashMap;

use serde::Serialize;

use super::ChartError;

/// Max chars an entity string is truncated to before rendering.
pub const ENTITY_VALUE_MAX_CHARS: usize = 4_096;
/// Max chars a rendered template may produce; exceeding this is a
/// `ChartError::Render` (rejects pathological templates/entities at runtime,
/// not a panic).
pub const CHART_RENDERED_MAX_CHARS: usize = 32_768;

/// What a target's template may reference. Nothing else is exposed.
#[derive(Debug, Clone, Serialize)]
/// What a target's template may reference. Nothing else is exposed.
pub struct RenderContext {
    /// The user's message.
    pub request: String,
    /// dep name → bound entities.
    pub deps: HashMap<String, Vec<BoundEntity>>,
    /// Prior target outputs, keyed by stage id.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub upstream: HashMap<String, serde_json::Value>,
    /// Chart name (provenance).
    pub chart: String,
}

/// A single entity bound to a dep, sanitized before render.
#[derive(Debug, Clone, Serialize)]
/// A single entity bound to a dep, sanitized before render.
pub struct BoundEntity {
    pub id: String,
    pub kind: String,
    /// Sanitized structured payload.
    pub value: serde_json::Value,
}

/// Paste a template value as canonical JSON.
///
/// Compound values interpolate with the engine's Python-repr scalar
/// rendering (`True`/`None`) by default; this filter serializes the value
/// to compact canonical JSON instead (`true`/`null`, double-quoted
/// strings). The output is marked safe and HTML-escaping is skipped: the
/// consumer is an LLM prompt, not a web page, so `<`/`>`/`&` paste
/// verbatim and round-trip byte-identical through `serde_json`.
fn tojson_filter(value: &minijinja::Value) -> Result<minijinja::Value, minijinja::Error> {
    serde_json::to_string(&value)
        .map(minijinja::Value::from_safe_string)
        .map_err(|e| minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, e.to_string()))
}

/// Build a fresh, strict minijinja environment. Renders are per-request and
/// short-lived; a fresh environment per render avoids any shared state.
/// The environment exposes no functions — only the context — plus the
/// `tojson` filter (see [`tojson_filter`]).
///
/// Note: minijinja 2.x folded `UnknownAttributes::Strict` into
/// `UndefinedBehavior::Strict` — under it, any unknown variable *or*
/// attribute access renders as an error instead of silently empty.
/// Unknown *filters* are likewise render errors, so a misspelled `tojson`
/// fails closed instead of pasting raw.
fn build_environment() -> minijinja::Environment<'static> {
    let mut env = minijinja::Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env.add_filter("tojson", tojson_filter);
    env
}

/// Sanitize an entity's structured payload before it is exposed to a
/// template.
///
/// Policy (documented):
/// - Strings are truncated to `ENTITY_VALUE_MAX_CHARS` chars.
/// - Control characters and NUL are stripped.
/// - Excessive whitespace runs are collapsed to a single space.
/// - Strings (and string arrays) are escaped so literal `{{`, `{%`, or `{#`
///   in untrusted entity data cannot open a template construct.
/// - Numbers, booleans, nulls, and nested objects pass through with string
///   leaves sanitized recursively.
pub fn sanitize_entity_value(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => {
            serde_json::Value::String(escape_template_markers(&strip_controls_for_template(&s)))
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sanitize_entity_value).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, val)| (k, sanitize_entity_value(val)))
                .collect(),
        ),
        other => other,
    }
}

/// Truncate, strip HTML/control chars/NUL, and collapse whitespace runs —
/// the control-strip half of template-bound value preparation (the other
/// half is [`escape_template_markers`]).
fn strip_controls_for_template(s: &str) -> String {
    // Strip HTML tags (incl. <script>/<style>) + collapse whitespace — the
    // canonical untrusted-text sanitizer for LLM-bound content.
    let stripped = common_core::string::strip_html(s);
    let collapsed: String = stripped
        .chars()
        .map(|c| {
            if c == '\u{0}' || (c.is_control() && c != '\n' && c != '\t' && c != '\r') {
                ' '
            } else {
                c
            }
        })
        .collect();
    // Control chars replaced by spaces can create double-spaces; collapse
    // whitespace runs once more.
    let collapsed = common_core::string::collapse_whitespace(&collapsed);
    common_core::string::truncate_chars(&collapsed, ENTITY_VALUE_MAX_CHARS)
}

/// Escape Jinja template-open markers so untrusted entity data cannot
/// introduce new template constructs.
///
/// Policy: a `{` that is the start of a `{{`, `{%`, or `{#` token gets a
/// zero-width space (`\u{200B}`) inserted **between** the opening braces.
/// The token is broken, so minijinja treats the data as literal text — but
/// the characters themselves are preserved (the ZWSP is invisible in most
/// renderings).
fn escape_template_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' && matches!(chars.peek(), Some('{' | '%' | '#')) {
            out.push('{');
            out.push('\u{200B}'); // breaks the template token between the braces
        } else {
            out.push(c);
        }
    }
    out
}

/// Render a chart target's template against a sanitized context.
///
/// - Uses a fresh strict environment per call.
/// - The context is sanitized (entity strings escaped/truncated) before
///   rendering.
/// - Prior-target outputs paste as canonical JSON through the `tojson`
///   filter; attribute drills into them stay structured.
/// - Output is capped at `CHART_RENDERED_MAX_CHARS`; exceeding it returns
///   `ChartError::Render`.
pub fn render(template: &str, ctx: &RenderContext) -> Result<String, ChartError> {
    // Sanitize the context at the placeholder boundary: each bound entity's
    // value is run through `sanitize_entity_value`.
    let sanitized_deps: HashMap<String, Vec<BoundEntity>> = ctx
        .deps
        .iter()
        .map(|(dep, entities)| {
            (
                dep.clone(),
                entities
                    .iter()
                    .map(|e| BoundEntity {
                        id: e.id.clone(),
                        kind: e.kind.clone(),
                        value: sanitize_entity_value(e.value.clone()),
                    })
                    .collect(),
            )
        })
        .collect();
    let render_ctx = RenderContext {
        request: strip_controls_for_template(&ctx.request),
        deps: sanitized_deps,
        upstream: ctx.upstream.clone(),
        chart: ctx.chart.clone(),
    };

    let env = build_environment();
    let tmpl = env
        .template_from_str(template)
        .map_err(|e| ChartError::Render {
            target: ctx.chart.clone(),
            detail: e.to_string(),
        })?;

    let rendered = tmpl.render(&render_ctx).map_err(|e| ChartError::Render {
        target: ctx.chart.clone(),
        detail: e.to_string(),
    })?;

    if rendered.chars().count() > CHART_RENDERED_MAX_CHARS {
        return Err(ChartError::Render {
            target: ctx.chart.clone(),
            detail: format!("rendered output exceeds {CHART_RENDERED_MAX_CHARS} chars"),
        });
    }
    Ok(rendered)
}
#[cfg(test)]
#[path = "../../tests/charts_render.rs"]
mod tests;