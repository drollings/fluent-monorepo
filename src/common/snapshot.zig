const std = @import("std");
const word_index = @import("word_index.zig");
const trigram_index = @import("trigram_index.zig");

pub const GuidanceSnapshot = struct {
    git_head: ?[40]u8,
    file_count: u32,
    word_index_loaded: bool,
    trigram_index_loaded: bool,

    const MAGIC: u32 = 0x4748534E;
    const VERSION: u32 = 1;

    pub fn writeSnapshot(
        allocator: std.mem.Allocator,
        dir_path: []const u8,
        git_head: ?[]const u8,
        wi: *word_index.WordIndex,
        tri: *trigram_index.TrigramIndex,
        file_count: u32,
    ) !void {
        var buf: [4096]u8 = undefined;
        const snap_path = try std.fmt.bufPrint(&buf, "{s}/guidance.snapshot", .{dir_path});
        const f = try std.fs.cwd().createFile(snap_path, .{});
        defer f.close();

        var out: std.ArrayList(u8) = .empty;
        defer out.deinit(allocator);

        std.mem.writeInt(u32, try out.addManyAsArray(allocator, 4), MAGIC, .little);
        std.mem.writeInt(u32, try out.addManyAsArray(allocator, 4), VERSION, .little);

        if (git_head) |gh| {
            const len = @min(gh.len, 40);
            try out.append(allocator, @intCast(len));
            var gh_buf: [40]u8 = [_]u8{0} ** 40;
            @memcpy(gh_buf[0..len], gh[0..len]);
            try out.appendSlice(allocator, &gh_buf);
        } else {
            try out.append(allocator, 0);
            var pad: [40]u8 = [_]u8{0} ** 40;
            try out.appendSlice(allocator, &pad);
        }

        std.mem.writeInt(u32, try out.addManyAsArray(allocator, 4), file_count, .little);
        try f.writeAll(out.items);

        try wi.writeToDisk(dir_path, git_head orelse null);
        try tri.writeToDisk(dir_path, git_head orelse null);
    }

    pub fn loadSnapshot(
        allocator: std.mem.Allocator,
        dir_path: []const u8,
    ) !?GuidanceSnapshot {
        var buf: [4096]u8 = undefined;
        const snap_path = try std.fmt.bufPrint(&buf, "{s}/guidance.snapshot", .{dir_path});
        const content = std.fs.cwd().readFileAlloc(allocator, snap_path, std.math.maxInt(usize)) catch return null;
        defer allocator.free(content);

        if (content.len < 10) return null;
        var offset: usize = 0;

        const magic = std.mem.readInt(u32, content[offset..][0..4], .little);
        offset += 4;
        if (magic != MAGIC) return null;

        const version = std.mem.readInt(u32, content[offset..][0..4], .little);
        offset += 4;
        if (version != VERSION) return null;

        var git_head: ?[40]u8 = null;
        const gh_len: usize = content[offset];
        offset += 1;
        if (offset + 40 > content.len) return null;
        if (gh_len > 0 and gh_len <= 40) {
            var gh: [40]u8 = [_]u8{0} ** 40;
            @memcpy(gh[0..gh_len], content[offset..][0..gh_len]);
            git_head = gh;
        }
        offset += 40;

        const file_count = std.mem.readInt(u32, content[offset..][0..4], .little);

        return .{
            .git_head = git_head,
            .file_count = file_count,
            .word_index_loaded = false,
            .trigram_index_loaded = false,
        };
    }

    pub fn readSnapshotGitHead(dir_path: []const u8) ?[40]u8 {
        var buf: [4096]u8 = undefined;
        const snap_path = std.fmt.bufPrint(&buf, "{s}/guidance.snapshot", .{dir_path}) catch return null;
        const content = std.fs.cwd().readFileAlloc(std.heap.page_allocator, snap_path, std.math.maxInt(usize)) catch return null;
        defer std.heap.page_allocator.free(content);

        if (content.len < 10) return null;
        var offset: usize = 0;
        const magic = std.mem.readInt(u32, content[offset..][0..4], .little);
        offset += 4;
        if (magic != MAGIC) return null;
        const version = std.mem.readInt(u32, content[offset..][0..4], .little);
        offset += 4;
        if (version != VERSION) return null;
        const gh_len: usize = content[offset];
        offset += 1;
        if (offset + 40 > content.len) return null;
        if (gh_len == 0 or gh_len > 40) return null;
        var gh: [40]u8 = [_]u8{0} ** 40;
        @memcpy(gh[0..gh_len], content[offset..][0..gh_len]);
        return gh;
    }

    pub fn isCurrent(dir_path: []const u8, current_head: []const u8) bool {
        const snap_head = readSnapshotGitHead(dir_path) orelse return false;
        if (current_head.len != 40) return false;
        var buf: [40]u8 = undefined;
        for (current_head, 0..) |c, i| buf[i] = c;
        return std.mem.eql(u8, &snap_head, &buf);
    }
};

const testing = std.testing;

test "GuidanceSnapshot write and load" {
    const dir = ".test_tmp_snap";
    defer {
        std.fs.cwd().deleteTree(dir) catch {};
    }
    std.fs.cwd().makePath(dir) catch {};

    var wi = word_index.WordIndex.init(testing.allocator);
    defer wi.deinit();
    try wi.indexFile("test.zig", "hello world");

    var tri = trigram_index.TrigramIndex.init(testing.allocator);
    defer tri.deinit();
    try tri.buildFromContent("test.zig", "hello world");

    const git_head = "abc123def456abc123def456abc123def456abcd";
    try GuidanceSnapshot.writeSnapshot(testing.allocator, dir, git_head, &wi, &tri, 1);

    const snap = try GuidanceSnapshot.loadSnapshot(testing.allocator, dir);
    try testing.expect(snap != null);
    try testing.expect(snap.?.git_head != null);
    try testing.expect(snap.?.file_count == 1);
}

test "GuidanceSnapshot isCurrent" {
    const dir = ".test_tmp_snap_current";
    defer {
        std.fs.cwd().deleteTree(dir) catch {};
    }
    std.fs.cwd().makePath(dir) catch {};

    var wi = word_index.WordIndex.init(testing.allocator);
    defer wi.deinit();

    var tri = trigram_index.TrigramIndex.init(testing.allocator);
    defer tri.deinit();

    const git_head = "0123456789abcdef0123456789abcdef01234567";
    try GuidanceSnapshot.writeSnapshot(testing.allocator, dir, git_head, &wi, &tri, 0);

    try testing.expect(GuidanceSnapshot.isCurrent(dir, git_head));
    try testing.expect(!GuidanceSnapshot.isCurrent(dir, "different_head_that_is_exactly_40_chars!"));
}