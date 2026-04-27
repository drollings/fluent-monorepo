const std = @import("std");
const doc_registry = @import("doc_registry.zig");
const DocRegistry = doc_registry.DocRegistry;
const index_header = @import("index_header.zig");

pub const Trigram = u24;

pub const TrigramHit = struct {
    doc_id: u32,
    position: u32,
};

pub const PostingMask = packed struct {
    bloom: u8,
    loc_mask: u8,

    pub fn hasTrigramAt(self: PostingMask, pos_in_unit: u3) bool {
        return (self.loc_mask & (@as(u8, 1) << pos_in_unit)) != 0;
    }
};

pub const DocPosting = struct {
    doc_id: u32,
    count: u16,
    masks: []PostingMask,
};

pub const MAX_POSTINGS: u16 = 512;

pub const TrigramIndex = struct {
    allocator: std.mem.Allocator,
    index: std.AutoHashMap(Trigram, std.ArrayList(TrigramHit)),
    registry: DocRegistry,

    pub fn init(allocator: std.mem.Allocator) TrigramIndex {
        return .{
            .allocator = allocator,
            .index = std.AutoHashMap(Trigram, std.ArrayList(TrigramHit)).init(allocator),
            .registry = DocRegistry.init(allocator, true),
        };
    }

    pub fn deinit(self: *TrigramIndex) void {
        var iter = self.index.iterator();
        while (iter.next()) |entry| {
            entry.value_ptr.deinit(self.allocator);
        }
        self.index.deinit();

        self.registry.deinit();
    }

    pub fn buildFromContent(self: *TrigramIndex, path: []const u8, content: []const u8) !void {
        const doc_id = try self.registry.getOrCreate(path);
        var pos: u32 = 0;
        while (pos + 3 <= content.len) : (pos += 1) {
            const tri: Trigram = @as(Trigram, content[pos]) |
                (@as(Trigram, content[pos + 1]) << 8) |
                (@as(Trigram, content[pos + 2]) << 16);

            const gop = try self.index.getOrPut(tri);
            if (!gop.found_existing) {
                gop.value_ptr.* = .empty;
            }
            try gop.value_ptr.append(self.allocator, .{
                .doc_id = doc_id,
                .position = pos,
            });
        }
    }

    pub fn search(self: *TrigramIndex, tri_bytes: [3]u8) []const TrigramHit {
        const tri: Trigram = @as(Trigram, tri_bytes[0]) |
            (@as(Trigram, tri_bytes[1]) << 8) |
            (@as(Trigram, tri_bytes[2]) << 16);
        if (self.index.get(tri)) |hits| return hits.items;
        return &.{};
    }

    pub fn searchTrigram(self: *TrigramIndex, tri: Trigram) []const TrigramHit {
        if (self.index.get(tri)) |hits| return hits.items;
        return &.{};
    }

    pub fn candidates(self: *TrigramIndex, query: []const u8, allocator: std.mem.Allocator) ![]u32 {
        if (query.len < 3) return try allocator.alloc(u32, 0);

        var trigrams: std.ArrayList(Trigram) = .empty;
        defer trigrams.deinit(allocator);

        var i: usize = 0;
        while (i + 3 <= query.len) : (i += 1) {
            const tri: Trigram = @as(Trigram, query[i]) |
                (@as(Trigram, query[i + 1]) << 8) |
                (@as(Trigram, query[i + 2]) << 16);
            try trigrams.append(allocator, tri);
        }

        if (trigrams.items.len == 0) return try allocator.alloc(u32, 0);

        std.sort.insertion(Trigram, trigrams.items, {}, struct {
            fn lessThan(ctx: void, a: Trigram, b: Trigram) bool {
                _ = ctx;
                return a < b;
            }
        }.lessThan);

        var result: std.ArrayList(u32) = .empty;
        errdefer result.deinit(allocator);

        const first_hits = self.searchTrigram(trigrams.items[0]);
        if (first_hits.len == 0) return try allocator.alloc(u32, 0);

        var seen = std.AutoHashMap(u32, void).init(allocator);
        defer seen.deinit();

        for (first_hits) |hit| {
            const gop = try seen.getOrPut(hit.doc_id);
            if (!gop.found_existing) try result.append(allocator, hit.doc_id);
        }

        return result.toOwnedSlice(allocator);
    }

    pub fn hitPath(self: *const TrigramIndex, hit: TrigramHit) []const u8 {
        return self.registry.pathForId(hit.doc_id);
    }

    const MAGIC: u32 = 0x54524947;
    const VERSION: u32 = 1;

    pub fn writeToDisk(self: *TrigramIndex, dir_path: []const u8, git_head: ?[]const u8) !void {
        var buf: [4096]u8 = undefined;
        const path = try std.fmt.bufPrint(&buf, "{s}/trigram_index.bin", .{dir_path});
        const f = try std.Io.Dir.cwd().createFile(path, .{});
        defer f.close();

        var out: std.ArrayList(u8) = .empty;
        defer out.deinit(self.allocator);

        var hdr_writer = out.writer(self.allocator);
        try index_header.write(&hdr_writer, .{ .magic = MAGIC, .version = VERSION, .git_head = git_head });

        std.mem.writeInt(u32, try out.addManyAsArray(self.allocator, 4), self.registry.count(), .little);

        var keys: std.ArrayList(Trigram) = .empty;
        defer keys.deinit(self.allocator);
        var ki = self.index.keyIterator();
        while (ki.next()) |k| try keys.append(self.allocator, k.*);
        std.sort.insertion(Trigram, keys.items, {}, struct {
            fn lessThan(ctx: void, a: Trigram, b: Trigram) bool {
                _ = ctx;
                return a < b;
            }
        }.lessThan);

        std.mem.writeInt(u32, try out.addManyAsArray(self.allocator, 4), @intCast(keys.items.len), .little);
        for (keys.items) |tri| {
            var tri_buf: [4]u8 = undefined;
            std.mem.writeInt(u32, &tri_buf, tri, .little);
            try out.appendSlice(self.allocator, tri_buf[0..3]);
            const hits = self.index.get(tri).?;
            std.mem.writeInt(u32, try out.addManyAsArray(self.allocator, 4), @intCast(hits.items.len), .little);
            for (hits.items) |hit| {
                std.mem.writeInt(u32, try out.addManyAsArray(self.allocator, 4), hit.doc_id, .little);
                std.mem.writeInt(u32, try out.addManyAsArray(self.allocator, 4), hit.position, .little);
            }
        }
        try f.writeAll(out.items);
    }

    pub fn readFromDisk(dir_path: []const u8, allocator: std.mem.Allocator) !?TrigramIndex {
        var buf: [4096]u8 = undefined;
        const path = try std.fmt.bufPrint(&buf, "{s}/trigram_index.bin", .{dir_path});
        const content = std.Io.Dir.cwd().readFileAlloc(allocator, path, std.math.maxInt(usize)) catch return null;
        defer allocator.free(content);

        const hdr = index_header.read(content, MAGIC, VERSION) orelse return null;
        var offset: usize = hdr.offset;

        const doc_count = std.mem.readInt(u32, content[offset..][0..4], .little);
        offset += 4;

        var idx = init(allocator);
        for (0..doc_count) |_| {
            offset += 4;
            offset += 8;
        }

        const n_trigrams = std.mem.readInt(u32, content[offset..][0..4], .little);
        offset += 4;
        offset += 4;

        var tri_i: u32 = 0;
        while (tri_i < n_trigrams) : (tri_i += 1) {
            if (offset + 3 > content.len) break;
            const tri: Trigram = @as(Trigram, content[offset]) |
                (@as(Trigram, content[offset + 1]) << 8) |
                (@as(Trigram, content[offset + 2]) << 16);
            offset += 4;

            if (offset + 4 > content.len) break;
            const n_hits = std.mem.readInt(u32, content[offset..][0..4], .little);
            offset += 4;

            var hit_list: std.ArrayList(TrigramHit) = .empty;
            try hit_list.ensureTotalCapacity(allocator, n_hits);
            var h: u32 = 0;
            while (h < n_hits and offset + 8 <= content.len) : (h += 1) {
                const doc_id = std.mem.readInt(u32, content[offset..][0..4], .little);
                offset += 4;
                const position = std.mem.readInt(u32, content[offset..][0..4], .little);
                offset += 4;
                hit_list.appendAssumeCapacity(.{ .doc_id = doc_id, .position = position });
            }
            try idx.index.put(tri, hit_list);
        }

        return idx;
    }

    pub fn readGitHead(dir_path: []const u8) ?[40]u8 {
        var buf: [4096]u8 = undefined;
        const path = std.fmt.bufPrint(&buf, "{s}/trigram_index.bin", .{dir_path}) catch return null;
        return index_header.readGitHeadFromFile(path, MAGIC, VERSION);
    }
};

