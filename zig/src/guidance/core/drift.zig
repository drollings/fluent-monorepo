//! core/drift.zig — Drift follow-up suggestion logic.
//!
//! Consolidates:
//!   - query_engine.zig:computeDriftFollowUps()
//!   - query_engine.zig:tokenizeCapabilityWords()
//!
//! Used to suggest follow-up queries when search results drift from the
//! query intent (words in query not covered by result module/symbol names).

const std = @import("std");
const common = @import("common");
const core_ranking = @import("ranking.zig");

pub const SearchResult = core_ranking.SearchResult;

const StringInterner = common.interner.StringInterner;
const BitSetDrift = common.drift.BitSetDrift;

/// Stop-words filtered out when tokenizing capability text.
pub const stop_words = [_][]const u8{
    "the",  "a",    "an",   "is",   "in",  "of",   "to",   "for",  "and",   "or",
    "with", "from", "that", "this", "how", "does", "what", "when", "where", "why",
    "use",  "get",  "set",  "its",  "are", "not",
};

/// Split text into lowercase tokens, filtering short tokens and stop-words.
/// Appended to `out`; each token is allocator-owned.
pub fn tokenizeCapabilityWords(
    allocator: std.mem.Allocator,
    text: []const u8,
    out: *std.ArrayList([]const u8),
) !void {
    var it = std.mem.tokenizeAny(u8, text, " \t\n\r_-./");
    while (it.next()) |raw| {
        if (raw.len < 3) continue;
        const lower = try std.ascii.allocLowerString(allocator, raw);
        var is_stop = false;
        for (stop_words) |sw| {
            if (std.mem.eql(u8, lower, sw)) {
                is_stop = true;
                break;
            }
        }
        if (is_stop) {
            allocator.free(lower);
            continue;
        }
        try out.append(allocator, lower);
    }
}

/// Compute drift follow-up query terms: words in `query_text` not covered by
/// result module/symbol names. Returns a slice of suggestion strings.
/// Result slice and all inner strings are allocator-owned; caller frees.
pub fn computeDriftFollowUps(
    allocator: std.mem.Allocator,
    query_text: []const u8,
    results: []const SearchResult,
) ![]const []const u8 {
    var interner = StringInterner.init(allocator);
    defer interner.deinit();

    // Collect needed words (from query)
    var needed_words: std.ArrayList([]const u8) = .empty;
    defer {
        for (needed_words.items) |w| allocator.free(w);
        needed_words.deinit(allocator);
    }
    try tokenizeCapabilityWords(allocator, query_text, &needed_words);

    if (needed_words.items.len == 0) return try allocator.alloc([]const u8, 0);

    // Collect available words (from result modules and symbol names)
    var avail_words: std.ArrayList([]const u8) = .empty;
    defer {
        for (avail_words.items) |w| allocator.free(w);
        avail_words.deinit(allocator);
    }
    for (results) |r| {
        try tokenizeCapabilityWords(allocator, r.module, &avail_words);
        try tokenizeCapabilityWords(allocator, r.name, &avail_words);
    }

    // Intern all words first to fix total capacity
    for (needed_words.items) |w| _ = try interner.intern(w);
    for (avail_words.items) |w| _ = try interner.intern(w);
    const cap = @max(1, interner.count());

    // Build needed bitset
    var needed_bs = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, cap);
    defer needed_bs.deinit(allocator);
    for (needed_words.items) |w| {
        if (interner.getIndex(w)) |idx| needed_bs.set(idx);
    }

    // Build available bitset
    var avail_bs = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, cap);
    defer avail_bs.deinit(allocator);
    for (avail_words.items) |w| {
        if (interner.getIndex(w)) |idx| avail_bs.set(idx);
    }

    const drift = BitSetDrift{ .interner = &interner };
    return try drift.generateFollowUps(allocator, &needed_bs, &avail_bs);
}
