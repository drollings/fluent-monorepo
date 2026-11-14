//! Robust parsing for model-generated text (single home for JSON
//! extraction; think-tag stripping delegates to `fluent-llm`).
//!
//! Model output arrives wrapped in prose, think tags, or truncated.
//! Whole-body `serde_json::from_str` fails on all three; callers
//! strip think blocks first, then extract the first balanced JSON
//! value and parse that. `None` means "no JSON present" — callers
//! keep their own fail-open/fail-closed choice.

/// First balanced `{...}` or `[...]` substring, whichever opener comes
/// first. String literals (with `\"` escapes) never open, close, or
/// end the scan. `None` when no opener exists or nothing balances.
#[must_use]
/// Robust parsing for model-generated text (single home for JSON
/// extraction; think-tag stripping delegates to `fluent-llm`).
///
/// Model output arrives wrapped in prose, think tags, or truncated.
/// Whole-body `serde_json::from_str` fails on all three; callers
/// strip think blocks first, then extract the first balanced JSON
/// value and parse that. `None` means "no JSON present" — callers
/// keep their own fail-open/fail-closed choice.
/// First balanced `{...}` or `[...]` substring, whichever opener comes
/// first. String literals (with `\"` escapes) never open, close, or
/// end the scan. `None` when no opener exists or nothing balances.
pub fn extract_json_value(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let open = bytes[cursor];
        if open == b'{' || open == b'[' {
            if let Some(end) = balanced_end(bytes, cursor) {
                return Some(&text[cursor..=end]);
            }
            return None;
        }
        cursor += 1;
    }
    None
}

/// Byte offset of the closer matching the opener at `start`, or `None`.
/// A stack tracks the expected closers, so mixed brackets must nest
/// properly; JSON strings are skipped whole (backslash escapes
/// respected), so brackets inside string literals never count.
fn balanced_end(bytes: &[u8], start: usize) -> Option<usize> {
    debug_assert!(bytes[start] == b'{' || bytes[start] == b'[');
    let mut expected: Vec<u8> = Vec::new();
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 2;
                    } else if bytes[index] == b'"' {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
            }
            b'{' => {
                expected.push(b'}');
                index += 1;
            }
            b'[' => {
                expected.push(b']');
                index += 1;
            }
            b'}' | b']' => {
                if expected.pop() != Some(bytes[index]) {
                    return None;
                }
                index += 1;
                if expected.is_empty() {
                    return Some(index - 1);
                }
            }
            _ => {
                index += 1;
            }
        }
    }
    None
}

/// Strip think/reasoning blocks. Delegates to the shared
/// `fluent-llm` stripper (single home there since its M11; no second
/// implementation here).
#[must_use]
/// Strip think/reasoning blocks. Delegates to the shared
/// `fluent-llm` stripper (single home there since its M11; no second
/// implementation here).
pub fn strip_think_tags(text: &str) -> String {
    fluent_llm::thinking::strip_think_block(text)
}

#[cfg(test)]
#[path = "../tests/model_text.rs"]
mod tests;
