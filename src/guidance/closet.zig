const std = @import("std");

pub const DETAIL_CHAR_LIMIT: usize = 1500;
pub const DETAIL_EXTRACT_WINDOW: usize = 5000;

pub const DetailLine = struct {
    topic: []const u8,
    entities: []const u8,
    drawer_ref: []const u8,
};

pub const DetailSegment = struct {
    id: []const u8,
    lines: []const DetailLine,
    source_file: []const u8,
    wing: []const u8,
    room: []const u8,
};

pub const DetailLayer = struct {
    allocator: std.mem.Allocator,
    detail_id_base: []const u8,

    pub fn init(allocator: std.mem.Allocator, detail_id_base: []const u8) DetailLayer {
        return .{
            .allocator = allocator,
            .detail_id_base = detail_id_base,
        };
    }

    pub fn deinit(self: *DetailLayer) void {
        _ = self;
    }

    pub fn buildDetailLines(
        self: *DetailLayer,
        source_file: []const u8,
        drawer_ids: []const []const u8,
        content: []const u8,
        wing: []const u8,
        room: []const u8,
    ) ![]DetailLine {
        var lines: std.ArrayList(DetailLine) = .empty;
        errdefer {
            for (lines.items) |item| {
                self.allocator.free(item.topic);
                self.allocator.free(item.entities);
                self.allocator.free(item.drawer_ref);
            }
            lines.deinit(self.allocator);
        }

        var iter = std.mem.splitScalar(u8, content, '\n');
        var line_num: usize = 0;
        while (iter.next()) |line| : (line_num += 1) {
            const trimmed = std.mem.trim(u8, line, " \t\r");
            if (trimmed.len < 10) continue;
            if (isBoilerplate(trimmed)) continue;

            const topic = try self.extractTopic(trimmed);
            const drawer_ref = try self.buildDrawerRef(drawer_ids, line_num);
            const entities = try self.extractEntities(trimmed);

            try lines.append(self.allocator, .{
                .topic = topic,
                .entities = entities,
                .drawer_ref = drawer_ref,
            });
            _ = source_file;
            _ = wing;
            _ = room;
        }

        return lines.toOwnedSlice(self.allocator);
    }

    pub fn upsertDetailLines(
        _: *DetailLayer,
        details_col: []const u8,
        detail_id_base: []const u8,
        lines: []const DetailLine,
        metadata: []const u8,
    ) !void {
        _ = details_col;
        _ = detail_id_base;
        _ = lines;
        _ = metadata;
    }

pub fn purgeFileDetails(_: *DetailLayer, _: []const u8, _: []const u8) void {}

    fn isBoilerplate(line: []const u8) bool {
        if (std.mem.startsWith(u8, line, "//") and !std.mem.startsWith(u8, line, "///")) return true;
        if (std.mem.startsWith(u8, line, "/*")) return true;
        if (std.mem.startsWith(u8, line, "pub const")) return false;
        if (line.len > DETAIL_EXTRACT_WINDOW) return true;
        return false;
    }

    fn extractTopic(self: *DetailLayer, line: []const u8) ![]const u8 {
        var buf: [1024]u8 = undefined;
        const end = @min(line.len, buf.len);
        var j: usize = 0;
        for (line[0..end]) |ch| {
            if (ch == '|') break;
            buf[j] = std.ascii.toLower(ch);
            j += 1;
        }
        if (j == 0) j = @min(line.len, buf.len);
        const topic = std.mem.trim(u8, buf[0..j], " \t");
        return try self.allocator.dupe(u8, topic);
    }

    fn buildDrawerRef(self: *DetailLayer, drawer_ids: []const []const u8, line_num: usize) ![]const u8 {
        if (drawer_ids.len == 0) return try self.allocator.dupe(u8, "");
        const max = @min(drawer_ids.len, 3);
        var buf: [4096]u8 = undefined;
        var fbs = std.io.fixedBufferStream(&buf);
        var writer = fbs.writer();
        for (drawer_ids[0..max], 0..) |id, i| {
            if (i > 0) try writer.writeAll(",");
            try writer.writeAll(id);
        }
        try writer.writeAll(":");
        try writer.print("{d}", .{line_num});
        const result = fbs.getWritten();
        return try self.allocator.dupe(u8, result);
    }

    fn extractEntities(self: *DetailLayer, line: []const u8) ![]const u8 {
        var buf: [2048]u8 = undefined;
        var j: usize = 0;
        var in_word = false;
        for (line) |ch| {
            if (std.ascii.isUpper(ch)) {
                if (in_word and j > 0 and buf[j - 1] != ';') {
                    buf[j] = ';';
                    j += 1;
                }
                if (j < buf.len) {
                    buf[j] = std.ascii.toLower(ch);
                    j += 1;
                }
                in_word = true;
            } else if (std.ascii.isAlphanumeric(ch)) {
                if (j < buf.len) {
                    buf[j] = ch;
                    j += 1;
                }
                in_word = true;
            } else {
                if (in_word and j > 0 and j < buf.len) {
                    buf[j] = ';';
                    j += 1;
                }
                in_word = false;
            }
        }
        if (j > 0 and buf[j - 1] == ';') j -= 1;
        const result = std.mem.trim(u8, buf[0..j], ";");
        return try self.allocator.dupe(u8, result);
    }
};

const testing = std.testing;

test "DetailLayer buildDetailLines" {
    var layer = DetailLayer.init(testing.allocator, "test-base");
    defer layer.deinit();

    const content =
        \\pub fn filterStages() void {
        \\    return pipeline.filterByType();
        \\}
    ;
    const drawer_ids = [_][]const u8{"drawer-001"};
    const lines = try layer.buildDetailLines("src/foo.zig", &drawer_ids, content, "main", "code");
    defer {
        for (lines) |line| {
            testing.allocator.free(line.topic);
            testing.allocator.free(line.entities);
            testing.allocator.free(line.drawer_ref);
        }
        testing.allocator.free(lines);
    }
    try testing.expect(lines.len > 0);
}