//! sync/json_writer.zig — JSON serialization for guidance documents.
//!
//! Moved from types.zig to keep type definitions separate from serialization logic.
const std = @import("std");
const types = @import("../types.zig");
const common = @import("common");

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

fn writeEscaped(writer: anytype, value: []const u8) !void {
    try common.jsonWriteEscaped(writer, value);
}

/// Write `"key": "value"` — no leading whitespace, no trailing comma or newline.
fn writeStrField(writer: anytype, key: []const u8, value: []const u8) !void {
    try writer.print("\"{s}\": \"", .{key});
    try writeEscaped(writer, value);
    try writer.writeByte('"');
}

/// Emit `,\n` when `need.*` is already true, then set `need.*`.
/// Call before every field; the first call is a no-op that arms subsequent ones.
fn writeSep(writer: anytype, need: *bool) !void {
    if (need.*) try writer.writeAll(",\n");
    need.* = true;
}

/// Write `"key": ["a", "b"]` — no leading whitespace, no trailing comma or newline.
fn writeStringArray(writer: anytype, key: []const u8, items: []const []const u8) !void {
    try writer.print("\"{s}\": [", .{key});
    for (items, 0..) |item, i| {
        if (i > 0) try writer.writeAll(", ");
        try writer.writeByte('"');
        try writeEscaped(writer, item);
        try writer.writeByte('"');
    }
    try writer.writeByte(']');
}

// ---------------------------------------------------------------------------
// Member serialization
// ---------------------------------------------------------------------------

