//! core/intent.zig — Deterministic query intent classification.
//!
//! Replaces five overlapping classifiers:
//!   - query_engine.zig:isShortQuery()
//!   - query/identifier.zig:detectIdentifierPattern()
//!   - query/identifier.zig:shouldSkipLLMSynthesis()
//!   - query/strategy.zig:looksLikeNaturalLanguageQuestion()
//!   - skeleton.zig:classifyQuery() TIER classification
//!
//! classifyIntent is allocation-free for the skip_llm=true fast path.
//! For slow paths it arena-allocates the token slice for callers.

const std = @import("std");
const common = @import("common");

/// Escalation path for search tiers.
/// Low tier = deterministic, fast.  High tier = probabilistic, slow.
pub const QueryTier = enum {
    /// O(1) inverted index lookup for single identifiers.
    word_index,
    /// Capability anchor → member lookup.
    anchor_lookup,
    /// SQLite LIKE + position rank.
    fts_keyword,
    /// Reciprocal rank fusion of keyword + vector.
    rrf_merge,
    /// Weighted vector + keyword hybrid.
    hybrid,
    /// Pure cosine similarity — LLM synthesis required.
    vector_only,

    /// First tier to try for a given intent.
    pub fn from(intent: QueryIntent) QueryTier {
        return switch (intent) {
            .single_identifier => .word_index,
            .capability_keyword => .anchor_lookup,
            .file_path => .fts_keyword,
            .how_to, .conceptual => .rrf_merge,
            .multi_keyword => .fts_keyword,
        };
    }
};

/// Linguistic intent, classified deterministically from token pattern.
pub const QueryIntent = enum {
    /// Single identifier: "cmdExplain", "GuidanceDb"
    single_identifier,
    /// Matches capability name or alias: "database", "sync guidance"
    capability_keyword,
    /// Contains "/" or .zig suffix
    file_path,
    /// Starts with question verb or ends with "?"
    how_to,
    /// Multi-word, no identifier pattern
    conceptual,
    /// Multiple tokens, some may be identifiers
    multi_keyword,
};

/// Classification result.
/// `tokens` is arena-allocated when non-empty; freed by arena.deinit().
/// On the fast path (skip_llm=true) tokens is always &.{} — callers
/// that need tokens for that path must tokenize themselves.
pub const Classification = struct {
    intent: QueryIntent,
    confidence: f32,
    /// Arena-owned token slice.  &.{} on skip_llm=true paths.
    tokens: []const []const u8,
    /// True when LLM synthesis can be skipped (deterministic result expected).
    skip_llm: bool,
};

/// Classify a query into a deterministic intent.
/// arena is used only for token slice allocation on slow paths.
/// Fast paths (single_identifier, file_path) return tokens=&.{} and
/// do not allocate from the arena.
pub fn classifyIntent(arena: *std.heap.ArenaAllocator, query: []const u8) Classification {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len == 0) {
        return .{ .intent = .single_identifier, .confidence = 0.0, .tokens = &.{}, .skip_llm = true };
    }

    // Count tokens and scan for structural patterns.
    var token_count: usize = 0;
    var has_path_separator = false;
    var all_identifier_chars = true;

    var tok_it = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    while (tok_it.next()) |t| {
        token_count += 1;
        if (std.mem.indexOfScalar(u8, t, '/') != null) has_path_separator = true;
        for (t) |c| {
            if (!std.ascii.isAlphanumeric(c) and c != '_' and c != '.') {
                all_identifier_chars = false;
            }
        }
    }

    // Fast path: file-path query — no allocation needed.
    if (has_path_separator or std.mem.endsWith(u8, trimmed, ".zig")) {
        return .{ .intent = .file_path, .confidence = 0.95, .tokens = &.{}, .skip_llm = true };
    }

    // Fast path: single identifier — delegate casing check to common helper.
    if (token_count == 1 and all_identifier_chars and common.looksLikeIdentifier(trimmed)) {
        return .{ .intent = .single_identifier, .confidence = 0.9, .tokens = &.{}, .skip_llm = true };
    }

    // Slow paths need tokens for downstream ranking.
    const aa = arena.allocator();
    var token_list: std.ArrayList([]const u8) = .empty;
    var it2 = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    while (it2.next()) |t| token_list.append(aa, t) catch {};
    const tokens = token_list.items;

    // Case-insensitive question-prefix check via stack buffer (no allocation).
    var lower_buf: [128]u8 = undefined;
    const check_len = @min(trimmed.len, lower_buf.len);
    const lower_head = std.ascii.lowerString(lower_buf[0..check_len], trimmed[0..check_len]);

    const nl_prefixes = [_][]const u8{
        "how ",  "what ",    "where ", "why ",      "when ", "which ",
        "show ", "explain ", "find ",  "describe ", "list ", "if ",
        "does ",
    };
    var has_question_prefix = trimmed[trimmed.len - 1] == '?';
    for (nl_prefixes) |prefix| {
        if (std.mem.startsWith(u8, lower_head, prefix)) {
            has_question_prefix = true;
            break;
        }
    }

    if (has_question_prefix) {
        return .{ .intent = .how_to, .confidence = 0.85, .tokens = tokens, .skip_llm = false };
    }

    if (token_count == 2 and all_identifier_chars) {
        return .{ .intent = .multi_keyword, .confidence = 0.8, .tokens = tokens, .skip_llm = false };
    }

    if (token_count >= 2) {
        return .{ .intent = .conceptual, .confidence = 0.7, .tokens = tokens, .skip_llm = false };
    }

    return .{ .intent = .single_identifier, .confidence = 0.5, .tokens = tokens, .skip_llm = false };
}

