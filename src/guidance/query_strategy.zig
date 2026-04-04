//! query_strategy.zig — QueryStrategy VTable for intent-based query routing.
//!
//! Implements M2 of ROADMAP_20260403_FLUENT_GUIDANCE.md:
//! Classify query intent before retrieval and route to the optimal strategy.
//!
//! VTable pattern follows fluent-wvr (see doc/skills/fluent-wvr/SKILL.md):
//!   {ptr: *anyopaque, vtable: *const VTable} — two pointers, no inheritance.
//!
//! Strategy priority (lower = tried first):
//!   0  IdentifierLookupStrategy  — single-token camelCase/PascalCase/snake_case
//!   2  CapabilityQueryStrategy   — query matches a known capability/alias
//!   4  ConceptQueryStrategy      — fallback: hybrid search + LLM synthesis

const std = @import("std");
const types = @import("types.zig");
const vector_db_mod = @import("vector");
const staged_mod = @import("staged.zig");

const GuidanceDb = vector_db_mod.GuidanceDb;
const SearchResult = GuidanceDb.SearchResult;

// =============================================================================
// QueryIntent enum
// =============================================================================

/// Classifies the intent of a query for strategy routing.
pub const QueryIntent = enum {
    /// Single identifier: "cmdExplain", "GuidanceDb", "embed"
    identifier_lookup,
    /// Matches known capability name or alias: "database", "sync guidance"
    capability_query,
    /// "How does X work?", "What design patterns are used?" — conceptual multi-word
    concept_query,
    /// Fallback for everything else
    general_search,
};

// =============================================================================
// QueryStrategy VTable
// =============================================================================

/// Polymorphic query routing interface.
/// Each strategy determines if it applies to a query, then executes it.
/// Two pointers, no inheritance — fluent-wvr pattern.
pub const QueryStrategy = struct {
    ptr: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        /// Does this strategy apply to the given query?
        /// Deterministic — no allocations allowed.
        matches: *const fn (ptr: *anyopaque, query: []const u8, db: *GuidanceDb) bool,

        /// Execute the strategy, return owned stage slice.
        /// Caller must free with types.freeStages() + allocator.free().
        execute: *const fn (
            ptr: *anyopaque,
            allocator: std.mem.Allocator,
            db: *GuidanceDb,
            query: []const u8,
            original_query: []const u8,
            workspace: []const u8,
            aliases: ?vector_db_mod.SemanticAliases,
        ) anyerror![]types.Stage,

        /// Intent this strategy handles (for diagnostics).
        intent: QueryIntent,

        /// Priority (lower = tried first). Strategies are sorted at build time.
        priority: u8,
    };

    pub fn matches(self: QueryStrategy, query: []const u8, db: *GuidanceDb) bool {
        return self.vtable.matches(self.ptr, query, db);
    }

    pub fn execute(
        self: QueryStrategy,
        allocator: std.mem.Allocator,
        db: *GuidanceDb,
        query: []const u8,
        original_query: []const u8,
        workspace: []const u8,
        aliases: ?vector_db_mod.SemanticAliases,
    ) ![]types.Stage {
        return self.vtable.execute(self.ptr, allocator, db, query, original_query, workspace, aliases);
    }

    pub fn intent(self: QueryStrategy) QueryIntent {
        return self.vtable.intent;
    }

    pub fn priority(self: QueryStrategy) u8 {
        return self.vtable.priority;
    }
};

// =============================================================================
// Strategy 1: IdentifierLookupStrategy (priority 0)
// =============================================================================
//
// Matches single-token queries that look like identifiers: camelCase, PascalCase,
// snake_case. Routes through the staged pipeline like all other queries,
// ensuring keyword queries get the same LLM synthesis and capability integration
// as natural language queries. The staged pipeline automatically prioritizes
// exact name matches in its scoring.

/// Implementation struct for identifier lookup.
pub const IdentifierLookupStrategy = struct {
    // Stateless — no fields needed.

    pub fn strategy(self: *IdentifierLookupStrategy) QueryStrategy {
        return .{ .ptr = self, .vtable = &identifier_vtable };
    }
};

