//! Unit tests for the shared model-output parsing helpers
//! (`extract_json_value`, `strip_think_tags`): clean, wrapped,
//! truncated, and garbage model text. Included via `#[path]`
//! forwarder from `src/model_text.rs`, per repo convention.

use super::{extract_json_value, strip_think_tags};

#[test]
fn clean_object_returns_whole() {
    assert_eq!(extract_json_value(r#"{"1": 85}"#), Some(r#"{"1": 85}"#));
}

#[test]
fn clean_array_returns_whole() {
    assert_eq!(extract_json_value("[1, 2]"), Some("[1, 2]"));
}

#[test]
fn think_wrapped_json_is_found() {
    let text = "<think>scoring each paragraph</think>{\"1\": 85}";
    assert_eq!(extract_json_value(text), Some(r#"{"1": 85}"#));
}

#[test]
fn prose_around_json_is_skipped() {
    let text = "Here you go:\n{\"a\": 1}\nhope this helps";
    assert_eq!(extract_json_value(text), Some(r#"{"a": 1}"#));
}

#[test]
fn unclosed_object_returns_none() {
    assert_eq!(extract_json_value(r#"{"a": 1"#), None);
}

#[test]
fn unclosed_array_returns_none() {
    assert_eq!(extract_json_value("[1, 2"), None);
}

#[test]
fn empty_string_returns_none() {
    assert_eq!(extract_json_value(""), None);
    assert_eq!(extract_json_value("   \n  "), None);
}

#[test]
fn garbage_returns_none() {
    assert_eq!(extract_json_value("hello world"), None);
}

#[test]
fn braces_inside_strings_do_not_close() {
    assert_eq!(extract_json_value(r#"{"a": "}{"}"#), Some(r#"{"a": "}{"}"#));
}

#[test]
fn escaped_quotes_do_not_end_strings() {
    assert_eq!(
        extract_json_value(r#"{"a": "x\"y"}"#),
        Some(r#"{"a": "x\"y"}"#)
    );
}

#[test]
fn nested_structures_balance() {
    assert_eq!(
        extract_json_value(r#"{"a": {"b": [1, {"c": 2}]}}"#),
        Some(r#"{"a": {"b": [1, {"c": 2}]}}"#)
    );
}

#[test]
fn first_opener_wins() {
    assert_eq!(
        extract_json_value(r#"x [1, {"a": 2}] y {"b": 3}"#),
        Some(r#"[1, {"a": 2}]"#)
    );
    assert_eq!(
        extract_json_value(r#"results: {"b": 3} then [4]"#),
        Some(r#"{"b": 3}"#)
    );
}

#[test]
fn mismatched_brackets_return_none() {
    assert_eq!(extract_json_value(r#"{"a": [1}"#), None);
    assert_eq!(extract_json_value("[1}"), None);
}

#[test]
fn strip_think_tags_removes_wrapped_reasoning() {
    assert_eq!(
        strip_think_tags("<think>private reasoning</think>{\"1\": 85}"),
        "{\"1\": 85}"
    );
}

#[test]
fn strip_think_tags_leaves_plain_text_untouched() {
    assert_eq!(strip_think_tags(r#"{"1": 85}"#), r#"{"1": 85}"#);
    assert_eq!(strip_think_tags("plain answer"), "plain answer");
}

#[test]
fn strip_think_tags_handles_unclosed_tag_without_panic() {
    let out = strip_think_tags("<think>unclosed reasoning {\"1\": 85}");
    assert!(!out.contains("<think>"), "tag must be gone, got: {out:?}");
}
