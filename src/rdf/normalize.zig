/// normalize.zig — RDF Term Normalization
///
/// Converts RDF terms into canonical forms suitable for SQLite storage.
/// Uses Blake3 (64-bit truncated) for deterministic IRI → NodeId hashing.
///
/// Key exports:
///   hashIRI(iri)         — deterministic i64 for an IRI
///   hashBlankNode(scope, id) — scope-qualified i64 for a blank node
///   normalizeLiteral(raw, lang, datatype) → TypedValue
const std = @import("std");
const parser_mod = @import("parser.zig");
const Term = parser_mod.Term;
const Literal = parser_mod.Literal;

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/// Deterministic i64 hash of an IRI string using Blake3 (64-bit truncation).
pub fn hashIRI(iri: []const u8) i64 {
    var out: [32]u8 = undefined;
    std.crypto.hash.Blake3.hash(iri, &out, .{});
    // Take first 8 bytes reinterpreted as i64 (little-endian)
    return @bitCast(std.mem.readInt(u64, out[0..8], .little));
}

/// Scope-qualified i64 hash for a blank node.
/// Two blank nodes with the same local id but different scopes get different hashes.
pub fn hashBlankNode(scope: []const u8, id: []const u8) i64 {
    var h = std.crypto.hash.Blake3.init(.{});
    // Length-prefix each part to avoid collision between "a"+"bc" and "ab"+"c"
    var len_buf: [8]u8 = undefined;
    std.mem.writeInt(u64, &len_buf, scope.len, .little);
    h.update(&len_buf);
    h.update(scope);
    std.mem.writeInt(u64, &len_buf, id.len, .little);
    h.update(&len_buf);
    h.update(id);
    var out: [32]u8 = undefined;
    h.final(&out);
    return @bitCast(std.mem.readInt(u64, out[0..8], .little));
}

// ---------------------------------------------------------------------------
// XSD datatype values
// ---------------------------------------------------------------------------

pub const XsdType = enum {
    string,
    lang_string,
    integer,
    decimal,
    double,
    boolean,
    date_time,
    other,
};

pub const TypedValue = union(XsdType) {
    string: void, // value already held by caller
    lang_string: void,
    integer: i64,
    decimal: f64,
    double: f64,
    boolean: bool,
    date_time: i64, // Unix seconds (approximate)
    other: void,
};

const XSD = "http://www.w3.org/2001/XMLSchema#";

/// Detect the XSD type from a datatype IRI.
pub fn detectXsdType(datatype: ?[]const u8) XsdType {
    const dt = datatype orelse return .string;
    if (std.mem.eql(u8, dt, XSD ++ "string")) return .string;
    if (std.mem.eql(u8, dt, XSD ++ "integer") or
        std.mem.eql(u8, dt, XSD ++ "int") or
        std.mem.eql(u8, dt, XSD ++ "long") or
        std.mem.eql(u8, dt, XSD ++ "short")) return .integer;
    if (std.mem.eql(u8, dt, XSD ++ "decimal")) return .decimal;
    if (std.mem.eql(u8, dt, XSD ++ "float") or
        std.mem.eql(u8, dt, XSD ++ "double")) return .double;
    if (std.mem.eql(u8, dt, XSD ++ "boolean")) return .boolean;
    if (std.mem.eql(u8, dt, XSD ++ "dateTime") or
        std.mem.eql(u8, dt, XSD ++ "date")) return .date_time;
    return .other;
}

/// Parse a literal's value string into a TypedValue.
/// Returns .string / .lang_string / .other when parsing is not applicable.
pub fn normalizeLiteral(value: []const u8, lang: ?[]const u8, datatype: ?[]const u8) TypedValue {
    if (lang != null) return .lang_string;
    const xsd_type = detectXsdType(datatype);
    return switch (xsd_type) {
        .integer => blk: {
            const v = std.fmt.parseInt(i64, std.mem.trim(u8, value, " \t"), 10) catch break :blk TypedValue.other;
            break :blk TypedValue{ .integer = v };
        },
        .decimal, .double => blk: {
            const v = std.fmt.parseFloat(f64, std.mem.trim(u8, value, " \t")) catch break :blk TypedValue.other;
            break :blk TypedValue{ .double = v };
        },
        .boolean => blk: {
            if (std.mem.eql(u8, value, "true") or std.mem.eql(u8, value, "1"))
                break :blk TypedValue{ .boolean = true };
            if (std.mem.eql(u8, value, "false") or std.mem.eql(u8, value, "0"))
                break :blk TypedValue{ .boolean = false };
            break :blk TypedValue.other;
        },
        .date_time => blk: {
            // Very basic stub: return 0. A real implementation would parse ISO 8601.
            // The value parameter is intentionally unused in this stub.
            break :blk TypedValue{ .date_time = if (value.len > 0) 0 else 0 };
        },
        .string => TypedValue.string,
        .lang_string => TypedValue.lang_string,
        .other => TypedValue.other,
    };
}

