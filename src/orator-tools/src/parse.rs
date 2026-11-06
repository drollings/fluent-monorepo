//! Tool-call parsing: strict JSON primary, Dexter-shaped XML fallback.
//!
//! The grammar emitter produces the `llama-server --grammar-file` text
//! constraining model output to the registry's call shape. Parsing runs
//! GBNF first and XML on failure through ordered fallback — a recorded
//! small-model transcript yields byte-identical calls on both paths.

use fluent_wvr::ladder::first_accept_in_order_sync;
use thiserror::Error;

use crate::tool::ToolCall;

/// Parse failures. Unknown tools fail here, never at execution: a
/// near-valid call naming the wrong tool must not run.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// No calls on either path.
    #[error("no tool calls found")]
    NoCalls,
    /// Strict JSON malformed.
    #[error("malformed tool JSON: {0}")]
    BadJson(String),
    /// Names a tool outside the registry.
    #[error("unknown tool: {0}")]
    UnknownTool(String),
}

/// Parse one model turn: `{"preamble": str, "tool_calls":
/// [{name, arguments}]}`. Plain prose is not a call — it fails here so
/// the loop answers it as speech.
pub fn parse_gbnf(text: &str) -> Result<(Vec<ToolCall>, String), ParseError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|error| ParseError::BadJson(error.to_string()))?;
    parse_call_value(&value)
}

/// Shared shape check for both JSON arrivals (strict text and schema
/// driven). `arguments` defaults to `{}`; values pass through untouched.
fn parse_call_value(value: &serde_json::Value) -> Result<(Vec<ToolCall>, String), ParseError> {
    let preamble = value
        .get("preamble")
        .and_then(|preamble| preamble.as_str())
        .unwrap_or("")
        .to_string();
    let calls = value
        .get("tool_calls")
        .and_then(|calls| calls.as_array())
        .ok_or(ParseError::NoCalls)?;
    if calls.is_empty() {
        return Err(ParseError::NoCalls);
    }
    let mut out = Vec::with_capacity(calls.len());
    for call in calls {
        let name = call
            .get("name")
            .and_then(|name| name.as_str())
            .ok_or_else(|| ParseError::BadJson("call without a name".to_string()))?;
        let args = call
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        out.push(ToolCall {
            name: name.to_string(),
            args,
            preamble: preamble.clone(),
        });
    }
    Ok((out, preamble))
}

/// Dexter-shaped XML fallback: `<tool_call><invoke name>…<parameter
/// name>…`, values JSON-or-String. Text before the first block is the
/// preamble. `None` when no block exists (skip, not error). Caps:
/// preamble at most 10k chars, args JSON at most 64 KiB; `'`/`<`/`&`
/// in names refuse (strict shape, never silent).
pub fn parse_xml(text: &str) -> Result<Option<(Vec<ToolCall>, String)>, ParseError> {
    const MAX_ARGS_BYTES: usize = 64 * 1024;
    const MAX_PREAMBLE_CHARS: usize = 10_000;
    let blocks = xml_blocks(text, "tool_call");
    if blocks.is_empty() {
        return Ok(None);
    }
    let first = text
        .find("<tool_call>")
        .or_else(|| text.find("<tool_call "))
        .unwrap_or(0);
    let preamble = text[..first].trim().to_string();
    if preamble.chars().count() > MAX_PREAMBLE_CHARS {
        return Err(ParseError::BadJson(format!(
            "preamble exceeds {MAX_PREAMBLE_CHARS} chars"
        )));
    }
    let mut out = Vec::new();
    let mut args_bytes = 0usize;
    for block in blocks {
        for invoke in xml_tag(&block, "invoke", "name") {
            if invoke.attr.contains(['\'', '<', '&']) {
                return Err(ParseError::BadJson("invoke name rejects '/</&".to_string()));
            }
            let mut args = serde_json::Map::new();
            for param in xml_tag(&invoke.inner, "parameter", "name") {
                if param.attr.contains(['\'', '<', '&']) || param.inner.contains(['<', '&']) {
                    return Err(ParseError::BadJson("parameter rejects '/</&".to_string()));
                }
                let key = param.attr;
                let raw = param.inner;
                args_bytes += key.len() + raw.len();
                if args_bytes > MAX_ARGS_BYTES {
                    return Err(ParseError::BadJson(format!(
                        "args exceed {MAX_ARGS_BYTES} bytes"
                    )));
                }
                let value: serde_json::Value = serde_json::from_str(raw.trim())
                    .unwrap_or_else(|_| serde_json::Value::String(raw.trim().to_string()));
                args.insert(key, value);
            }
            out.push(ToolCall {
                name: invoke.attr,
                args: serde_json::Value::Object(args),
                preamble: preamble.clone(),
            });
        }
    }
    if out.is_empty() {
        return Err(ParseError::NoCalls);
    }
    Ok(Some((out, preamble)))
}

