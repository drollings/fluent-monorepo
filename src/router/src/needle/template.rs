//! Output-template rendering for direct tool responses.
//!
//! When Needle calls a tool whose `schema_overrides` entry declares an
//! `output_template`, the router answers **directly** by rendering that
//! template with the envelope's bound arguments — no dispatch, no classifier,
//! no extra inference (roadmap design decision 3). This module is the pure,
//! dependency-free renderer: given a template and the bound arguments object it
//! either produces the fully-rendered text or `None`.
//!
//! The `None` contract is the safety gate: a template that references a missing
//! argument, or that has a malformed placeholder (an unclosed brace), must
//! **not** silently produce a half answer. The caller falls through to the
//! normal route/dispatch path in that case — a template only ever *enables* a
//! direct answer, it never forces one.

use serde_json::{Map, Value};

/// Render an output template with the bound arguments.
///
/// - `{key}` placeholders are substituted by looking up `args[key]` and
///   rendering the value inline: scalars via their natural textual form
///   (strings as-is, numbers/booleans via `Display`), objects and arrays as
///   compact JSON.
/// - Returns `None` when a referenced key is missing, a placeholder is
///   malformed (an unclosed brace, a nested `{`), or a placeholder is empty
///   (`{}`) — a template that cannot be fully rendered never yields a partial
///   answer.
pub fn render_output_template(
    template: &str,
    args: &Map<String, Value>,
) -> Option<String> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c == '{' {
            let key = parse_placeholder(&mut chars)?;
            if key.is_empty() {
                return None;
            }
            let value = args.get(&key)?;
            push_rendered(&mut out, value);
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// The `{key}` placeholder keys a template references, in order. Missing /
/// malformed placeholders are skipped (they make the template unrenderable;
/// [`render_output_template`] is the authority on whether rendering succeeds).
/// Used by tests and callers that must build a complete argument set for a
/// template.
pub fn template_placeholders(template: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut chars = template.chars();
    while let Some(c) = chars.next() {
        if c == '{' {
            if let Some(key) = parse_placeholder(&mut chars) {
                if !key.is_empty() {
                    keys.push(key);
                }
            }
        }
    }
    keys
}

/// Consume placeholder characters up to the closing `}`. Returns `None` when
/// the brace is unclosed or a nested `{` appears — both malformed.
fn parse_placeholder(chars: &mut std::str::Chars<'_>) -> Option<String> {
    let mut key = String::new();
    loop {
        match chars.next() {
            Some('}') => return Some(key),
            // An unclosed brace (`None`) and a nested `{` are both malformed.
            Some('{') | None => return None,
            Some(ch) => key.push(ch),
        }
    }
}

/// Append a bound argument's inline textual rendering to `out`.
fn push_rendered(out: &mut String, value: &Value) {
    match value {
        Value::String(s) => out.push_str(s),
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        // Objects and arrays render as compact JSON so structure is preserved.
        other => out.push_str(&other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().expect("object").clone()
    }

    #[test]
    fn substitutes_scalar_placeholders() {
        let args = map(json!({"name": "Paris", "temp_c": 27, "sky": "clear"}));
        assert_eq!(
            render_output_template("It is {temp_c}C and {sky} in {name}", &args),
            Some("It is 27C and clear in Paris".into())
        );
    }

    #[test]
    fn passes_literal_text_through() {
        let args = map(json!({}));
        assert_eq!(
            render_output_template("no placeholders here", &args),
            Some("no placeholders here".into())
        );
    }

    #[test]
    fn missing_arg_returns_none() {
        let args = map(json!({"a": 1}));
        assert_eq!(render_output_template("x {missing} y", &args), None);
    }

    #[test]
    fn unclosed_brace_returns_none() {
        let args = map(json!({"a": 1}));
        assert_eq!(render_output_template("x {a", &args), None);
        assert_eq!(render_output_template("x {a} and {b", &args), None);
    }

    #[test]
    fn nested_brace_returns_none() {
        let args = map(json!({"a": 1}));
        assert_eq!(render_output_template("x {a {b}} y", &args), None);
    }

    #[test]
    fn empty_placeholder_returns_none() {
        let args = map(json!({}));
        assert_eq!(render_output_template("x {} y", &args), None);
    }

    #[test]
    fn renders_object_and_array_as_compact_json() {
        let args = map(json!({"obj": {"k": 1}, "arr": [1, 2]}));
        assert_eq!(
            render_output_template("obj={obj} arr={arr}", &args),
            Some(r#"obj={"k":1} arr=[1,2]"#.into())
        );
    }

    #[test]
    fn renders_null_and_bool() {
        let args = map(json!({"n": null, "b": true}));
        assert_eq!(
            render_output_template("{n} {b}", &args),
            Some("null true".into())
        );
    }

    #[test]
    fn placeholders_are_extracted_in_order() {
        assert_eq!(
            template_placeholders("hello {name}, {temp_c}C"),
            vec!["name".to_string(), "temp_c".to_string()]
        );
        assert_eq!(template_placeholders("no braces"), Vec::<String>::new());
        // Malformed braces are skipped by the extractor.
        assert_eq!(template_placeholders("x {a {b} y"), Vec::<String>::new());
    }
}
