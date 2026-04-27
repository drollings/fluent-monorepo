//! core/metadata.zig — Unified GuidanceDoc JSON metadata loading.
//!
//! Consolidates:
//!   - query_engine.zig:loadPublicMemberNames()
//!   - query_engine.zig:loadUsedByFromJson()
//!   - query_engine.zig:loadSkillsFromJson()
//!   - staged.zig:buildMetadataStage()
//!   - staged.zig:loadSkillNamesFromJson()
//!
//! All returned strings are allocator-owned; callers must free.

const std = @import("std");
const common = @import("common");
const types_mod = @import("../types.zig");

/// Load skill names referenced in a guidance JSON file.
/// Returns a newline-delimited string of skill names, or null.
/// Result is allocator-owned; caller frees.
pub fn loadSkillsFromJson(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    var parsed = common.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();

    const skills_val = parsed.value.object.get("skills") orelse return null;
    if (skills_val != .array) return null;

    var out: std.ArrayList(u8) = .empty;
    for (skills_val.array.items) |item| {
        const ref: []const u8 = switch (item) {
            .string => |s| s,
            .object => blk: {
                const rv = item.object.get("ref") orelse break :blk "";
                if (rv != .string) break :blk "";
                break :blk rv.string;
            },
            else => "",
        };
        if (ref.len == 0) continue;
        const skill_name = common.skillNameFromRef(ref);
        if (skill_name.len == 0) continue;
        out.appendSlice(allocator, skill_name) catch continue;
        out.append(allocator, '\n') catch continue;
    }
    if (out.items.len == 0) {
        out.deinit(allocator);
        return null;
    }
    return out.toOwnedSlice(allocator) catch null;
}

/// Load skill name strings from a guidance JSON file (as a slice of slices).
/// Result slice and all inner strings are allocator-owned; caller frees.
pub fn loadSkillNamesFromJson(allocator: std.mem.Allocator, json_path: []const u8) ![][]const u8 {
    var parsed = common.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return &.{};
    defer parsed.deinit();

    const sv = parsed.value.object.get("skills") orelse return &.{};
    if (sv != .array) return &.{};

    var out: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (out.items) |s| allocator.free(s);
        out.deinit(allocator);
    }

    for (sv.array.items) |item| {
        const ref: []const u8 = switch (item) {
            .string => |s| s,
            .object => blk: {
                const rv = item.object.get("ref") orelse break :blk "";
                if (rv != .string) break :blk "";
                break :blk rv.string;
            },
            else => "",
        };
        if (ref.len == 0) continue;
        const skill_name = common.skillNameFromRef(ref);
        if (skill_name.len == 0) continue;
        try out.append(allocator, try allocator.dupe(u8, skill_name));
    }

    return out.toOwnedSlice(allocator);
}

/// Load used_by paths from a guidance JSON file.
/// Returns null if file is missing or has no used_by entries.
/// Result slice and all inner strings are allocator-owned; caller frees.
pub fn loadUsedByFromJson(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    var parsed = common.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();

    const ub_val = parsed.value.object.get("used_by") orelse return null;
    if (ub_val != .array) return null;

    var out: std.ArrayList([]const u8) = .empty;
    for (ub_val.array.items) |item| {
        if (item != .string) continue;
        out.append(allocator, allocator.dupe(u8, item.string) catch continue) catch continue;
    }
    if (out.items.len == 0) {
        out.deinit(allocator);
        return null;
    }
    return out.toOwnedSlice(allocator) catch null;
}

/// Load public member names from a guidance JSON file.
/// Returns null if file is missing or has no public non-test members.
/// Result slice and all inner strings are allocator-owned; caller frees.
pub fn loadPublicMemberNames(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    var parsed = common.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();

    const members_val = parsed.value.object.get("members") orelse return null;
    if (members_val != .array) return null;

    var out: std.ArrayList([]const u8) = .empty;
    for (members_val.array.items) |item| {
        if (item != .object) continue;
        const is_pub: bool = blk: {
            const pv = item.object.get("is_pub") orelse break :blk false;
            if (pv != .bool) break :blk false;
            break :blk pv.bool;
        };
        if (!is_pub) continue;
        const type_v = item.object.get("type") orelse continue;
        if (type_v != .string) continue;
        if (std.mem.eql(u8, type_v.string, "test_decl")) continue;
        const name_v = item.object.get("name") orelse continue;
        if (name_v != .string) continue;
        if (name_v.string.len == 0) continue;
        out.append(allocator, allocator.dupe(u8, name_v.string) catch continue) catch continue;
    }
    if (out.items.len == 0) {
        out.deinit(allocator);
        return null;
    }
    return out.toOwnedSlice(allocator) catch null;
}