/// Returns true when LLM synthesis can be skipped for this query.
/// Mirrors the existing isShortQuery() logic in query_engine.zig.
/// A query is "short" (skip LLM) when:
///   - empty / whitespace-only
///   - 1 token and no question mark or question-word prefix
pub fn isShortQuery(query: []const u8) bool {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len == 0) return true;

    // Question mark at end triggers LLM filter.
    if (trimmed[trimmed.len - 1] == '?') return false;

    // Check for question word prefixes (case-insensitive, with trailing space).
    const question_prefixes = [_][]const u8{ "if ", "how ", "where ", "when ", "does ", "why ", "what " };
    for (question_prefixes) |prefix| {
        if (trimmed.len >= prefix.len) {
            const candidate = trimmed[0..prefix.len];
            var i: usize = 0;
            while (i < prefix.len) : (i += 1) {
                if (std.ascii.toLower(candidate[i]) != std.ascii.toLower(prefix[i])) break;
            }
            if (i == prefix.len) return false;
        }
    }

    // Word count: 1 or fewer = short (no LLM filter).
    var tok = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    var count: usize = 0;
    while (tok.next()) |_| {
        count += 1;
        if (count > 1) return false;
    }
    return true;
}

/// Returns true when query has a natural-language question prefix or ends with '?'.
/// Allocation-free — uses a stack buffer for case-insensitive prefix comparison.
pub fn isNaturalLanguageQuery(query: []const u8) bool {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len == 0) return false;
    if (trimmed[trimmed.len - 1] == '?') return true;
    var buf: [128]u8 = undefined;
    const copy_len = @min(trimmed.len, buf.len);
    const lower = std.ascii.lowerString(buf[0..copy_len], trimmed[0..copy_len]);
    const nl_prefixes = [_][]const u8{
        "how ",  "what ",    "where ", "why ",      "when ", "which ",
        "show ", "explain ", "find ",  "describe ", "list ",
    };
    for (nl_prefixes) |prefix| {
        if (std.mem.startsWith(u8, lower, prefix)) return true;
    }
    return false;
}

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;

test "isShortQuery: empty string is short" {
    try testing.expect(isShortQuery(""));
    try testing.expect(isShortQuery("   "));
}

test "isShortQuery: one word is short" {
    try testing.expect(isShortQuery("foo"));
    try testing.expect(isShortQuery("  bar  "));
}

test "isShortQuery: two words is not short" {
    try testing.expect(!isShortQuery("foo bar"));
    try testing.expect(!isShortQuery("one two"));
}

test "isShortQuery: three words is not short" {
    try testing.expect(!isShortQuery("foo bar baz"));
    try testing.expect(!isShortQuery("one two three"));
}

test "isShortQuery: question mark makes it not short" {
    try testing.expect(!isShortQuery("foo?"));
    try testing.expect(!isShortQuery("foo bar?"));
}

test "isShortQuery: question word prefixes make it not short" {
    try testing.expect(!isShortQuery("how does this work"));
    try testing.expect(!isShortQuery("what is foo"));
    try testing.expect(!isShortQuery("where is bar"));
    try testing.expect(!isShortQuery("when does it run"));
    try testing.expect(!isShortQuery("why is this happening"));
    try testing.expect(!isShortQuery("if this happens"));
    try testing.expect(!isShortQuery("does it work"));
}

test "isShortQuery: question words case insensitive" {
    try testing.expect(!isShortQuery("How does this work"));
    try testing.expect(!isShortQuery("WHAT is foo"));
    try testing.expect(!isShortQuery("Where IS bar"));
}

test "isShortQuery: regular two-word queries are not short" {
    try testing.expect(!isShortQuery("sync json"));
    try testing.expect(!isShortQuery("parse file"));
    try testing.expect(!isShortQuery("load config"));
}

test "classifyIntent: single identifier fast path" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const c = classifyIntent(&arena, "cmdExplain");
    try testing.expectEqual(QueryIntent.single_identifier, c.intent);
    try testing.expect(c.skip_llm);
    try testing.expectEqual(@as(usize, 0), c.tokens.len);
}

test "classifyIntent: file path fast path" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const c = classifyIntent(&arena, "src/guidance/staged.zig");
    try testing.expectEqual(QueryIntent.file_path, c.intent);
    try testing.expect(c.skip_llm);
}

test "classifyIntent: how_to question" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const c = classifyIntent(&arena, "how does filterStages work?");
    try testing.expectEqual(QueryIntent.how_to, c.intent);
    try testing.expect(!c.skip_llm);
}

test "classifyIntent: multi_keyword two identifiers" {
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const c = classifyIntent(&arena, "filterStages dupeStage");
    try testing.expectEqual(QueryIntent.multi_keyword, c.intent);
    try testing.expect(!c.skip_llm);
    try testing.expectEqual(@as(usize, 2), c.tokens.len);
}
