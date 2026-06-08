const std = @import("std");

pub const WordTokenizer = struct {
    buf: []const u8,
    pos: usize = 0,

    pub fn next(self: *WordTokenizer) ?[]const u8 {
        while (self.pos < self.buf.len and !isWordChar(self.buf[self.pos])) {
            self.pos += 1;
        }
        if (self.pos >= self.buf.len) return null;
        const start = self.pos;
        while (self.pos < self.buf.len and isWordChar(self.buf[self.pos])) {
            self.pos += 1;
        }
        return self.buf[start..self.pos];
    }

    pub fn reset(self: *WordTokenizer, buf: []const u8) void {
        self.buf = buf;
        self.pos = 0;
    }
};

pub fn isWordChar(c: u8) bool {
    return std.ascii.isAlphanumeric(c) or c == '_';
}

pub fn normalizeChar(c: u8) u8 {
    if (c >= 'A' and c <= 'Z') return c + 32;
    return c;
}

pub fn splitIdentifier(token: []const u8, results: *std.ArrayList([]const u8), allocator: std.mem.Allocator) !void {
    if (token.len < 2) return;

    var seg_start: usize = 0;
    var i: usize = 1;

    while (i < token.len) : (i += 1) {
        const prev = token[i - 1];
        const cur = token[i];

        var boundary = false;

        if (cur == '_') {
            boundary = true;
        } else if (std.ascii.isUpper(cur) and std.ascii.isLower(prev)) {
            boundary = true;
        } else if (std.ascii.isLower(cur) and std.ascii.isUpper(prev)) {
            if (i >= 2 and std.ascii.isUpper(token[i - 2])) {
                boundary = true;
            }
        } else if (std.ascii.isDigit(cur) != std.ascii.isDigit(prev)) {
            boundary = true;
        }

        if (boundary) {
            if (cur == '_') {
                const seg = token[seg_start..i];
                if (seg.len >= 2) try emitSubToken(seg, results, allocator);
                seg_start = i + 1;
            } else {
                const seg = token[seg_start..i];
                if (seg.len >= 2) try emitSubToken(seg, results, allocator);
                seg_start = i;
            }
        }
    }

    if (token.len > seg_start) {
        const seg = token[seg_start..];
        if (seg.len >= 2) try emitSubToken(seg, results, allocator);
    }
}

fn emitSubToken(seg: []const u8, results: *std.ArrayList([]const u8), allocator: std.mem.Allocator) !void {
    var lower_buf: [256]u8 = undefined;
    if (seg.len <= lower_buf.len) {
        for (seg, 0..) |c, j| lower_buf[j] = normalizeChar(c);
        const lower = try allocator.dupe(u8, lower_buf[0..seg.len]);
        try results.append(allocator, lower);
    } else {
        const lower = try allocator.alloc(u8, seg.len);
        for (seg, 0..) |c, j| lower[j] = normalizeChar(c);
        try results.append(allocator, lower);
    }
}

pub fn normalizeInto(dst: []u8, src: []const u8) []u8 {
    const len = @min(src.len, dst.len);
    for (src[0..len], 0..) |c, i| dst[i] = normalizeChar(c);
    return dst[0..len];
}

const testing = std.testing;

test "WordTokenizer basic" {
    var tok: WordTokenizer = .{ .buf = "hello world" };
    var words: std.ArrayList([]const u8) = .empty;
    defer words.deinit(testing.allocator);
    while (tok.next()) |word| {
        try words.append(testing.allocator, word);
    }
    try testing.expectEqual(@as(usize, 2), words.items.len);
    try testing.expectEqualStrings("hello", words.items[0]);
    try testing.expectEqualStrings("world", words.items[1]);
}

test "WordTokenizer identifiers and symbols" {
    var tok: WordTokenizer = .{ .buf = "fn fooBar(baz: u32) void" };
    var words: std.ArrayList([]const u8) = .empty;
    defer words.deinit(testing.allocator);
    while (tok.next()) |word| {
        try words.append(testing.allocator, word);
    }
    try testing.expectEqual(@as(usize, 5), words.items.len);
    try testing.expectEqualStrings("fn", words.items[0]);
    try testing.expectEqualStrings("fooBar", words.items[1]);
    try testing.expectEqualStrings("baz", words.items[2]);
    try testing.expectEqualStrings("u32", words.items[3]);
    try testing.expectEqualStrings("void", words.items[4]);
}

test "normalizeChar" {
    try testing.expectEqual(@as(u8, 'a'), normalizeChar('A'));
    try testing.expectEqual(@as(u8, 'z'), normalizeChar('z'));
    try testing.expectEqual(@as(u8, '0'), normalizeChar('0'));
    try testing.expectEqual(@as(u8, '_'), normalizeChar('_'));
}

test "splitIdentifier snake_case" {
    var results: std.ArrayList([]const u8) = .empty;
    defer {
        for (results.items) |item| testing.allocator.free(item);
        results.deinit(testing.allocator);
    }
    try splitIdentifier("foo_bar_baz", &results, testing.allocator);
    try testing.expectEqual(@as(usize, 3), results.items.len);
    try testing.expectEqualStrings("foo", results.items[0]);
    try testing.expectEqualStrings("bar", results.items[1]);
    try testing.expectEqualStrings("baz", results.items[2]);
}

test "splitIdentifier camelCase" {
    var results: std.ArrayList([]const u8) = .empty;
    defer {
        for (results.items) |item| testing.allocator.free(item);
        results.deinit(testing.allocator);
    }
    try splitIdentifier("filterStages", &results, testing.allocator);
    var found_filter = false;
    var found_stages = false;
    for (results.items) |item| {
        if (std.mem.eql(u8, item, "filter")) found_filter = true;
        if (std.mem.eql(u8, item, "stages")) found_stages = true;
    }
    try testing.expect(found_filter);
    try testing.expect(found_stages);
}

test "splitIdentifier PascalCase" {
    var results: std.ArrayList([]const u8) = .empty;
    defer {
        for (results.items) |item| testing.allocator.free(item);
        results.deinit(testing.allocator);
    }
    try splitIdentifier("WordTokenizer", &results, testing.allocator);
    var found_word = false;
    var found_tokenizer = false;
    for (results.items) |item| {
        if (std.mem.eql(u8, item, "word")) found_word = true;
        if (std.mem.eql(u8, item, "tokenizer")) found_tokenizer = true;
    }
    try testing.expect(found_word);
    try testing.expect(found_tokenizer);
}

test "splitIdentifier too short" {
    var results: std.ArrayList([]const u8) = .empty;
    defer results.deinit(testing.allocator);
    try splitIdentifier("a", &results, testing.allocator);
    try testing.expectEqual(@as(usize, 0), results.items.len);
}

test "normalizeInto" {
    var buf: [256]u8 = undefined;
    const result = normalizeInto(&buf, "HelloWorld");
    try testing.expectEqualStrings("helloworld", result);
}