/// Build a metadata Stage from a guidance JSON file.
/// Returns null if the file has no metadata worth emitting.
/// Result is allocator-owned; caller frees via types.freeStages.
pub fn buildMetadataStage(
    allocator: std.mem.Allocator,
    json_path: []const u8,
    source: []const u8,
) !?types_mod.Stage {
    var parsed = common.parseJsonFile(allocator, json_path, 8 * 1024 * 1024) orelse return null;
    defer parsed.deinit();
    const root = parsed.value.object;

    var meta_buf_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer meta_buf_aw.deinit();
    const mw = &meta_buf_aw.writer;

    // keywords: public member names.
    if (root.get("members")) |mv| {
        if (mv == .array) {
            var kw_count: usize = 0;
            var kw_buf: std.ArrayList(u8) = .empty;
            defer kw_buf.deinit(allocator);
            for (mv.array.items) |item| {
                if (item != .object) continue;
                const is_pub: bool = blk: {
                    const pv = item.object.get("is_pub") orelse break :blk false;
                    if (pv != .bool) break :blk false;
                    break :blk pv.bool;
                };
                if (!is_pub) continue;
                const tv = item.object.get("type") orelse continue;
                if (tv != .string) continue;
                if (std.mem.eql(u8, tv.string, "test_decl")) continue;
                const nv = item.object.get("name") orelse continue;
                if (nv != .string) continue;
                if (kw_count > 0) try kw_buf.appendSlice(allocator, ", ");
                try kw_buf.appendSlice(allocator, nv.string);
                kw_count += 1;
                if (kw_count >= 12) break;
            }
            if (kw_buf.items.len > 0) {
                try mw.print("keywords: {s}\n", .{kw_buf.items});
            }
        }
    }

    // used_by: reverse dependency paths (exclude test files).
    if (root.get("used_by")) |ubv| {
        if (ubv == .array and ubv.array.items.len > 0) {
            var count: usize = 0;
            for (ubv.array.items) |item| {
                if (item != .string) continue;
                if (common.isTestPath(item.string)) continue;
                if (count == 0) {
                    try mw.writeAll("used_by: ");
                } else {
                    try mw.writeAll(", ");
                }
                try mw.writeAll(item.string);
                count += 1;
                if (count >= 5) break;
            }
            if (count > 0) try mw.writeByte('\n');
        }
    }

    // skills: skill refs.
    if (root.get("skills")) |sv| {
        if (sv == .array and sv.array.items.len > 0) {
            try mw.writeAll("skills: ");
            for (sv.array.items[0..@min(4, sv.array.items.len)], 0..) |item, i| {
                const ref: []const u8 = switch (item) {
                    .string => |s| s,
                    .object => blk: {
                        const rv = item.object.get("ref") orelse break :blk "";
                        if (rv != .string) break :blk "";
                        break :blk rv.string;
                    },
                    else => "",
                };
                if (ref.len == 0) continue;
                const skill_name = common.skillNameFromRef(ref);
                if (skill_name.len == 0) continue;
                if (i > 0) try mw.writeAll(", ");
                try mw.writeAll(skill_name);
            }
            try mw.writeByte('\n');
        }
    }

    // capabilities: capability refs.
    if (root.get("capabilities")) |cv| {
        if (cv == .array and cv.array.items.len > 0) {
            try mw.writeAll("capabilities: ");
            for (cv.array.items[0..@min(4, cv.array.items.len)], 0..) |item, i| {
                const cap_name: []const u8 = switch (item) {
                    .string => |s| s,
                    else => "",
                };
                if (cap_name.len == 0) continue;
                if (i > 0) try mw.writeAll(", ");
                try mw.writeAll(cap_name);
            }
            try mw.writeByte('\n');
        }
    }

    if (meta_buf_aw.written().len == 0) return null;

    return types_mod.Stage{
        .kind = .metadata,
        .content = try meta_buf_aw.toOwnedSlice(),
        .source = try allocator.dupe(u8, source),
    };
}
