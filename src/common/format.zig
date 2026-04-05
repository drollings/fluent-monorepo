const std = @import("std");

/// Defines a column structure for data storage, manages ownership, and ensures consistent access patterns.
pub const Column = struct {
    header: []const u8,
    key: []const u8,
    width: usize = 0,
    align_left: bool = true,
};

/// Defines a table structure for structured data; owned by the module; maintains invariant data integrity.
pub const Table = struct {
    columns: []const Column,
    rows: []const std.json.Value,
    title: []const u8,
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator, columns: []const Column, title: []const u8) Table {
        return .{
            .columns = columns,
            .rows = &[_]std.json.Value{},
            .title = title,
            .allocator = allocator,
        };
    }

    pub fn withRows(self: *Table, rows: []const std.json.Value) void {
        self.rows = rows;
    }

    pub fn render(self: *const Table, writer: anytype) !void {
        if (self.title.len > 0) {
            try writer.print("{s}\n", .{self.title});
            var i: usize = 0;
            while (i < self.title.len) : (i += 1) {
                try writer.writeAll("=");
            }
            try writer.writeAll("\n\n");
        }

        for (self.columns) |col| {
            const w = if (col.width > 0) col.width else col.header.len + 2;
            if (col.align_left) {
                try writer.print(" {s:<{}} ", .{ col.header, w });
            } else {
                try writer.print(" {s:>{}} ", .{ col.header, w });
            }
        }
        try writer.writeAll("\n");

        for (self.columns) |col| {
            const w = if (col.width > 0) col.width else col.header.len + 2;
            try writer.writeAll(" ");
            var i: usize = 0;
            while (i < w) : (i += 1) {
                try writer.writeAll("-");
            }
            try writer.writeAll(" ");
        }
        try writer.writeAll("\n");

        for (self.rows) |row| {
            if (row != .object) continue;
            for (self.columns) |col| {
                const val = row.object.get(col.key) orelse std.json.Value{ .null = {} };
                const str = valueToString(self.allocator, val) catch "";
                defer if (str.len > 0) self.allocator.free(@constCast(str.ptr[0..str.len]));

                const w = if (col.width > 0) col.width else col.header.len + 2;
                if (col.align_left) {
                    try writer.print(" {s:<{}} ", .{ str, w });
                } else {
                    try writer.print(" {s:>{}} ", .{ str, w });
                }
            }
            try writer.writeAll("\n");
        }
    }
};

/// Converts a JSON value into a Zig-safe string slice.
fn valueToString(allocator: std.mem.Allocator, val: std.json.Value) ![]const u8 {
    return switch (val) {
        .null => allocator.dupe(u8, "null"),
        .bool => |b| if (b) allocator.dupe(u8, "true") else allocator.dupe(u8, "false"),
        .integer => |i| std.fmt.allocPrint(allocator, "{d}", .{i}),
        .float => |f| std.fmt.allocPrint(allocator, "{d}", .{f}),
        .string => |s| allocator.dupe(u8, s),
        .array => blk: {
            var buf: std.ArrayListUnmanaged(u8) = .{};
            errdefer buf.deinit(allocator);
            try buf.append(allocator, '[');
            for (val.array.items, 0..) |item, idx| {
                if (idx > 0) try buf.appendSlice(allocator, ", ");
                const s = try valueToString(allocator, item);
                defer allocator.free(s);
                try buf.appendSlice(allocator, s);
            }
            try buf.append(allocator, ']');
            break :blk buf.toOwnedSlice(allocator);
        },
        .object => blk: {
            var buf: std.ArrayListUnmanaged(u8) = .{};
            errdefer buf.deinit(allocator);
            try buf.append(allocator, '{');
            var iter = val.object.iterator();
            var first = true;
            while (iter.next()) |entry| {
                if (!first) try buf.appendSlice(allocator, ", ");
                first = false;
                try buf.append(allocator, '"');
                try buf.appendSlice(allocator, entry.key_ptr.*);
                try buf.appendSlice(allocator, "\": ");
                const s = try valueToString(allocator, entry.value_ptr.*);
                defer allocator.free(s);
                try buf.appendSlice(allocator, s);
            }
            try buf.append(allocator, '}');
            break :blk buf.toOwnedSlice(allocator);
        },
    };
}

