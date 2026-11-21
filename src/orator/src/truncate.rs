//! One char-count truncator for every tool output and error path.
//!
//! Single home: tool loops, fetchers, and session error trims compose
//! this instead of copying private variants. The marker carries the
//! total length so the cut is loud, never silent.

/// Shared tool-output cap in chars. Every truncation goes through
/// [`truncate_tool_output`]; the marker carries the total length.
pub const TOOL_OUTPUT_CHARS: usize = 6000;

/// Cap `text` at [`TOOL_OUTPUT_CHARS`] chars with a total-length marker.
/// Small outputs pass through verbatim.
#[must_use]
pub fn truncate_tool_output(text: &str) -> String {
    common_core::string::truncate_chars_with_suffix(text, TOOL_OUTPUT_CHARS, |total| {
        format!("\n(truncated, {total} total chars)")
    })
}

#[cfg(test)]
#[path = "../tests/truncate.rs"]
mod tests;