/// BlankNodeScope — tracks blank node ID → NodeId mapping within one file/scope.
pub const BlankNodeScope = struct {
    allocator: std.mem.Allocator,
    scope_id: []const u8, // e.g. file path or document IRI
    map: std.StringHashMap(i64),

    pub fn init(allocator: std.mem.Allocator, scope_id: []const u8) !BlankNodeScope {
        return BlankNodeScope{
            .allocator = allocator,
            .scope_id = scope_id,
            .map = std.StringHashMap(i64).init(allocator),
        };
    }

    pub fn deinit(self: *BlankNodeScope) void {
        var it = self.map.iterator();
        while (it.next()) |entry| self.allocator.free(entry.key_ptr.*);
        self.map.deinit();
    }

    /// Return stable i64 for blank node id.  Allocates on first encounter.
    pub fn resolve(self: *BlankNodeScope, id: []const u8) !i64 {
        if (self.map.get(id)) |existing| return existing;
        const node_id = hashBlankNode(self.scope_id, id);
        const owned_id = try self.allocator.dupe(u8, id);
        try self.map.put(owned_id, node_id);
        return node_id;
    }
};

// =============================================================================
// Tests — Milestone 1.3
// =============================================================================

const testing = std.testing;

test "IRI hash is deterministic" {
    const iri = "http://example.org/foo";
    const h1 = hashIRI(iri);
    const h2 = hashIRI(iri);
    try testing.expectEqual(h1, h2);
}

test "different IRIs produce different hashes" {
    const h1 = hashIRI("http://example.org/foo");
    const h2 = hashIRI("http://example.org/bar");
    try testing.expect(h1 != h2);
}

test "blank node scoping" {
    var scope = try BlankNodeScope.init(testing.allocator, "file://test.ttl");
    defer scope.deinit();
    const id1 = try scope.resolve("b1");
    const id2 = try scope.resolve("b1");
    try testing.expectEqual(id1, id2); // same id → same hash

    const id3 = try scope.resolve("b2");
    try testing.expect(id1 != id3); // different ids → different hashes
}

test "blank node different scopes produce different IDs" {
    const h1 = hashBlankNode("scope1", "b1");
    const h2 = hashBlankNode("scope2", "b1");
    try testing.expect(h1 != h2);
}

test "normalize integer literal" {
    const tv = normalizeLiteral("42", null, XSD ++ "integer");
    try testing.expectEqual(XsdType.integer, @as(XsdType, tv));
    try testing.expectEqual(@as(i64, 42), tv.integer);
}

test "normalize decimal literal" {
    const tv = normalizeLiteral("3.14", null, XSD ++ "decimal");
    try testing.expectEqual(XsdType.double, @as(XsdType, tv));
}

test "normalize boolean literal true" {
    const tv = normalizeLiteral("true", null, XSD ++ "boolean");
    try testing.expectEqual(XsdType.boolean, @as(XsdType, tv));
    try testing.expect(tv.boolean);
}

test "normalize boolean literal false" {
    const tv = normalizeLiteral("false", null, XSD ++ "boolean");
    try testing.expect(!tv.boolean);
}

test "normalize lang string" {
    const tv = normalizeLiteral("bonjour", "fr", null);
    try testing.expectEqual(XsdType.lang_string, @as(XsdType, tv));
}

test "normalize plain string" {
    const tv = normalizeLiteral("hello", null, null);
    try testing.expectEqual(XsdType.string, @as(XsdType, tv));
}

test "normalize dateTime stub" {
    const tv = normalizeLiteral("2024-01-01T00:00:00Z", null, XSD ++ "dateTime");
    try testing.expectEqual(XsdType.date_time, @as(XsdType, tv));
}
