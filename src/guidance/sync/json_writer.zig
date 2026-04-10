//! sync/json_writer.zig — JSON serialization for guidance documents.
//!
//! Moved from types.zig to keep type definitions separate from serialization logic.
const std = @import("std");
const types = @import("../types.zig");
const common = @import("common");

fn writeEscapedValue(writer: anytype, value: []const u8) !void {
    try common.jsonWriteEscaped(writer, value);
}

fn writeEscapedString(writer: anytype, key: []const u8, value: []const u8) !void {
    try writer.print("  \"{s}\": \"", .{key});
    try writeEscapedValue(writer, value);
    try writer.writeAll("\",\n");
}

pub fn jsonifyMember(allocator: std.mem.Allocator, member: types.Member) !?[]u8 {
    var list: std.ArrayList(u8) = .{};
    errdefer list.deinit(allocator);
    const writer = list.writer(allocator);

    try writer.writeAll("{\n");
    try writer.print("  \"type\": \"{s}\",\n", .{@tagName(member.type)});
    try writeEscapedString(writer, "name", member.name);

    if (member.match_hash) |h| {
        try writer.writeAll("  \"match_hash\": \"");
        try writeEscapedValue(writer, h);
        try writer.writeAll("\",\n");
    }
    if (member.signature) |s| {
        try writeEscapedString(writer, "signature", s);
    }
    if (member.params.len > 0) {
        try writer.writeAll("  \"params\": [\n");
        for (member.params, 0..) |param, i| {
            try writer.writeAll("    { ");
            try writer.writeAll("\"name\": \"");
            try writeEscapedValue(writer, param.name);
            try writer.writeAll("\"");
            if (param.type) |t| {
                try writer.writeAll(", \"type\": \"");
                try writeEscapedValue(writer, t);
                try writer.writeAll("\"");
            }
            if (param.default) |d| {
                try writer.writeAll(", \"default\": \"");
                try writeEscapedValue(writer, d);
                try writer.writeAll("\"");
            }
            try writer.writeAll(" }");
            if (i < member.params.len - 1) try writer.writeAll(",");
            try writer.writeAll("\n");
        }
        try writer.writeAll("  ],\n");
    }
    if (member.returns) |r| {
        try writeEscapedString(writer, "returns", r);
    }

    if (member.tags.len > 0) {
        try writer.writeAll("  \"tags\": [");
        for (member.tags, 0..) |tag, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.print("\"{s}\"", .{tag});
        }
        try writer.writeAll("],\n");
    }
    if (member.patterns.len > 0) {
        try writer.writeAll("  \"patterns\": [\n");
        for (member.patterns, 0..) |pat, i| {
            try writer.writeAll("    { \"name\": \"");
            try writeEscapedValue(writer, pat.name);
            try writer.writeAll("\", \"type\": \"");
            try writeEscapedValue(writer, @tagName(pat.type));
            try writer.writeAll("\"");
            if (pat.ref) |ref| {
                try writer.writeAll(", \"ref\": \"");
                try writeEscapedValue(writer, ref);
                try writer.writeAll("\"");
            }
            try writer.writeAll(" }");
            if (i < member.patterns.len - 1) try writer.writeAll(",");
            try writer.writeAll("\n");
        }
        try writer.writeAll("  ],\n");
    }
    const is_pub_has_more = member.members.len > 0 or member.line != null;
    try writer.print("  \"is_pub\": {}{s}\n", .{ member.is_pub, if (is_pub_has_more) "," else "" });

    if (member.members.len > 0) {
        try writer.writeAll("  \"members\": [\n");
        for (member.members, 0..) |nested, i| {
            if (i > 0) try writer.writeAll(",");
            const nested_json = try jsonifyMember(allocator, nested);
            if (nested_json) |nj| {
                defer allocator.free(nj);
                try writer.writeAll("    ");
                for (nj) |c| {
                    if (c == '\n') {
                        try writer.writeAll("\n    ");
                    } else {
                        try writer.writeByte(c);
                    }
                }
                try writer.writeAll("\n");
            }
        }
        try writer.writeAll("  ]");
        if (member.line != null) {
            try writer.writeAll(",");
        }
        try writer.writeAll("\n");
    }
    if (member.line) |l| {
        try writer.print("  \"line\": {}\n", .{l});
    }
    try writer.writeAll("}");
    return @as(?[]u8, try list.toOwnedSlice(allocator));
}