/// Converts a JSON value into a formatted Zig array with indentation.
pub fn formatJson(allocator: std.mem.Allocator, value: anytype, indent: usize) ![]const u8 {
    var buf: std.ArrayListUnmanaged(u8) = .{};
    errdefer buf.deinit(allocator);
    try stringify(value, indent, 0, buf.writer(allocator));
    return buf.toOwnedSlice(allocator);
}

/// Converts a value into a formatted string with specified indentation and level.
fn stringify(value: anytype, indent: usize, level: usize, writer: anytype) !void {
    const T = @TypeOf(value);
    if (T == []const u8) {
        try writer.print("\"{s}\"", .{value});
    } else if (T == bool) {
        try writer.writeAll(if (value) "true" else "false");
    } else if (@typeInfo(T) == .int or @typeInfo(T) == .float) {
        try writer.print("{d}", .{value});
    } else if (@typeInfo(T) == .null) {
        try writer.writeAll("null");
    } else if (@typeInfo(T) == .array) {
        try writer.writeAll("[");
        for (value, 0..) |item, i| {
            if (i > 0) try writer.writeAll(", ");
            try stringify(item, indent, level, writer);
        }
        try writer.writeAll("]");
    } else if (@typeInfo(T) == .@"struct") {
        try writer.writeAll("{\n");
        const fields = std.meta.fields(T);
        inline for (fields, 0..) |field, i| {
            if (i > 0) try writer.writeAll(",\n");
            var j: usize = 0;
            while (j < (level + 1) * indent) : (j += 1) try writer.writeAll(" ");
            try writer.print("\"{s}\": ", .{field.name});
            try stringify(@field(value, field.name), indent, level + 1, writer);
        }
        try writer.writeAll("\n");
        var j: usize = 0;
        while (j < level * indent) : (j += 1) try writer.writeAll(" ");
        try writer.writeAll("}");
    } else {
        try writer.print("{}", .{value});
    }
}

/// Converts a CSV-formatted JSON slice into a CSV-formatted string using an allocator and field names.
pub fn formatCsv(allocator: std.mem.Allocator, rows: []const std.json.Value, fieldnames: ?[]const []const u8) ![]const u8 {
    if (rows.len == 0) return allocator.dupe(u8, "");

    var buf: std.ArrayListUnmanaged(u8) = .{};
    errdefer buf.deinit(allocator);
    const writer = buf.writer(allocator);

    const fields = if (fieldnames) |f| f else blk: {
        if (rows[0] == .object) {
            var count: usize = 0;
            var iter = rows[0].object.iterator();
            while (iter.next() != null) : (count += 1) {}
            var result = try allocator.alloc([]const u8, count);
            iter = rows[0].object.iterator();
            var i: usize = 0;
            while (iter.next()) |entry| : (i += 1) {
                result[i] = entry.key_ptr.*;
            }
            break :blk result;
        }
        break :blk &[_][]const u8{};
    };

    for (fields, 0..) |field, i| {
        if (i > 0) try writer.writeAll(",");
        try writeCsvField(writer, field);
    }
    try writer.writeAll("\n");

    for (rows) |row| {
        if (row != .object) continue;
        for (fields, 0..) |field, i| {
            if (i > 0) try writer.writeAll(",");
            if (row.object.get(field)) |val| {
                const str = valueToString(allocator, val) catch "";
                defer if (str.len > 0) allocator.free(@constCast(str.ptr[0..str.len]));
                try writeCsvField(writer, str);
            }
        }
        try writer.writeAll("\n");
    }

    return buf.toOwnedSlice(allocator);
}

