//! Parse tests: both paths, preamble carriage, grammar shape, and the
//! identical-calls golden on a recorded small-model transcript.

use crate::parse::{gbnf_for_tools, parse_gbnf, parse_model_output, parse_xml, ParseError};

const GBNF_TURN: &str = r#"{"preamble": "Let me check the time.", "tool_calls": [{"name": "time_now", "arguments": {}}]}"#;

const XML_TURN: &str = "Let me check the time.<tool_call><invoke name=\"time_now\"></invoke></tool_call>";

#[test]
fn gbnf_parses_calls_with_preamble() {
    let (calls, preamble) = parse_gbnf(GBNF_TURN).expect("gbnf parses");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "time_now");
    assert_eq!(calls[0].preamble, "Let me check the time.");
    assert_eq!(preamble, "Let me check the time.");
}

#[test]
fn gbnf_rejects_prose_and_non_calls() {
    assert!(matches!(
        parse_gbnf("just talking here").unwrap_err(),
        ParseError::BadJson(_)
    ));
    assert_eq!(
        parse_gbnf(r#"{"preamble": "hi", "tool_calls": []}"#).unwrap_err(),
        ParseError::NoCalls
    );
}

#[test]
fn xml_parses_dexter_shape_with_json_or_string_values() {
    let text = "Looking it up.<tool_call><invoke name=\"calc\"><parameter name=\"expr\">{\"op\": \"add\"}</parameter><parameter name=\"note\">rough sum</parameter></invoke></tool_call>";
    let (calls, preamble) = parse_xml(text)
        .expect("xml parses")
        .expect("block found");
    assert_eq!(preamble, "Looking it up.");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "calc");
    assert!(calls[0].args.get("op").is_none());
    assert_eq!(calls[0].args["expr"]["op"], serde_json::json!("add"));
    assert_eq!(calls[0].args["note"], serde_json::json!("rough sum"));
}

#[test]
fn xml_without_blocks_skips() {
    assert!(parse_xml("plain prose, no calls").expect("skip").is_none());
}

#[test]
fn model_output_prefers_gbnf_falls_back_to_xml() {
    let (calls, _) = parse_model_output(GBNF_TURN).expect("gbnf path");
    assert_eq!(calls[0].name, "time_now");
    let (calls, preamble) = parse_model_output(XML_TURN).expect("xml path");
    assert_eq!(calls[0].name, "time_now");
    assert_eq!(preamble, "Let me check the time.");
    assert!(parse_model_output("plain prose here").is_err());
}

#[test]
fn identical_calls_on_both_paths() {
    // Recorded small-model transcript: the grammar path rejects it (not
    // strict JSON) while the XML path must yield byte-identical calls to
    // the grammar-shaped equivalent.
    let strict = r#"{"preamble": "On it.", "tool_calls": [{"name": "shell", "arguments": {"command": "ls /tmp"}}]}"#;
    let loose = "On it.<tool_call><invoke name=\"shell\"><parameter name=\"command\">ls /tmp</parameter></invoke></tool_call>";
    let (gbnf_calls, gbnf_preamble) = parse_model_output(strict).expect("strict parses");
    let (xml_calls, xml_preamble) = parse_model_output(loose).expect("loose parses");
    assert_eq!(gbnf_calls, xml_calls);
    assert_eq!(gbnf_preamble, xml_preamble);
}

#[test]
fn grammar_names_every_tool_and_stays_open_on_args() {
    let grammar = gbnf_for_tools(&["time_now", "shell"]);
    assert!(grammar.contains("\"\\\"time_now\\\"\""), "{grammar}");
    assert!(grammar.contains("\"\\\"shell\\\"\""), "{grammar}");
    assert!(grammar.contains("root ::="), "{grammar}");
    assert!(grammar.contains("ws ::="), "{grammar}");
    let empty = gbnf_for_tools(&[]);
    assert!(empty.contains("root ::="), "{empty}");
}