pub fn jsonifyGuidanceDoc(allocator: std.mem.Allocator, doc: types.GuidanceDoc) ![]u8 {
    var list: std.ArrayList(u8) = .{};
    errdefer list.deinit(allocator);
    const writer = list.writer(allocator);

    try writer.writeAll("{\n");
    try writer.writeAll("  \"meta\": {\n");
    try writer.writeAll("    \"module\": \"");
    try writeEscapedValue(writer, doc.meta.module);
    try writer.writeAll("\",\n");
    try writer.writeAll("    \"source\": \"");
    try writeEscapedValue(writer, doc.meta.source);
    try writer.writeAll("\",\n");
    try writer.writeAll("    \"language\": \"");
    try writeEscapedValue(writer, doc.meta.language);
    try writer.writeAll("\"\n");
    const meta_has_more = doc.comment != null or doc.detail != null or doc.keywords.len > 0 or doc.skills.len > 0 or doc.capabilities.len > 0 or doc.hashtags.len > 0 or doc.used_by.len > 0 or doc.equivalents.len > 0 or doc.members.len > 0;
    if (meta_has_more) {
        try writer.writeAll("  },\n");
    } else {
        try writer.writeAll("  }\n");
    }

    if (doc.comment) |d| {
        try writer.writeAll("  \"comment\": \"");
        try writeEscapedValue(writer, d);
        if (doc.detail != null or doc.keywords.len > 0 or doc.skills.len > 0 or doc.capabilities.len > 0 or doc.hashtags.len > 0 or doc.used_by.len > 0 or doc.equivalents.len > 0 or doc.members.len > 0) {
            try writer.writeAll("\",\n");
        } else {
            try writer.writeAll("\"\n");
        }
    }

    if (doc.detail) |d| {
        try writer.writeAll("  \"detail\": \"");
        try writeEscapedValue(writer, d);
        if (doc.keywords.len > 0 or doc.skills.len > 0 or doc.capabilities.len > 0 or doc.hashtags.len > 0 or doc.used_by.len > 0 or doc.equivalents.len > 0 or doc.members.len > 0) {
            try writer.writeAll("\",\n");
        } else {
            try writer.writeAll("\"\n");
        }
    }

    if (doc.keywords.len > 0) {
        try writer.writeAll("  \"keywords\": [");
        for (doc.keywords, 0..) |kw, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.writeAll("\"");
            try writeEscapedValue(writer, kw);
            try writer.writeAll("\"");
        }
        if (doc.skills.len > 0 or doc.capabilities.len > 0 or doc.hashtags.len > 0 or doc.used_by.len > 0 or doc.equivalents.len > 0 or doc.members.len > 0) {
            try writer.writeAll("],\n");
        } else {
            try writer.writeAll("]\n");
        }
    }

    if (doc.skills.len > 0) {
        try writer.writeAll("  \"skills\": [\n");
        for (doc.skills, 0..) |skill, i| {
            try writer.writeAll("    { \"ref\": \"");
            try writeEscapedValue(writer, skill.ref);
            try writer.writeAll("\"");
            if (skill.context) |ctx| {
                try writer.writeAll(", \"context\": \"");
                try writeEscapedValue(writer, ctx);
                try writer.writeAll("\"");
            }
            try writer.writeAll(" }");
            if (i < doc.skills.len - 1) try writer.writeAll(",");
            try writer.writeAll("\n");
        }
        if (doc.capabilities.len > 0 or doc.hashtags.len > 0 or doc.used_by.len > 0 or doc.equivalents.len > 0 or doc.members.len > 0) {
            try writer.writeAll("  ],\n");
        } else {
            try writer.writeAll("  ]\n");
        }
    }

    if (doc.capabilities.len > 0) {
        try writer.writeAll("  \"capabilities\": [");
        for (doc.capabilities, 0..) |cap, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.writeAll("\"");
            try writeEscapedValue(writer, cap);
            try writer.writeAll("\"");
        }
        if (doc.hashtags.len > 0 or doc.used_by.len > 0 or doc.equivalents.len > 0 or doc.members.len > 0) {
            try writer.writeAll("],\n");
        } else {
            try writer.writeAll("]\n");
        }
    }

    if (doc.hashtags.len > 0) {
        try writer.writeAll("  \"hashtags\": [");
        for (doc.hashtags, 0..) |tag, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.writeAll("\"");
            try writeEscapedValue(writer, tag);
            try writer.writeAll("\"");
        }
        if (doc.used_by.len > 0 or doc.equivalents.len > 0 or doc.members.len > 0) {
            try writer.writeAll("],\n");
        } else {
            try writer.writeAll("]\n");
        }
    }

    if (doc.used_by.len > 0) {
        try writer.writeAll("  \"used_by\": [");
        for (doc.used_by, 0..) |u, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.writeAll("\"");
            try writeEscapedValue(writer, u);
            try writer.writeAll("\"");
        }
        if (doc.equivalents.len > 0 or doc.members.len > 0) {
            try writer.writeAll("],\n");
        } else {
            try writer.writeAll("]\n");
        }
    }

    if (doc.equivalents.len > 0) {
        try writer.writeAll("  \"equivalents\": [");
        for (doc.equivalents, 0..) |e, i| {
            if (i > 0) try writer.writeAll(", ");
            try writer.writeAll("\"");
            try writeEscapedValue(writer, e);
            try writer.writeAll("\"");
        }
        if (doc.members.len > 0) {
            try writer.writeAll("],\n");
        } else {
            try writer.writeAll("]\n");
        }
    }

    if (doc.members.len > 0) {
        try writer.writeAll("  \"members\": [\n");
        for (doc.members, 0..) |member, i| {
            const member_json = try jsonifyMember(allocator, member);
            if (member_json) |mj| {
                defer allocator.free(mj);
                try writer.writeAll("    ");
                for (mj) |c| {
                    if (c == '\n') {
                        try writer.writeAll("\n    ");
                    } else {
                        try writer.writeByte(c);
                    }
                }
            }
            if (i < doc.members.len - 1) {
                try writer.writeAll(",\n");
            } else {
                try writer.writeAll("\n");
            }
        }
        try writer.writeAll("  ]\n");
    }

    try writer.writeAll("}\n");
    return list.toOwnedSlice(allocator);
}
