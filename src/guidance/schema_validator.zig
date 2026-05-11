//! schema_validator.zig — GuidanceDoc field validation.
//!
//! Validates that a GuidanceDoc produced by the Zig AST parser or the Python
//! provider meets the schema defined in VISION.md. This catches issues early
//! (before the doc is written to disk or synced to the DB) and enforces parity
//! between the Zig and Python providers.
//!
//! §Rules enforced:
//!   - meta.module, meta.source, meta.language must be non-empty
//!   - Every member must have a non-empty name
//!   - language must be a known tag (zig, python, markdown, config, unknown)
//!
//! The validator is deliberately lenient about optional fields (comment, detail,
//! keywords, skills, capabilities) — those may be empty during initial sync.

const std = @import("std");
const types = @import("types.zig");

/// Errors emitted by the validator.
pub const ValidationError = error{
    MissingModule,
    MissingSource,
    MissingLanguage,
    UnknownLanguage,
    MemberMissingName,
};

const KNOWN_LANGUAGES = [_][]const u8{
    "zig",  "python", "markdown",   "config",     "unknown",
    "rust", "go",     "typescript", "javascript", "c",
    "cpp",
};

/// Validates a GuidanceDoc structure using an allocator and returns no error if successful.
pub fn validateGuidanceDoc(allocator: std.mem.Allocator, doc: *const types.GuidanceDoc) ValidationError!void {
    _ = allocator; // reserved for future richer error messages

    if (doc.meta.module.len == 0) return error.MissingModule;
    if (doc.meta.source.len == 0) return error.MissingSource;
    if (doc.meta.language.len == 0) return error.MissingLanguage;

    // Validate language tag.
    var known = false;
    for (KNOWN_LANGUAGES) |lang| {
        if (std.mem.eql(u8, doc.meta.language, lang)) {
            known = true;
            break;
        }
    }
    if (!known) return error.UnknownLanguage;

    // Every member must have a non-empty name.
    for (doc.members) |m| {
        if (m.name.len == 0) return error.MemberMissingName;
    }
}

/// Writes a JSON schema to a file using an allocator and path parameters.
pub fn writeJsonSchemaFile(allocator: std.mem.Allocator, path: []const u8) !void {
    const schema =
        \\{
        \\  "$schema": "http://json-schema.org/draft-07/schema#",
        \\  "title": "GuidanceDoc",
        \\  "description": "Guidance JSON schema — canonical AST metadata format",
        \\  "type": "object",
        \\  "required": ["meta", "members"],
        \\  "properties": {
        \\    "meta": {
        \\      "type": "object",
        \\      "required": ["module", "source", "language"],
        \\      "properties": {
        \\        "module":   { "type": "string", "minLength": 1 },
        \\        "source":   { "type": "string", "minLength": 1 },
        \\        "language": { "type": "string", "minLength": 1 }
        \\      }
        \\    },
        \\    "comment":      { "type": ["string", "null"] },
        \\    "detail":       { "type": ["string", "null"] },
        \\    "keywords":     { "type": "array", "items": { "type": "string" } },
        \\    "skills": {
        \\      "type": "array",
        \\      "items": {
        \\        "type": "object",
        \\        "required": ["ref"],
        \\        "properties": {
        \\          "ref":     { "type": "string" },
        \\          "context": { "type": ["string", "null"] }
        \\        }
        \\      }
        \\    },
        \\    "capabilities": { "type": "array", "items": { "type": "string" } },
        \\    "hashtags":     { "type": "array", "items": { "type": "string" } },
        \\    "used_by":      { "type": "array", "items": { "type": "string" } },
        \\    "members": {
        \\      "type": "array",
        \\      "items": {
        \\        "type": "object",
        \\        "required": ["name", "type"],
        \\        "properties": {
        \\          "name":        { "type": "string", "minLength": 1 },
        \\          "type":        { "type": "string" },
        \\          "signature":   { "type": ["string", "null"] },
        \\          "line":        { "type": ["integer", "null"] },
        \\          "match_hash":  { "type": ["string", "null"] },
        \\          "comment":     { "type": ["string", "null"] },
        \\          "detail":      { "type": ["string", "null"] },
        \\          "keywords":    { "type": "array", "items": { "type": "string" } },
        \\          "skills":      { "type": "array" },
        \\          "capabilities":{ "type": "array", "items": { "type": "string" } },
        \\          "equivalents": { "type": "array", "items": { "type": "string" } }
        \\        }
        \\      }
        \\    }
        \\  }
        \\}
        \\
    ;

    const dir = std.fs.path.dirname(path) orelse ".";
    std.fs.makeDirAbsolute(dir) catch |err| if (err != error.PathAlreadyExists) return err;

    const file = try std.Io.Dir.createFileAbsolute(std.Io.Threaded.global_single_threaded.io(), path, .{});
    defer file.close();
    try file.writeAll(schema);
    _ = allocator; // not needed for static string write
}

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;

test "validateGuidanceDoc: valid minimal doc passes" {
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "foo", .source = "src/foo.zig", .language = "zig" },
    };
    try validateGuidanceDoc(testing.allocator, &doc);
}

test "validateGuidanceDoc: missing module → MissingModule" {
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "", .source = "src/foo.zig", .language = "zig" },
    };
    try testing.expectError(error.MissingModule, validateGuidanceDoc(testing.allocator, &doc));
}

test "validateGuidanceDoc: missing source → MissingSource" {
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "foo", .source = "", .language = "zig" },
    };
    try testing.expectError(error.MissingSource, validateGuidanceDoc(testing.allocator, &doc));
}

test "validateGuidanceDoc: unknown language → UnknownLanguage" {
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "foo", .source = "src/foo.zig", .language = "cobol" },
    };
    try testing.expectError(error.UnknownLanguage, validateGuidanceDoc(testing.allocator, &doc));
}

test "validateGuidanceDoc: member with empty name → MemberMissingName" {
    const bad_member = types.Member{ .type = .fn_decl, .name = "" };
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "foo", .source = "src/foo.zig", .language = "zig" },
        .members = &.{bad_member},
    };
    try testing.expectError(error.MemberMissingName, validateGuidanceDoc(testing.allocator, &doc));
}

test "validateGuidanceDoc: python language is valid" {
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "foo", .source = "bin/foo.py", .language = "python" },
    };
    try validateGuidanceDoc(testing.allocator, &doc);
}
