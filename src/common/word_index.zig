const std = @import("std");
const tokenizer = @import("tokenizer.zig");
const doc_registry = @import("doc_registry.zig");
const DocRegistry = doc_registry.DocRegistry;
const index_header = @import("index_header.zig");

pub const WordHit = struct {
    doc_id: u32,
    line_num: u32,
};

pub const WordIndex = struct {
    index: std.StringHashMap(std.ArrayList(WordHit)),
    file_words: std.StringHashMap([]const []const u8),
    allocator: std.mem.Allocator,
    skip_file_words: bool = false,
    registry: DocRegistry,

    pub fn init(allocator: std.mem.Allocator) WordIndex {
        return .{
            .index = std.StringHashMap(std.ArrayList(WordHit)).init(allocator),
            .file_words = std.StringHashMap([]const []const u8).init(allocator),
            .allocator = allocator,
            .registry = DocRegistry.init(allocator, false),
        };
    }

    pub fn deinit(self: *WordIndex) void {
        var iter = self.index.iterator();
        while (iter.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            entry.value_ptr.deinit(self.allocator);
        }
        self.index.deinit();

        var fw_iter = self.file_words.iterator();
        while (fw_iter.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            self.allocator.free(entry.value_ptr.*);
        }
        self.file_words.deinit();

        self.registry.deinit();
    }

    pub fn hitPath(self: *const WordIndex, hit: WordHit) []const u8 {
        return self.registry.pathForId(hit.doc_id);
    }

    fn indexOneToken(
        self: *WordIndex,
        token: []const u8,
        doc_id: u32,
        line_num: u32,
        words_set: *std.StringHashMap(void),
    ) !void {
        const gop = try self.index.getOrPut(token);
        if (!gop.found_existing) {
            const duped = try self.allocator.dupe(u8, token);
            gop.key_ptr.* = duped;
            gop.value_ptr.* = .empty;
        }
        if (gop.value_ptr.items.len > 0) {
            const last = gop.value_ptr.items[gop.value_ptr.items.len - 1];
            if (last.doc_id == doc_id and last.line_num == line_num) {
                const wgop = try words_set.getOrPut(gop.key_ptr.*);
                if (!wgop.found_existing) wgop.key_ptr.* = gop.key_ptr.*;
                return;
            }
        }
        try gop.value_ptr.append(self.allocator, .{
            .doc_id = doc_id,
            .line_num = line_num,
        });
        const wgop = try words_set.getOrPut(gop.key_ptr.*);
        if (!wgop.found_existing) wgop.key_ptr.* = gop.key_ptr.*;
    }

    pub fn indexFile(self: *WordIndex, path: []const u8, content: []const u8) !void {
        self.removeFile(path);

        const stable_path = try self.allocator.dupe(u8, path);
        errdefer self.allocator.free(stable_path);

        const doc_id = try self.registry.getOrCreate(stable_path);

        var words_arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
        defer words_arena.deinit();
        var words_set = std.StringHashMap(void).init(words_arena.allocator());
        var line_num: u32 = 0;
        var lines = std.mem.splitScalar(u8, content, '\n');

        while (lines.next()) |line| {
            line_num += 1;
            var tok = tokenizer.WordTokenizer{ .buf = line };
            while (tok.next()) |word| {
                if (word.len < 2) continue;

                const aa = words_arena.allocator();

                var stack_buf: [256]u8 = undefined;
                const lower_word: []const u8 = if (word.len <= stack_buf.len) blk: {
                    for (word, 0..) |c, j| stack_buf[j] = tokenizer.normalizeChar(c);
                    break :blk stack_buf[0..word.len];
                } else blk: {
                    const buf = aa.alloc(u8, word.len) catch continue;
                    for (word, 0..) |c, j| buf[j] = tokenizer.normalizeChar(c);
                    break :blk buf;
                };

                try self.indexOneToken(lower_word, doc_id, line_num, &words_set);

                var needs_split: bool = false;
                if (word.len >= 4) {
                    for (word) |c| {
                        if (c == '_' or (c >= 'A' and c <= 'Z')) {
                            needs_split = true;
                            break;
                        }
                    }
                }
                if (needs_split) {
                    var sub_toks: std.ArrayList([]const u8) = .empty;
                    defer sub_toks.deinit(aa);
                    tokenizer.splitIdentifier(word, &sub_toks, aa) catch continue;
                    for (sub_toks.items) |sub| {
                        try self.indexOneToken(sub, doc_id, line_num, &words_set);
                    }
                }
            }
        }

        if (!self.skip_file_words) {
            const compact = try self.allocator.alloc([]const u8, words_set.count());
            var ki: usize = 0;
            var wk_iter = words_set.keyIterator();
            while (wk_iter.next()) |k| : (ki += 1) {
                compact[ki] = k.*;
            }
            try self.file_words.put(stable_path, compact);
        } else {
            self.allocator.free(stable_path);
        }
        words_set.deinit();
    }

    pub fn removeFile(self: *WordIndex, path: []const u8) void {
        const removed = self.file_words.fetchRemove(path) orelse return;
        const stable_path = removed.key;
        const words_slice = removed.value;

        const doc_id = self.registry.path_to_id.get(stable_path) orelse {
            self.allocator.free(words_slice);
            self.allocator.free(stable_path);
            return;
        };
        _ = self.registry.path_to_id.remove(stable_path);
        if (doc_id < self.registry.id_to_path.items.len) {
            self.registry.id_to_path.items[doc_id] = "";
        }
        defer {
            self.allocator.free(words_slice);
            self.allocator.free(stable_path);
        }

        for (words_slice) |word| {
            const word_ptr = &word;
            if (self.index.getEntry(word_ptr.*)) |entry| {
                const hits = entry.value_ptr;
                var i: usize = 0;
                while (i < hits.items.len) {
                    if (hits.items[i].doc_id == doc_id) {
                        _ = hits.swapRemove(i);
                    } else {
                        i += 1;
                    }
                }
                if (hits.items.len == 0) {
                    const owned_word = entry.key_ptr.*;
                    hits.deinit(self.allocator);
                    _ = self.index.remove(word_ptr.*);
                    self.allocator.free(owned_word);
                }
            }
        }
    }

    pub fn search(self: *WordIndex, word: []const u8) []const WordHit {
        var buf: [512]u8 = undefined;
        const lower = blk: {
            for (word, 0..) |c, i| buf[i] = tokenizer.normalizeChar(c);
            break :blk buf[0..word.len];
        };
        if (self.index.get(lower)) |hits| return hits.items;
        return &.{};
    }

    pub fn searchDeduped(self: *WordIndex, word: []const u8, allocator: std.mem.Allocator) ![]const WordHit {
        const hits = self.search(word);
        if (hits.len == 0) return try allocator.alloc(WordHit, 0);
        if (hits.len == 1) {
            const out = try allocator.alloc(WordHit, 1);
            out[0] = hits[0];
            return out;
        }

        const DedupKey = struct { doc_id: u32, line_num: u32 };
        var seen = std.AutoHashMap(DedupKey, void).init(allocator);
        defer seen.deinit();
        try seen.ensureTotalCapacity(@intCast(hits.len));

        var result: std.ArrayList(WordHit) = .empty;
        errdefer result.deinit(allocator);
        try result.ensureTotalCapacity(allocator, hits.len);

        for (hits) |hit| {
            const key = DedupKey{ .doc_id = hit.doc_id, .line_num = hit.line_num };
            const gop = try seen.getOrPut(key);
            if (!gop.found_existing) {
                result.appendAssumeCapacity(hit);
            }
        }
        return result.toOwnedSlice(allocator);
    }

    pub fn searchPrefix(self: *WordIndex, prefix_raw: []const u8, allocator: std.mem.Allocator, max_results: usize) ![]const WordHit {
        if (prefix_raw.len == 0 or max_results == 0) return try allocator.alloc(WordHit, 0);
        var buf: [512]u8 = undefined;
        const prefix = blk: {
            for (prefix_raw, 0..) |c, i| buf[i] = tokenizer.normalizeChar(c);
            break :blk buf[0..prefix_raw.len];
        };

        var result: std.ArrayList(WordHit) = .empty;
        errdefer result.deinit(allocator);
        try result.ensureTotalCapacity(allocator, max_results);

        const DedupKey = struct { doc_id: u32, line_num: u32 };
        var seen = std.AutoHashMap(DedupKey, void).init(allocator);
        defer seen.deinit();
        try seen.ensureTotalCapacity(@intCast(max_results));

        var key_iter = self.index.keyIterator();
        outer: while (key_iter.next()) |k| {
            if (k.len <= prefix.len) continue;
            if (!std.mem.startsWith(u8, k.*, prefix)) continue;
            const hits = self.index.get(k.*) orelse continue;
            for (hits.items) |hit| {
                const dk = DedupKey{ .doc_id = hit.doc_id, .line_num = hit.line_num };
                const gop = try seen.getOrPut(dk);
                if (!gop.found_existing) {
                    result.appendAssumeCapacity(hit);
                    if (result.items.len >= max_results) break :outer;
                }
            }
        }
        return result.toOwnedSlice(allocator);
    }

    const MAGIC: u32 = 0x574F5244;
    const VERSION: u32 = 1;

    pub fn writeToDisk(self: *WordIndex, dir_path: []const u8, git_head: ?[]const u8) !void {
        var buf: [4096]u8 = undefined;
        const idx_path = try std.fmt.bufPrint(&buf, "{s}/word_index.bin", .{dir_path});
        const f = try std.Io.Dir.cwd().createFile(idx_path, .{});
        defer f.close();
        var fw = f.writer(&buf);
        const w = &fw.interface;

        try index_header.write(w, .{ .magic = MAGIC, .version = VERSION, .git_head = git_head });

        var words = std.ArrayList([]const u8).empty;
        defer words.deinit(self.allocator);
        var ki = self.index.keyIterator();
        while (ki.next()) |k| try words.append(self.allocator, k.*);
        std.mem.sort([]const u8, words.items, {}, struct {
            fn lessThan(ctx: void, a: []const u8, b: []const u8) bool {
                _ = ctx;
                return std.mem.order(u8, a, b) == .lt;
            }
        }.lessThan);

        try w.writeInt(u32, @intCast(words.items.len), .little);
        for (words.items) |word| {
            try w.writeInt(u16, @intCast(word.len), .little);
            try w.writeAll(word);
            const hits = self.index.get(word).?;
            try w.writeInt(u32, @intCast(hits.items.len), .little);
            for (hits.items) |hit| {
                try w.writeInt(u32, hit.doc_id, .little);
                try w.writeInt(u32, hit.line_num, .little);
            }
        }
        try w.flush();
    }

    pub fn readFromDisk(dir_path: []const u8, allocator: std.mem.Allocator) !?WordIndex {
        var buf: [4096]u8 = undefined;
        const idx_path = try std.fmt.bufPrint(&buf, "{s}/word_index.bin", .{dir_path});

        const content = std.Io.Dir.cwd().readFileAlloc(std.heap.page_allocator, idx_path, std.math.maxInt(usize)) catch return null;
        defer std.heap.page_allocator.free(content);

        const hdr = index_header.read(content, MAGIC, VERSION) orelse return null;
        var offset: usize = hdr.offset;

        const n_words = std.mem.readInt(u32, content[offset..][0..4], .little);
        offset += 4;

        var wi = init(allocator);
        var word_i: u32 = 0;
        while (word_i < n_words and offset + 6 <= content.len) : (word_i += 1) {
            const wlen = std.mem.readInt(u16, content[offset..][0..2], .little);
            offset += 2;
            if (offset + wlen > content.len) break;
            const word_buf = try allocator.alloc(u8, wlen);
            @memcpy(word_buf, content[offset..][0..wlen]);
            offset += wlen;

            if (offset + 4 > content.len) break;
            const n_hits = std.mem.readInt(u32, content[offset..][0..4], .little);
            offset += 4;

            var hit_list: std.ArrayList(WordHit) = .empty;
            try hit_list.ensureTotalCapacity(allocator, n_hits);
            var hi: u32 = 0;
            while (hi < n_hits and offset + 8 <= content.len) : (hi += 1) {
                const doc_id = std.mem.readInt(u32, content[offset..][0..4], .little);
                offset += 4;
                const line_num = std.mem.readInt(u32, content[offset..][0..4], .little);
                offset += 4;
                hit_list.appendAssumeCapacity(.{ .doc_id = doc_id, .line_num = line_num });
            }
            try wi.index.put(word_buf, hit_list);
        }
        return wi;
    }

    pub fn readGitHead(dir_path: []const u8) ?[40]u8 {
        var buf: [4096]u8 = undefined;
        const idx_path = std.fmt.bufPrint(&buf, "{s}/word_index.bin", .{dir_path}) catch return null;
        return index_header.readGitHeadFromFile(idx_path, MAGIC, VERSION);
    }
};