/// Write one Member as a JSON object directly to `writer`.
/// `indent` is prepended to the outer braces; field lines get `indent ++ "  "`.
/// No trailing newline or comma is written — the caller decides.
fn writeMember(writer: anytype, member: types.Member, indent: []const u8) !void {
    var fi_buf: [64]u8 = undefined;
    const fi = std.fmt.bufPrint(&fi_buf, "{s}  ", .{indent}) catch return error.IndentTooDeep;

    try writer.print("{s}{{\n", .{indent});

    var need: bool = false;

    try writeSep(writer, &need);
    try writer.print("{s}\"type\": \"{s}\"", .{ fi, @tagName(member.type) });

    try writeSep(writer, &need);
    try writer.print("{s}", .{fi});
    try writeStrField(writer, "name", member.name);

    if (member.match_hash) |h| {
        try writeSep(writer, &need);
        try writer.print("{s}", .{fi});
        try writeStrField(writer, "match_hash", h);
    }
    if (member.signature) |s| {
        try writeSep(writer, &need);
        try writer.print("{s}", .{fi});
        try writeStrField(writer, "signature", s);
    }
    if (member.params.len > 0) {
        try writeSep(writer, &need);
        try writer.print("{s}\"params\": [\n", .{fi});
        for (member.params, 0..) |param, i| {
            try writer.print("{s}  {{ \"name\": \"", .{fi});
            try writeEscaped(writer, param.name);
            try writer.writeByte('"');
            if (param.type) |t| {
                try writer.writeAll(", \"type\": \"");
                try writeEscaped(writer, t);
                try writer.writeByte('"');
            }
            if (param.default) |d| {
                try writer.writeAll(", \"default\": \"");
                try writeEscaped(writer, d);
                try writer.writeByte('"');
            }
            try writer.writeAll(" }");
            if (i < member.params.len - 1) try writer.writeByte(',');
            try writer.writeByte('\n');
        }
        try writer.print("{s}]", .{fi});
    }
    if (member.returns) |r| {
        try writeSep(writer, &need);
        try writer.print("{s}", .{fi});
        try writeStrField(writer, "returns", r);
    }
    if (member.tags.len > 0) {
        try writeSep(writer, &need);
        try writer.print("{s}", .{fi});
        try writeStringArray(writer, "tags", member.tags);
    }
    if (member.patterns.len > 0) {
        try writeSep(writer, &need);
        try writer.print("{s}\"patterns\": [\n", .{fi});
        for (member.patterns, 0..) |pat, i| {
            try writer.print("{s}  {{ \"name\": \"", .{fi});
            try writeEscaped(writer, pat.name);
            try writer.print("\", \"type\": \"{s}\"", .{@tagName(pat.type)});
            if (pat.ref) |ref| {
                try writer.writeAll(", \"ref\": \"");
                try writeEscaped(writer, ref);
                try writer.writeByte('"');
            }
            try writer.writeAll(" }");
            if (i < member.patterns.len - 1) try writer.writeByte(',');
            try writer.writeByte('\n');
        }
        try writer.print("{s}]", .{fi});
    }

    // is_pub is always present.
    try writeSep(writer, &need);
    try writer.print("{s}\"is_pub\": {any}", .{ fi, member.is_pub });

    if (member.members.len > 0) {
        var ni_buf: [64]u8 = undefined;
        const ni = std.fmt.bufPrint(&ni_buf, "{s}    ", .{indent}) catch return error.IndentTooDeep;

        try writeSep(writer, &need);
        try writer.print("{s}\"members\": [\n", .{fi});
        for (member.members, 0..) |nested, i| {
            try writeMember(writer, nested, ni);
            if (i < member.members.len - 1) try writer.writeByte(',');
            try writer.writeByte('\n');
        }
        try writer.print("{s}]", .{fi});
    }

    if (member.line) |l| {
        try writeSep(writer, &need);
        try writer.print("{s}\"line\": {d}", .{ fi, l });
    }

    try writer.print("\n{s}}}", .{indent});
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Serialize a Member to an owned byte slice.  The caller must free the result.
/// Returns null only on allocation failure; the ?[]u8 type is preserved for
/// backward compatibility with the re-export in types.zig.
pub fn jsonifyMember(allocator: std.mem.Allocator, member: types.Member) !?[]u8 {
    var list_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer list_aw.deinit();
    const writer = &list_aw.writer;
    try writeMember(writer, member, "");
    return try list_aw.toOwnedSlice();
}

pub fn jsonifyGuidanceDoc(allocator: std.mem.Allocator, doc: types.GuidanceDoc) ![]u8 {
    var list_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer list_aw.deinit();
    const writer = &list_aw.writer;

    // meta block (always present).
    try writer.writeAll("{\n  \"meta\": {\n    \"module\": \"");
    try writeEscaped(writer, doc.meta.module);
    try writer.writeAll("\",\n    \"source\": \"");
    try writeEscaped(writer, doc.meta.source);
    try writer.writeAll("\",\n    \"language\": \"");
    try writeEscaped(writer, doc.meta.language);
    try writer.writeAll("\"\n  }");

    // Early exit when meta is the only field.
    const has_fields = doc.comment != null or doc.detail != null or
        doc.keywords.len > 0 or doc.skills.len > 0 or
        doc.capabilities.len > 0 or doc.hashtags.len > 0 or
        doc.used_by.len > 0 or doc.equivalents.len > 0 or
        doc.members.len > 0;
    if (!has_fields) {
        try writer.writeAll("\n}\n");
        return list_aw.toOwnedSlice();
    }

    // Close meta with comma; remaining fields use writeSep to separate each other.
    try writer.writeAll(",\n");
    var need: bool = false;

    if (doc.comment) |d| {
        try writeSep(writer, &need);
        try writer.writeAll("  ");
        try writeStrField(writer, "comment", d);
    }
    if (doc.detail) |d| {
        try writeSep(writer, &need);
        try writer.writeAll("  ");
        try writeStrField(writer, "detail", d);
    }
    if (doc.keywords.len > 0) {
        try writeSep(writer, &need);
        try writer.writeAll("  ");
        try writeStringArray(writer, "keywords", doc.keywords);
    }
    if (doc.skills.len > 0) {
        try writeSep(writer, &need);
        try writer.writeAll("  \"skills\": [\n");
        for (doc.skills, 0..) |skill, i| {
            try writer.writeAll("    { \"ref\": \"");
            try writeEscaped(writer, skill.ref);
            try writer.writeByte('"');
            if (skill.context) |ctx| {
                try writer.writeAll(", \"context\": \"");
                try writeEscaped(writer, ctx);
                try writer.writeByte('"');
            }
            try writer.writeAll(" }");
            if (i < doc.skills.len - 1) try writer.writeByte(',');
            try writer.writeByte('\n');
        }
        try writer.writeAll("  ]");
    }
    if (doc.capabilities.len > 0) {
        try writeSep(writer, &need);
        try writer.writeAll("  ");
        try writeStringArray(writer, "capabilities", doc.capabilities);
    }
    if (doc.hashtags.len > 0) {
        try writeSep(writer, &need);
        try writer.writeAll("  ");
        try writeStringArray(writer, "hashtags", doc.hashtags);
    }
    if (doc.used_by.len > 0) {
        try writeSep(writer, &need);
        try writer.writeAll("  ");
        try writeStringArray(writer, "used_by", doc.used_by);
    }
    if (doc.equivalents.len > 0) {
        try writeSep(writer, &need);
        try writer.writeAll("  ");
        try writeStringArray(writer, "equivalents", doc.equivalents);
    }
    if (doc.members.len > 0) {
        try writeSep(writer, &need);
        try writer.writeAll("  \"members\": [\n");
        for (doc.members, 0..) |member, i| {
            try writeMember(writer, member, "    ");
            if (i < doc.members.len - 1) try writer.writeByte(',');
            try writer.writeByte('\n');
        }
        try writer.writeAll("  ]");
    }

    try writer.writeAll("\n}\n");
    return list_aw.toOwnedSlice();
}