struct Tagged {
    attr: String,
    inner: String,
}

/// Inner texts of `<tag attr="v">…</tag>` with the attribute value.
fn xml_tag(block: &str, tag: &str, attr: &str) -> Vec<Tagged> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(start) = rest.find(open.as_str()) {
        let head = &rest[start..];
        let Some(tag_end) = head.find('>') else { break };
        let head_text = &head[..tag_end];
        let needle = format!("{attr}=\"");
        let Some(attr_at) = head_text.find(needle.as_str()) else {
            break;
        };
        let value_start = attr_at + needle.len();
        let Some(value_end) = head_text[value_start..].find('"') else {
            break;
        };
        let value = head_text[value_start..value_start + value_end].to_string();
        let inner_start = start + tag_end + 1;
        let Some(inner_end) = rest[inner_start..].find(close.as_str()) else {
            break;
        };
        out.push(Tagged {
            attr: value,
            inner: rest[inner_start..inner_start + inner_end].to_string(),
        });
        rest = &rest[inner_start + inner_end + close.len()..];
    }
    out
}

/// Inner texts of `<tag>…</tag>` blocks (attributes on the outer tag
/// tolerated and ignored).
fn xml_blocks(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(open.as_str()) {
        let head = &rest[start..];
        let Some(tag_end) = head.find('>') else { break };
        let inner_start = start + tag_end + 1;
        let Some(inner_end) = rest[inner_start..].find(close.as_str()) else {
            break;
        };
        out.push(rest[inner_start..inner_start + inner_end].to_string());
        rest = &rest[inner_start + inner_end + close.len()..];
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsePath {
    Gbnf,
    Xml,
}

/// GBNF primary, XML fallback. Grammar-shaped output parses on the first
/// path; small-model prose with XML blocks parses on the second. Total
/// prose fails on both and reports the grammar error.
pub fn parse_model_output(text: &str) -> Result<(Vec<ToolCall>, String), ParseError> {
    let found = first_accept_in_order_sync(
        [ParsePath::Gbnf, ParsePath::Xml],
        |path| match path {
            ParsePath::Gbnf => parse_gbnf(text).map(Some),
            ParsePath::Xml => parse_xml(text),
        },
        |_: &ParseError| false,
    );
    match found {
        Ok(Some(parsed)) => Ok(parsed),
        Ok(None) => Err(ParseError::NoCalls),
        Err(error) => Err(error),
    }
}

/// Emit the grammar constraining output to `{"preamble": str,
/// "tool_calls": [{name: <registry names>, arguments: <json>}]}.
/// Names are literals from the registry; arguments stay open JSON so
/// one grammar serves every schema without per-tool rules.
#[must_use]
pub fn gbnf_for_tools(names: &[&str]) -> String {
    let mut grammar = String::from(
        "root ::= \"{\\\"preamble\\\": \" string \", \\\"tool_calls\\\": \" calls \"}\"\n",
    );
    grammar.push_str("calls ::= \"[]\" | \"[\" call (\", \" call)* \"]\"\n");
    grammar.push_str("call ::= \"{\\\"name\\\": \" name \", \\\"arguments\\\": \" value \"}\"\n");
    grammar.push_str("name ::= ");
    if names.is_empty() {
        grammar.push_str("\"\"");
    } else {
        let options: Vec<String> = names
            .iter()
            .map(|name| format!("\"\\\"{name}\\\"\""))
            .collect();
        grammar.push_str(&options.join(" | "));
    }
    grammar.push_str(
        "\nvalue ::= object | array | string | number | (\"true\" | \"false\" | \"null\") ws\n",
    );
    grammar.push_str(
        "object ::= \"{\" ws (string \":\" ws value (\",\" ws string \":\" ws value)*)? \"}\" ws\n",
    );
    grammar.push_str("array ::= \"[\" ws (value (\",\" ws value)*)? \"]\" ws\n");
    grammar.push_str("string ::= \"\\\"\" ([^\"\\\\\\x7F\\x00-\\x1F] | \"\\\\\" ([\"\\\\/bfnrt] | \"u\" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F]))* \"\\\"\" ws\n");
    grammar.push_str(
        "number ::= (\"-\"? ([0-9] | [1-9] [0-9]*)) (\".\" [0-9]+)? ([eE] [-+]? [0-9]+)? ws\n",
    );
    grammar.push_str("ws ::= ([ \\t\\n] ws)?\n");
    grammar
}

#[cfg(test)]
#[path = "../tests/parse.rs"]
mod tests;