/// Checks if query strings match database entries, returning true or false.
fn identifierMatches(ptr: *anyopaque, query: []const u8, db: *GuidanceDb) bool {
    _ = ptr;
    _ = db;
    return looksLikeIdentifier(query);
}

/// Executes an identifier query in the GuidanceDb, returning results or errors.
fn identifierExecute(
    ptr: *anyopaque,
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    original_query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
) anyerror![]types.Stage {
    _ = ptr;
    // Delegate to the staged pipeline for unified processing.
    // The staged pipeline handles both keyword and natural language queries,
    // prioritizing exact name matches and providing consistent LLM synthesis.
    // This ensures keyword queries get the same rich output: source code,
    // capabilities, skills, metadata, and LLM-synthesized explanations.
    return staged_mod.executeStagedWithAliasesOriginal(
        allocator,
        db,
        query,
        original_query,
        workspace,
        aliases,
    );
}

const identifier_vtable: QueryStrategy.VTable = .{
    .matches = identifierMatches,
    .execute = identifierExecute,
    .intent = .identifier_lookup,
    .priority = 0,
};

// =============================================================================
// Strategy 2: CapabilityQueryStrategy (priority 2)
// =============================================================================
//
// Matches queries whose tokens correspond to known capability names or semantic
// aliases. Routes to staged pipeline (which already handles capability routing
// well) — this strategy primarily improves intent logging.

pub const CapabilityQueryStrategy = struct {
    pub fn strategy(self: *CapabilityQueryStrategy) QueryStrategy {
        return .{ .ptr = self, .vtable = &capability_vtable };
    }
};

/// Checks if a query matches capability data in the GuidanceDb, returning true or false.
fn capabilityMatches(ptr: *anyopaque, query: []const u8, db: *GuidanceDb) bool {
    _ = ptr;
    // Query is a capability match if findMatchedCapabilityNamesForQuery returns ≥ 1 result
    // with score > 0.5. We use a stack allocator approximation: check if query is
    // multi-word and contains known capability-related terms.
    // Full capability matching is done in executeStagedWithAliasesOriginal.
    _ = db;
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    // Only route through capability strategy for multi-word queries that look like
    // capability names (not single identifiers, which go through identifier strategy).
    var tok_count: usize = 0;
    var it = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    while (it.next()) |_| tok_count += 1;
    return tok_count >= 2 and tok_count <= 4 and !looksLikeNaturalLanguageQuestion(trimmed);
}

/// Executes a capability query, accepting parameters for database, query, workspace, and aliases, returning results or errors.
fn capabilityExecute(
    ptr: *anyopaque,
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    original_query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
) anyerror![]types.Stage {
    _ = ptr;
    // Delegate to staged pipeline — it already handles capability routing well.
    return staged_mod.executeStagedWithAliasesOriginal(
        allocator,
        db,
        query,
        original_query,
        workspace,
        aliases,
    );
}

const capability_vtable: QueryStrategy.VTable = .{
    .matches = capabilityMatches,
    .execute = capabilityExecute,
    .intent = .capability_query,
    .priority = 2,
};

// =============================================================================
// Strategy 3: ConceptQueryStrategy (priority 4, fallback)
// =============================================================================
//
// Matches multi-word natural language questions: "How does X work?",
// "What design patterns are used?". Delegates to staged pipeline + LLM synthesis.

pub const ConceptQueryStrategy = struct {
    pub fn strategy(self: *ConceptQueryStrategy) QueryStrategy {
        return .{ .ptr = self, .vtable = &concept_vtable };
    }
};

/// Checks if a query matches a concept using a pointer, query slice, and database.
fn conceptMatches(ptr: *anyopaque, query: []const u8, db: *GuidanceDb) bool {
    _ = ptr;
    _ = db;
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    var tok_count: usize = 0;
    var it = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    while (it.next()) |_| tok_count += 1;
    return tok_count >= 2;
}

/// Executes a query using provided parameters and returns results or errors.
fn conceptExecute(
    ptr: *anyopaque,
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    original_query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
) anyerror![]types.Stage {
    _ = ptr;
    return staged_mod.executeStagedWithAliasesOriginal(
        allocator,
        db,
        query,
        original_query,
        workspace,
        aliases,
    );
}