/// Writes a CSV field as a byte slice to the writer, handling null-termination.
fn writeCsvField(writer: anytype, field: []const u8) !void {
    const needs_quote = std.mem.indexOfAny(u8, field, "\",\n\r") != null;
    if (needs_quote) {
        try writer.writeAll("\"");
        for (field) |c| {
            if (c == '"') try writer.writeAll("\"\"");
            try writer.writeByte(c);
        }
        try writer.writeAll("\"");
    } else {
        try writer.writeAll(field);
    }
}

/// Converts a given byte slice into a formatted size array in Zig.
pub fn formatSize(bytes: usize, buf: []u8) []const u8 {
    const units = [_][]const u8{ "B", "KB", "MB", "GB", "TB" };
    var value: f64 = @floatFromInt(bytes);
    var unit_idx: usize = 0;

    while (value >= 1024.0 and unit_idx < units.len - 1) {
        value /= 1024.0;
        unit_idx += 1;
    }

    if (unit_idx == 0) {
        return std.fmt.bufPrint(buf, "{d} {s}", .{ @as(usize, @intFromFloat(value)), units[unit_idx] }) catch "N/A";
    } else {
        return std.fmt.bufPrint(buf, "{d:.1} {s}", .{ value, units[unit_idx] }) catch "N/A";
    }
}

/// Converts a null-terminated string slice into its corresponding usize value.
pub fn parseSize(size_str: []const u8) ?usize {
    const trimmed = std.mem.trim(u8, size_str, " \t\r\n");
    if (trimmed.len == 0) return null;

    const SizeMultiplier = struct {
        suffix: []const u8,
        mult: usize,
    };

    const multipliers = [_]SizeMultiplier{
        .{ .suffix = "TB", .mult = 1024 * 1024 * 1024 * 1024 },
        .{ .suffix = "GB", .mult = 1024 * 1024 * 1024 },
        .{ .suffix = "MB", .mult = 1024 * 1024 },
        .{ .suffix = "KB", .mult = 1024 },
        .{ .suffix = "T", .mult = 1024 * 1024 * 1024 * 1024 },
        .{ .suffix = "G", .mult = 1024 * 1024 * 1024 },
        .{ .suffix = "M", .mult = 1024 * 1024 },
        .{ .suffix = "K", .mult = 1024 },
        .{ .suffix = "B", .mult = 1 },
    };

    for (multipliers) |m| {
        if (std.ascii.endsWithIgnoreCase(trimmed, m.suffix)) {
            const num_str = trimmed[0 .. trimmed.len - m.suffix.len];
            const num = std.fmt.parseFloat(f64, num_str) catch return null;
            return @intFromFloat(num * @as(f64, @floatFromInt(m.mult)));
        }
    }

    return std.fmt.parseInt(usize, trimmed, 10) catch null;
}

const testing = std.testing;

test "formatSize: bytes" {
    var buf: [32]u8 = undefined;
    const result = formatSize(512, &buf);
    try testing.expect(std.mem.startsWith(u8, result, "512"));
}

test "formatSize: kilobytes" {
    var buf: [32]u8 = undefined;
    const result = formatSize(2048, &buf);
    try testing.expect(std.mem.indexOf(u8, result, "KB") != null);
}

test "parseSize: plain number" {
    try testing.expectEqual(@as(usize, 1024), parseSize("1024").?);
}

test "parseSize: with KB suffix" {
    try testing.expectEqual(@as(usize, 2048), parseSize("2KB").?);
}

test "parseSize: with MB suffix" {
    try testing.expectEqual(@as(usize, 2 * 1024 * 1024), parseSize("2MB").?);
}

test "formatJson: simple struct" {
    const TestStruct = struct {
        name: []const u8,
        value: i32,
    };
    const data = TestStruct{ .name = "test", .value = 42 };
    const result = try formatJson(testing.allocator, data, 2);
    defer testing.allocator.free(result);
    try testing.expect(std.mem.indexOf(u8, result, "\"name\"") != null);
    try testing.expect(std.mem.indexOf(u8, result, "\"test\"") != null);
    try testing.expect(std.mem.indexOf(u8, result, "42") != null);
}
