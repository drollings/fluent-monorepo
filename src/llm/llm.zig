//! llm.zig — LLM response post-processing for guidance and coral.
//!
//! This file is part of the `llm` build module.
//! It provides post-processing functions for LLM output:
//!   stripThinkBlock, extractCommentTag, isMalformedResponse, stripPreamble

const std = @import("std");

/// Checks if a needle substring exists within the haystack, ignoring case sensitivity.
fn containsIgnoreCase(haystack: []const u8, needle: []const u8) bool {
    if (needle.len == 0) return true;
    if (needle.len > haystack.len) return false;
    var i: usize = 0;
    while (i + needle.len <= haystack.len) : (i += 1) {
        if (std.ascii.eqlIgnoreCase(haystack[i .. i + needle.len], needle)) return true;
    }
    return false;
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Removes tags from a Zig string slice, returning a cleaned version.
fn stripTagBlock(text: []const u8, open: []const u8, close: []const u8) []const u8 {
    const tag_start = std.mem.indexOf(u8, text, open) orelse return text;
    if (std.mem.indexOfPos(u8, text, tag_start + open.len, close)) |close_start| {
        const after = close_start + close.len;
        if (after >= text.len) return "";
        var s = after;
        while (s < text.len and (text[s] == ' ' or text[s] == '\n')) s += 1;
        return text[s..];
    }
    return std.mem.trim(u8, text[0..tag_start], " \t\r\n");
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Strips think-block tags from LLM output (e.g. <think>reasoning</think>).
pub fn stripThinkBlock(text: []const u8) []const u8 {
    if (std.mem.indexOf(u8, text, "<think>")) |think_start| {
        const think_end = std.mem.indexOfPos(u8, text, think_start + 7, "</think>");
        if (think_end) |te| {
            const after = te + 8;
            if (after >= text.len) return "";
            var start = after;
            while (start < text.len and (text[start] == ' ' or text[start] == '\n')) start += 1;
            return text[start..];
        }
        return std.mem.trim(u8, text[0..think_start], " \t\r\n");
    }
    if (std.mem.indexOf(u8, text, "[THINK]")) |think_start| {
        const think_end = std.mem.indexOfPos(u8, text, think_start + 7, "[/THINK]");
        if (think_end) |te| {
            const after = te + 8;
            if (after >= text.len) return "";
            var start = after;
            while (start < text.len and (text[start] == ' ' or text[start] == '\n')) start += 1;
            return text[start..];
        }
        return std.mem.trim(u8, text[0..think_start], " \t\r\n");
    }
    return text;
}

/// Removes leading preamble lines from LLM output.
pub fn stripPreamble(allocator: std.mem.Allocator, text: []const u8) ![]const u8 {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return allocator.dupe(u8, trimmed);

    const nl_pos = std.mem.indexOfScalar(u8, trimmed, '\n') orelse trimmed.len;
    const first_line = trimmed[0..nl_pos];

    const preambles = [_][]const u8{
        "let's ", "let me ", "we need to ",    "here's ",    "here is ",
        "i'll ",  "i will ", "the answer is ", "to answer ", "okay, ",
        "ok, ",   "sure, ",  "alright, ",
    };

    const first_lower = try std.ascii.allocLowerString(allocator, first_line);
    defer allocator.free(first_lower);

    for (preambles) |preamble| {
        if (std.mem.startsWith(u8, first_lower, preamble)) {
            if (nl_pos >= trimmed.len) return allocator.dupe(u8, "");
            const rest = std.mem.trim(u8, trimmed[nl_pos + 1 ..], " \t\r\n");
            return allocator.dupe(u8, rest);
        }
    }

    return allocator.dupe(u8, trimmed);
}

/// Patterns that indicate an LLM preamble rather than a usable doc comment.
const llm_preamble_patterns = [_][]const u8{
    "here's a",    "here is a",   "i'll ",        "to summarize",
    "okay,",       "ok,",         "we need ",     "let's think",
    "let's craft", "let's count", "let me think", "i need to ",
};

/// Returns true if the LLM response appears malformed.
pub fn isMalformedResponse(text: []const u8) bool {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return true;
    if (llmHasDanglingEnd(trimmed)) return true;

    const rtrimmed = std.mem.trimRight(u8, trimmed, " \t");
    if (rtrimmed.len > 0 and rtrimmed[rtrimmed.len - 1] == '?') return true;

    if (llmIsGenericSelfRef(trimmed)) return true;
    if (llmIsOverlyGeneric(trimmed)) return true;

    inline for (llm_preamble_patterns) |pattern| {
        if (containsIgnoreCase(trimmed, pattern)) return true;
    }
    return false;
}

/// Checks if a Zig code snippet ends with a null byte, indicating a dangling end.
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

/// Checks if a Zig structure represents a valid self-referential LLM model.
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

/// Checks if the provided body is overly generic by evaluating its structure and returns true if it matches.
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

/// Extracts content from <comment> tags in LLM output.
pub fn extractCommentTag(text: []const u8) ?[]const u8 {
    const open = "<comment>";
    const close = "</comment>";
    const start = std.mem.indexOf(u8, text, open) orelse return null;
    const content_start = start + open.len;
    const end = std.mem.indexOfPos(u8, text, content_start, close) orelse return null;
    const content = std.mem.trim(u8, text[content_start..end], " \t\r\n");
    if (content.len == 0) return null;
    return content;
}

/// Returns true if text is blank or a plausible doc comment.
pub fn isBlankOrPlausible(text: []const u8) bool {
    const trimmed = std.mem.trim(u8, text, " \t\r\n");
    if (trimmed.len == 0) return true;
    if (trimmed.len < 3) return false;
    return !isMalformedResponse(trimmed);
}

// ── String utilities ────────────────────────────────────────────────────────
// Note: String utilities (looksLikeIdentifier, langFromPath, etc.) are
// available via @import("common").string or directly from common module.
// llm.zig only needs containsIgnoreCase which is defined locally above.

// ── Tests ───────────────────────────────────────────────────────────────────

test "stripThinkBlock removes think tags" {
    const text1 = "<think>Some thinking</think>\nActual response";
    try std.testing.expectEqualStrings("Actual response", stripThinkBlock(text1));

    const text2 = "No think tags here";
    try std.testing.expectEqualStrings(text2, stripThinkBlock(text2));

    const text3 = "<think>Only think\n</think>";
    try std.testing.expectEqualStrings("", stripThinkBlock(text3));
}

test "stripThinkBlock handles unclosed think tag" {
    try std.testing.expectEqualStrings("", stripThinkBlock("<think>Reasoning that never ends"));
}

test "stripThinkBlock handles [THINK] tags" {
    try std.testing.expectEqualStrings("Actual answer", stripThinkBlock("[THINK]reasoning here[/THINK]\nActual answer"));
}

test "stripPreamble removes leading preamble line" {
    const allocator = std.testing.allocator;

    const r1 = try stripPreamble(allocator, "Let's analyze this function.\nParses JSON from a byte slice.");
    defer allocator.free(r1);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", r1);

    const r2 = try stripPreamble(allocator, "Here's the description:\nBuilds the dep graph.");
    defer allocator.free(r2);
    try std.testing.expectEqualStrings("Builds the dep graph.", r2);

    const r3 = try stripPreamble(allocator, "Parses JSON tokens efficiently.");
    defer allocator.free(r3);
    try std.testing.expectEqualStrings("Parses JSON tokens efficiently.", r3);
}

test "isMalformedResponse: empty is malformed" {
    try std.testing.expect(isMalformedResponse(""));
    try std.testing.expect(isMalformedResponse("   "));
}

test "isMalformedResponse: dangling preposition" {
    try std.testing.expect(isMalformedResponse("Parses the input from"));
    try std.testing.expect(isMalformedResponse("Returns the value of"));
}

test "isMalformedResponse: ends with question mark" {
    try std.testing.expect(isMalformedResponse("Does something?"));
}

test "isMalformedResponse: generic self-reference" {
    try std.testing.expect(isMalformedResponse("this function"));
    try std.testing.expect(isMalformedResponse("This Method"));
    try std.testing.expect(!isMalformedResponse("this function parses JSON efficiently"));
}

test "isMalformedResponse: overly generic single word" {
    try std.testing.expect(isMalformedResponse("helper"));
    try std.testing.expect(isMalformedResponse("wrapper"));
    try std.testing.expect(!isMalformedResponse("Parses"));
}

test "isMalformedResponse: LLM preamble phrases" {
    try std.testing.expect(isMalformedResponse("Here's a description of the function"));
    try std.testing.expect(isMalformedResponse("I'll explain what this does"));
    try std.testing.expect(isMalformedResponse("To summarize, this parses JSON"));
    try std.testing.expect(isMalformedResponse("Okay, so this function"));
}

test "isMalformedResponse: valid responses are NOT malformed" {
    try std.testing.expect(!isMalformedResponse("Parses Zig AST tokens and extracts public members."));
    try std.testing.expect(!isMalformedResponse("Ring buffer for streaming price data."));
    try std.testing.expect(!isMalformedResponse("Builds incremental dependency graph from @import declarations."));
}

test "isMalformedResponse: reasoning-model chain-of-thought phrases" {
    try std.testing.expect(isMalformedResponse("we need to write a comment for this type"));
    try std.testing.expect(isMalformedResponse("let's think about what this does"));
    try std.testing.expect(isMalformedResponse("let me think about the ownership model"));
    try std.testing.expect(isMalformedResponse("i need to mention that it owns the allocator"));
}

test "extractCommentTag: returns tag content" {
    const text = "some reasoning\n<comment>Parses JSON from a byte slice.</comment>\nmore text";
    const result = extractCommentTag(text);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", result.?);
}

test "extractCommentTag: returns null when no tag present" {
    try std.testing.expect(extractCommentTag("we need to write a comment") == null);
    try std.testing.expect(extractCommentTag("") == null);
}

test "extractCommentTag: returns null for empty tag" {
    try std.testing.expect(extractCommentTag("<comment>   </comment>") == null);
}