const testing = std.testing;

test "WordIndex basic index and search" {
    var wi = WordIndex.init(testing.allocator);
    defer wi.deinit();

    try wi.indexFile("src/foo.zig", "pub fn filterStages() void {\n    return;\n}");
    try wi.indexFile("src/bar.zig", "const filter = filterStages;\n");

    const hits = wi.search("filterstages");
    try testing.expect(hits.len >= 1);

    const hits2 = wi.search("filter");
    try testing.expect(hits2.len >= 1);

    const hits_none = wi.search("nonexistent");
    try testing.expectEqual(@as(usize, 0), hits_none.len);
}

test "WordIndex removeFile" {
    var wi = WordIndex.init(testing.allocator);
    defer wi.deinit();

    try wi.indexFile("src/foo.zig", "hello world");
    try testing.expect(wi.search("hello").len > 0);

    wi.removeFile("src/foo.zig");
    try testing.expectEqual(@as(usize, 0), wi.search("hello").len);
}

test "WordIndex searchPrefix" {
    var wi = WordIndex.init(testing.allocator);
    defer wi.deinit();

    try wi.indexFile("src/foo.zig", "filterStages filtered filtering");
    const results = try wi.searchPrefix("filt", testing.allocator, 10);
    defer testing.allocator.free(results);
    try testing.expect(results.len > 0);
}

test "WordIndex writeToDisk and readFromDisk" {
    const tmp_path = ".test_tmp_word_index";
    defer {
        std.Io.Dir.cwd().deleteTree(tmp_path) catch {};
    }
    std.Io.Dir.cwd().makePath(tmp_path) catch {};

    {
        var wi = WordIndex.init(testing.allocator);
        try wi.indexFile("src/test.zig", "hello world foo");
        try wi.writeToDisk(tmp_path, null);
        wi.deinit();
    }

    var loaded = (try WordIndex.readFromDisk(tmp_path, testing.allocator)) orelse {
        try testing.expect(false);
        return;
    };
    defer loaded.deinit();

    const hits = loaded.search("hello");
    try testing.expect(hits.len == 1);
}

test "WordIndex skip_file_words" {
    var wi = WordIndex.init(testing.allocator);
    wi.skip_file_words = true;
    defer wi.deinit();

    try wi.indexFile("src/foo.zig", "hello world");
    try testing.expect(wi.search("hello").len > 0);
    try testing.expectEqual(@as(usize, 0), wi.file_words.count());
}
