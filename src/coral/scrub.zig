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
/// Note: isMalformedResponse logic is kept in sync with common/llm.zig.
const std = @import("std");
const string = @import("common");

/// Returns `true` if the comment text appears to be malformed/incomplete
/// (from LLM generation). Synced with common/llm.zig isMalformedResponse().
pub fn isMalformedResponse(text: []const u8) bool {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return true;

    if (llmHasDanglingEnd(trimmed)) return true;

    const rtrimmed = std.mem.trimRight(u8, trimmed, " \t");
    if (rtrimmed.len > 0 and rtrimmed[rtrimmed.len - 1] == '?') return true;

    if (llmIsGenericSelfRef(trimmed)) return true;
    if (llmIsOverlyGeneric(trimmed)) return true;

    if (string.containsIgnoreCase(trimmed, "here's a")) return true;
    if (string.containsIgnoreCase(trimmed, "here is a")) return true;
    if (string.containsIgnoreCase(trimmed, "i'll ")) return true;
    if (string.containsIgnoreCase(trimmed, "to summarize")) return true;
    if (string.containsIgnoreCase(trimmed, "okay,")) return true;
    if (string.containsIgnoreCase(trimmed, "ok,")) return true;

    if (string.containsIgnoreCase(trimmed, "we need ")) return true;
    if (string.containsIgnoreCase(trimmed, "let's think")) return true;
    if (string.containsIgnoreCase(trimmed, "let's craft")) return true;
    if (string.containsIgnoreCase(trimmed, "let's count")) return true;
    if (string.containsIgnoreCase(trimmed, "let me think")) return true;
    if (string.containsIgnoreCase(trimmed, "i need to ")) return true;

    return false;
}

fn llmHasDanglingEnd(body: []const u8) bool {
    const trimmed = std.mem.trimRight(u8, body, " \t.?");
    if (trimmed.len == 0) return false;
    var i: usize = trimmed.len;
    while (i > 0 and trimmed[i - 1] != ' ') i -= 1;
    const last_word = trimmed[i..];
    const danglers = [_][]const u8{ "of", "in", "for", "from", "with", "to", "a", "an", "the" };
    for (danglers) |d| {
        if (std.ascii.eqlIgnoreCase(last_word, d)) return true;
    }
    return false;
}

fn llmIsGenericSelfRef(body: []const u8) bool {
    const patterns = [_][]const u8{
        "this function", "this method", "this class",
        "this struct",   "this type",   "this module",
    };
    const trimmed = std.mem.trim(u8, body, " \t\r\n.");
    for (patterns) |p| {
        if (std.ascii.eqlIgnoreCase(trimmed, p)) return true;
    }
    return false;
}

fn llmIsOverlyGeneric(body: []const u8) bool {
    const generics = [_][]const u8{
        "function", "method",   "helper",  "util",           "utility",
        "handler",  "callback", "wrapper", "implementation",
    };
    const trimmed = std.mem.trim(u8, body, " \t\r\n.");
    if (trimmed.len > 20) return false;
    if (std.mem.indexOfScalar(u8, trimmed, ' ') != null) return false;
    for (generics) |g| {
        if (std.ascii.eqlIgnoreCase(trimmed, g)) return true;
    }
    return false;
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
        if (string.containsIgnoreCase(text, ind)) return true;
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
    try testing.expect(isSyntheticComment("Manages CozoResult instances with ownership, provides slicing and validation; self-manages memory."));
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
