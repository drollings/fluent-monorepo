//! identifier_match.zig — Identifier pattern detection for TIER 0/1 query routing.
//!
//! Detects whether a query string is an exact identifier (function, struct, method,
//! file, or module name) rather than natural language.  Identifier queries route to
//! TIER 0 or TIER 1 — deterministic DB lookups — bypassing LLM synthesis entirely.
//!
//! Detection rules (in priority order):
//! 1. Empty / whitespace-only → not an identifier.
//! 2. Contains spaces → not an identifier (identifiers are single tokens).
//! 3. File-path heuristic: contains '/' → .file kind.
//! 4. Dotted path: `module.Member` → .method or .struct based on member case.
//!    Three-segment: `a.b.c` → outer is module, inner is struct, last is method.
//! 5. Single PascalCase word → .struct (or generic type).
//! 6. Single snake_case / camelCase word → .function.
//!
//! §Performance: pure string scanning, no allocations, <1 µs.

const std = @import("std");

pub const IdentifierKind = enum {
    function,
    method,
    /// PascalCase type name (struct, enum, union)
    @"struct",
    file,
    module,
};

pub const IdentifierMatch = struct {
    kind: IdentifierKind,
    /// The final identifier name (e.g. "filterStages", "CSRGraph").
    name: []const u8,
    /// Owning module/namespace prefix, if present (e.g. "cache" from "cache.L1Cache").
    module: ?[]const u8,
    /// Raw file path when kind == .file.
    file: ?[]const u8,
};

/// Checks a byte array for a specific identifier pattern and returns an IdentifierMatch result.
pub fn detectIdentifierPattern(query: []const u8) ?IdentifierMatch {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len == 0) return null;

    // Any space → natural language, not an identifier.
    if (std.mem.indexOfAny(u8, trimmed, " \t\n\r") != null) return null;

    // File-path heuristic: forward slash present.
    if (std.mem.indexOfScalar(u8, trimmed, '/') != null) {
        return .{
            .kind = .file,
            .name = trimmed,
            .module = null,
            .file = trimmed,
        };
    }

    // Dotted path: split on first dot.
    if (std.mem.indexOfScalar(u8, trimmed, '.')) |first_dot| {
        const prefix = trimmed[0..first_dot];
        const rest = trimmed[first_dot + 1 ..];

        // Three-segment: module.Struct.method — look for a second dot in `rest`.
        if (std.mem.indexOfScalar(u8, rest, '.')) |second_dot| {
            const struct_name = rest[0..second_dot];
            const method_name = rest[second_dot + 1 ..];
            return .{
                .kind = .method,
                .name = method_name,
                .module = if (isPascalCase(struct_name)) struct_name else prefix,
                .file = null,
            };
        }

        // Two-segment: module.Member
        return .{
            .kind = if (isPascalCase(rest)) .@"struct" else .function,
            .name = rest,
            .module = prefix,
            .file = null,
        };
    }

    // Single token: classify by casing.
    return .{
        .kind = if (isPascalCase(trimmed)) .@"struct" else .function,
        .name = trimmed,
        .module = null,
        .file = null,
    };
}

/// Checks if a slice of bytes represents a valid PascalCase string.
pub fn isPascalCase(s: []const u8) bool {
    return s.len > 0 and std.ascii.isUpper(s[0]);
}

/// Checks if a query matches an exact match, returning true or false accordingly.
pub fn shouldSkipLLMSynthesis(query: []const u8, exact_match: bool) bool {
    // TIER 0: empty → list recent
    if (query.len == 0) return true;
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len == 0) return true;

    // TIER 1: exact name hit from DB
    if (exact_match) return true;

    // TIER 1: identifier-pattern detection
    if (detectIdentifierPattern(trimmed) != null) return true;

    return false;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "isPascalCase: true for uppercase first char" {
    try testing.expect(isPascalCase("CSRGraph"));
    try testing.expect(isPascalCase("Library"));
    try testing.expect(isPascalCase("ContextNode"));
    try testing.expect(isPascalCase("A"));
}

test "isPascalCase: false for lowercase or empty" {
    try testing.expect(!isPascalCase("filterStages"));
    try testing.expect(!isPascalCase("cache"));
    try testing.expect(!isPascalCase(""));
    try testing.expect(!isPascalCase("_Foo"));
}

test "detectIdentifierPattern: null for natural language" {
    try testing.expect(detectIdentifierPattern("how does filterStages work?") == null);
    try testing.expect(detectIdentifierPattern("vector search") == null);
    try testing.expect(detectIdentifierPattern("  ") == null);
    try testing.expect(detectIdentifierPattern("") == null);
}

test "detectIdentifierPattern: single snake_case function" {
    const m = detectIdentifierPattern("filterStages").?;
    try testing.expectEqual(IdentifierKind.function, m.kind);
    try testing.expectEqualStrings("filterStages", m.name);
    try testing.expect(m.module == null);
}

test "detectIdentifierPattern: single PascalCase struct" {
    const m = detectIdentifierPattern("ContextNode").?;
    try testing.expectEqual(IdentifierKind.@"struct", m.kind);
    try testing.expectEqualStrings("ContextNode", m.name);
    try testing.expect(m.module == null);
}

test "detectIdentifierPattern: module.struct two-segment" {
    const m = detectIdentifierPattern("cache.L1Cache").?;
    try testing.expectEqual(IdentifierKind.@"struct", m.kind);
    try testing.expectEqualStrings("L1Cache", m.name);
    try testing.expectEqualStrings("cache", m.module.?);
}

test "detectIdentifierPattern: module.function two-segment" {
    const m = detectIdentifierPattern("db.insertNode").?;
    try testing.expectEqual(IdentifierKind.function, m.kind);
    try testing.expectEqualStrings("insertNode", m.name);
    try testing.expectEqualStrings("db", m.module.?);
}

test "detectIdentifierPattern: three-segment method" {
    const m = detectIdentifierPattern("cache.L1Cache.get").?;
    try testing.expectEqual(IdentifierKind.method, m.kind);
    try testing.expectEqualStrings("get", m.name);
}

test "detectIdentifierPattern: file path" {
    const m = detectIdentifierPattern("src/coral/db.zig").?;
    try testing.expectEqual(IdentifierKind.file, m.kind);
    try testing.expectEqualStrings("src/coral/db.zig", m.name);
    try testing.expectEqualStrings("src/coral/db.zig", m.file.?);
}

test "shouldSkipLLMSynthesis: empty query" {
    try testing.expect(shouldSkipLLMSynthesis("", false));
    try testing.expect(shouldSkipLLMSynthesis("  ", false));
}

test "shouldSkipLLMSynthesis: exact match bypasses LLM" {
    try testing.expect(shouldSkipLLMSynthesis("some query", true));
}

test "shouldSkipLLMSynthesis: identifier bypasses LLM" {
    try testing.expect(shouldSkipLLMSynthesis("filterStages", false));
    try testing.expect(shouldSkipLLMSynthesis("ContextNode", false));
    try testing.expect(shouldSkipLLMSynthesis("cache.L1Cache", false));
}

test "shouldSkipLLMSynthesis: natural language does not bypass" {
    try testing.expect(!shouldSkipLLMSynthesis("how does filterStages work?", false));
    try testing.expect(!shouldSkipLLMSynthesis("vector search", false));
}
