//! query_strategy.zig — Query routing by intent.
//!
//! Strategy priority (lower = tried first):
//!   0  identifier lookup  — single-token camelCase/PascalCase/snake_case
//!   2  capability query    — query matches a known capability/alias
//!   4  concept query       — fallback: hybrid search + LLM synthesis
//!
//! Anti-pattern fixed: all three execute() implementations were identical.
//! Replaced with QueryMatch (matches-only function pointer) + single executeQuery.

const std = @import("std");
const types = @import("../types.zig");
const vector_db_mod = @import("vector");
const staged_mod = @import("../staged.zig");
const core_intent = @import("../core/intent.zig");
const identifier = @import("identifier.zig");

const GuidanceDb = vector_db_mod.GuidanceDb;
const SearchResult = GuidanceDb.SearchResult;

// =============================================================================
// QueryIntent enum
// =============================================================================

pub const QueryIntent = enum {
    identifier_lookup,
    capability_query,
    concept_query,
    general_search,
};

// =============================================================================
// QueryMatch — lightweight match predicate for intent routing
// =============================================================================

pub const QueryMatch = struct {
    matches: *const fn (query: []const u8, db: *GuidanceDb) bool,
    intent: QueryIntent,
    priority: u8,
};

fn queryMatch(
    comptime matches_fn: *const fn (query: []const u8, db: *GuidanceDb) bool,
    intent: QueryIntent,
    priority: u8,
) QueryMatch {
    return .{
        .matches = matches_fn,
        .intent = intent,
        .priority = priority,
    };
}

// =============================================================================
// Matches predicates (stateless — no ptr needed)
// =============================================================================

fn identifierMatches(query: []const u8, db: *GuidanceDb) bool {
    _ = db;
    return identifier.detectIdentifierPattern(query) != null;
}

fn capabilityMatches(query: []const u8, db: *GuidanceDb) bool {
    _ = db;
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    var tok_count: usize = 0;
    var it = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    while (it.next()) |_| tok_count += 1;
    return tok_count >= 2 and tok_count <= 4 and !core_intent.isNaturalLanguageQuery(trimmed);
}

fn conceptMatches(query: []const u8, db: *GuidanceDb) bool {
    _ = db;
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    var tok_count: usize = 0;
    var it = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    while (it.next()) |_| tok_count += 1;
    return tok_count >= 2;
}

// =============================================================================
// Query dispatcher
// =============================================================================

pub fn executeQuery(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    config: staged_mod.StagedConfig,
) ![]types.Stage {
    return staged_mod.executeStagedConfig(allocator, db, config);
}

pub fn executeQueryWithMatch(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    original_query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
    matches: []const QueryMatch,
) ![]types.Stage {
    const config: staged_mod.StagedConfig = .{
        .query = query,
        .original_query = original_query,
        .workspace = workspace,
        .aliases = aliases,
    };
    for (matches) |m| {
        if (m.matches(query, db)) {
            return executeQuery(allocator, db, config);
        }
    }
    return staged_mod.executeStagedConfig(allocator, db, config);
}

pub fn buildDefaultStrategies() [3]QueryMatch {
    return .{
        queryMatch(identifierMatches, .identifier_lookup, 0),
        queryMatch(capabilityMatches, .capability_query, 2),
        queryMatch(conceptMatches, .concept_query, 4),
    };
}

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;

test "QueryMatch priority ordering" {
    const strategies = buildDefaultStrategies();
    try testing.expect(strategies[0].priority < strategies[1].priority);
    try testing.expect(strategies[1].priority < strategies[2].priority);
}