const concept_vtable: QueryStrategy.VTable = .{
    .matches = conceptMatches,
    .execute = conceptExecute,
    .intent = .concept_query,
    .priority = 4,
};

// =============================================================================
// Strategy dispatcher
// =============================================================================

/// Dispatch query through strategies in priority order.
/// Falls back to staged pipeline if no strategy matches.
pub fn executeWithStrategy(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query: []const u8,
    original_query: []const u8,
    workspace: []const u8,
    aliases: ?vector_db_mod.SemanticAliases,
    strategies: []const QueryStrategy,
) ![]types.Stage {
    // Strategies should already be sorted by priority at build time.
    for (strategies) |s| {
        if (s.matches(query, db)) {
            return s.execute(allocator, db, query, original_query, workspace, aliases);
        }
    }
    // Ultimate fallback: staged pipeline.
    return staged_mod.executeStagedWithAliasesOriginal(
        allocator,
        db,
        query,
        original_query,
        workspace,
        aliases,
    );
}

/// Build the default strategy list (sorted by priority).
/// Call once during initialization; strategies are stateless so no deinit needed.
pub fn buildDefaultStrategies(
    identifier: *IdentifierLookupStrategy,
    capability: *CapabilityQueryStrategy,
    concept: *ConceptQueryStrategy,
) [3]QueryStrategy {
    return .{
        identifier.strategy(), // priority 0
        capability.strategy(), // priority 2
        concept.strategy(), // priority 4
    };
}

// =============================================================================
// Helpers
// =============================================================================

/// Returns true if the query looks like a single identifier token
/// (camelCase, PascalCase, snake_case, or a single word).
pub fn looksLikeIdentifier(query: []const u8) bool {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len < 2 or trimmed.len > 64) return false;

    // Must be a single token (no spaces)
    if (std.mem.indexOfAny(u8, trimmed, " \t\n\r") != null) return false;

    // Must start with letter or underscore
    if (!std.ascii.isAlphabetic(trimmed[0]) and trimmed[0] != '_') return false;

    // Must contain only alphanumeric + underscore
    for (trimmed) |c| {
        if (!std.ascii.isAlphanumeric(c) and c != '_') return false;
    }

    return true;
}

/// Returns true if the query looks like a natural language question.
fn looksLikeNaturalLanguageQuestion(query: []const u8) bool {
    const nl_prefixes = [_][]const u8{
        "how ",  "what ",    "where ",    "why ",  "when ", "which ",
        "show ", "explain ", "describe ", "find ", "list ",
    };
    // Case-insensitive prefix check using a stack buffer (no allocator needed).
    var buf: [128]u8 = undefined;
    const copy_len = @min(query.len, buf.len);
    const lower = std.ascii.lowerString(buf[0..copy_len], query[0..copy_len]);
    for (nl_prefixes) |prefix| {
        if (std.mem.startsWith(u8, lower, prefix)) return true;
    }
    if (std.mem.endsWith(u8, std.mem.trim(u8, query, " \t"), "?")) return true;
    return false;
}

// =============================================================================
// Tests
// =============================================================================

test "looksLikeIdentifier: single camelCase identifier" {
    try std.testing.expect(looksLikeIdentifier("cmdExplain"));
    try std.testing.expect(looksLikeIdentifier("GuidanceDb"));
    try std.testing.expect(looksLikeIdentifier("executeStaged"));
    try std.testing.expect(looksLikeIdentifier("_private"));
}

test "looksLikeIdentifier: rejects multi-word queries" {
    try std.testing.expect(!looksLikeIdentifier("sync guidance"));
    try std.testing.expect(!looksLikeIdentifier("AST parser"));
    try std.testing.expect(!looksLikeIdentifier("How does this work?"));
}

test "looksLikeIdentifier: rejects empty and too-long" {
    try std.testing.expect(!looksLikeIdentifier(""));
    try std.testing.expect(!looksLikeIdentifier("a"));
    // 65 chars — too long
    try std.testing.expect(!looksLikeIdentifier("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
}