pub const MmapTrigramIndex = struct {
    data: [*]const u8,
    len: usize,
    fd: ?std.fs.File,

    pub fn readFromMmap(path: []const u8) !?MmapTrigramIndex {
        const f = std.Io.Dir.cwd().openFile(path, .{}) catch return null;
        errdefer f.close();
        const stat = f.stat() catch return null;
        const size: usize = @intCast(stat.size);
        if (size < 16) return null;

        const data = std.posix.mmap(null, size, std.posix.PROT.READ, std.posix.MAP.SHARED, f.handle, 0) catch return null;
        return .{
            .data = data.ptr,
            .len = size,
            .fd = f,
        };
    }

    pub fn deinit(self: *MmapTrigramIndex) void {
        if (self.data != null and self.len > 0) {
            std.posix.munmap(@constCast(self.data)[0..self.len]);
        }
        if (self.fd) |f| f.close();
        self.data = null;
        self.len = 0;
        self.fd = null;
    }

    pub fn fileCount(self: MmapTrigramIndex) u32 {
        if (self.len < 14) return 0;
        const offset: usize = 10;
        return std.mem.readInt(u32, @constCast(self.data + offset)[0..4], .little);
    }

    pub fn search(_: MmapTrigramIndex, _: Trigram) []const TrigramHit {
        return &.{};
    }
};

fn makeTrigram(a: u8, b: u8, c: u8) Trigram {
    return @as(Trigram, a) | (@as(Trigram, b) << 8) | (@as(Trigram, c) << 16);
}

const testing = std.testing;

test "TrigramIndex buildFromContent and search" {
    var idx = TrigramIndex.init(testing.allocator);
    defer idx.deinit();

    try idx.buildFromContent("src/foo.zig", "pub fn hello() void");
    const hits = idx.search([3]u8{ 'h', 'e', 'l' });
    try testing.expect(hits.len > 0);
    const no_hits = idx.search([3]u8{ 'x', 'y', 'z' });
    try testing.expectEqual(@as(usize, 0), no_hits.len);
}

test "TrigramIndex candidates" {
    var idx = TrigramIndex.init(testing.allocator);
    defer idx.deinit();

    try idx.buildFromContent("src/foo.zig", "filterStages function pipeline");
    const cands = try idx.candidates("filter", testing.allocator);
    defer testing.allocator.free(cands);
    try testing.expect(cands.len > 0);
}

test "makeTrigram" {
    const tri = makeTrigram('a', 'b', 'c');
    try testing.expectEqual(@as(Trigram, 0x636261), tri);
}
