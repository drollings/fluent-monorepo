/// scrub.zig — Comment quality filter for ast-guidance infill pipeline.
///
/// Detects AI-generated comments that should be re-infilled on the next run:
///   - Truncated sentences (dangling prepositions, no terminal punctuation)
///   - LLM preamble phrases ("Here's a ...", "I'll explain ...")
///   - Self-referential generic phrases ("this function", "helper", ...)
///   - Reasoning-model chain-of-thought leakage ("we need to ...", "let's think ...")
///
/// Usage in ast-guidance sync --infill:
///   if (scrub.isSyntheticComment(existing_comment)) {
///       // Re-request infill from the LLM
///   }
///
/// Note: isMalformedResponse uses common.llm functions for LLM validation.
const std = @import("std");
const llm = @import("common");

/// Returns `true` if the comment text appears to be malformed/incomplete
/// (from LLM generation). Delegates to common/llm.isMalformedResponse.
pub fn isMalformedResponse(text: []const u8) bool {
    return llm.isMalformedResponse(text);
}

/// Returns `true` if `comment` is a synthetic placeholder that should be
/// replaced by a fresh LLM infill pass.
pub fn isSyntheticComment(comment: []const u8) bool {
    const trimmed = std.mem.trim(u8, comment, " \t\r\n");

    if (trimmed.len < 10) return true;
    if (isMalformedResponse(trimmed)) return true;
    if (hasBoilerplateManagesPattern(trimmed)) return true;

    return false;
}

/// Checks if the input text contains boilerplate patterns, returning true or false.
fn hasBoilerplateManagesPattern(text: []const u8) bool {
    if (text.len < 8) return false;
    if (!std.ascii.eqlIgnoreCase(text[0..8], "manages ")) return false;

    const lower_indicators = [_][]const u8{
        "with ownership",
        "instances with",
        "lifecycle",
        "ensures safe",
        "self-manages memory",
        "no direct ownership",
    };
    for (lower_indicators) |ind| {
        if (llm.containsIgnoreCase(text, ind)) return true;
    }
    return false;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "isSyntheticComment: empty and short are synthetic" {
    try testing.expect(isSyntheticComment(""));
    try testing.expect(isSyntheticComment("   "));
    try testing.expect(isSyntheticComment("Helper."));
}

test "isSyntheticComment: boilerplate manages pattern" {
    try testing.expect(isSyntheticComment("Manages SqliteResult instances with ownership, provides slicing and validation; self-manages memory."));
    try testing.expect(isSyntheticComment("Manages fixed-size buffer allocations with ownership and lifecycle control; ensures safe initialization/deinit."));
    try testing.expect(!isSyntheticComment("Manages the active REPL session: reads user input, evaluates it, and writes output to stdout."));
}

test "isSyntheticComment: malformed responses are synthetic" {
    try testing.expect(isSyntheticComment("Parses the input from"));
    try testing.expect(isSyntheticComment("here's a description of the function"));
    try testing.expect(isSyntheticComment("we need to write a comment for this"));
}

test "isSyntheticComment: valid comments pass" {
    try testing.expect(!isSyntheticComment("Parses Zig AST tokens and extracts public members."));
    try testing.expect(!isSyntheticComment("Ring buffer for streaming price data with configurable capacity."));
    try testing.expect(!isSyntheticComment("Builds incremental dependency graph from @import declarations."));
}
