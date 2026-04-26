const std = @import("std");
const word_index = @import("word_index.zig");
const trigram_index_mod = @import("trigram_index.zig");

pub const SearchHit = struct {
    file_path: []const u8,
    line_num: u32,
    score: f64,
    snippet: ?[]const u8 = null,
};

pub const FileMetadata = struct {
    path: []const u8,
    word_count: u32,
    line_count: u32,
};

pub const SearchBackend = struct {
    ptr: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        searchKeyword: *const fn (*anyopaque, std.mem.Allocator, []const u8) anyerror![]SearchHit,
        searchPrefix: *const fn (*anyopaque, std.mem.Allocator, []const u8, usize) anyerror![]SearchHit,
        searchTrigram: *const fn (*anyopaque, [3]u8) []SearchHit,
        getMetadata: *const fn (*anyopaque, []const u8) anyerror!?FileMetadata,
        close: *const fn (*anyopaque) void,
    };

    pub fn searchKeyword(self: SearchBackend, allocator: std.mem.Allocator, word: []const u8) ![]SearchHit {
        return self.vtable.searchKeyword(self.ptr, allocator, word);
    }

    pub fn searchPrefix(self: SearchBackend, allocator: std.mem.Allocator, prefix: []const u8, max: usize) ![]SearchHit {
        return self.vtable.searchPrefix(self.ptr, allocator, prefix, max);
    }

    pub fn searchTrigram(self: SearchBackend, tri: [3]u8) []SearchHit {
        return self.vtable.searchTrigram(self.ptr, tri);
    }

    pub fn getMetadata(self: SearchBackend, file_path: []const u8) !?FileMetadata {
        return self.vtable.getMetadata(self.ptr, file_path);
    }

    pub fn close(self: SearchBackend) void {
        self.vtable.close(self.ptr);
    }
};

pub const SqliteSearchBackend = struct {
    wi: *word_index.WordIndex,
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator, wi: *word_index.WordIndex) SqliteSearchBackend {
        return .{
            .wi = wi,
            .allocator = allocator,
        };
    }

    pub fn backend(self: *SqliteSearchBackend) SearchBackend {
        return .{
            .ptr = @ptrCast(self),
            .vtable = &.{
                .searchKeyword = sqlSearchKeyword,
                .searchPrefix = sqlSearchPrefix,
                .searchTrigram = sqlSearchTrigram,
                .getMetadata = sqlGetMetadata,
                .close = sqlClose,
            },
        };
    }

    fn sqlSearchKeyword(ptr: *anyopaque, allocator: std.mem.Allocator, word: []const u8) anyerror![]SearchHit {
        const self: *SqliteSearchBackend = @ptrCast(@alignCast(ptr));
        const hits = self.wi.searchDeduped(word, allocator) catch &.{};
        defer if (hits.len > 0) allocator.free(hits);
        const results = try allocator.alloc(SearchHit, hits.len);
        for (hits, 0..) |hit, i| {
            results[i] = .{
                .file_path = self.wi.hitPath(hit),
                .line_num = hit.line_num,
                .score = 1.0,
            };
        }
        return results;
    }

    fn sqlSearchPrefix(ptr: *anyopaque, allocator: std.mem.Allocator, prefix: []const u8, max: usize) anyerror![]SearchHit {
        const self: *SqliteSearchBackend = @ptrCast(@alignCast(ptr));
        const hits = self.wi.searchPrefix(prefix, allocator, max) catch &.{};
        defer if (hits.len > 0) allocator.free(hits);
        const results = try allocator.alloc(SearchHit, hits.len);
        for (hits, 0..) |hit, i| {
            results[i] = .{
                .file_path = self.wi.hitPath(hit),
                .line_num = hit.line_num,
                .score = 0.8,
            };
        }
        return results;
    }

    fn sqlSearchTrigram(ptr: *anyopaque, tri: [3]u8) []SearchHit {
        _ = ptr;
        _ = tri;
        return &.{};
    }

    fn sqlGetMetadata(ptr: *anyopaque, file_path: []const u8) anyerror!?FileMetadata {
        _ = ptr;
        _ = file_path;
        return null;
    }

    fn sqlClose(ptr: *anyopaque) void {
        _ = ptr;
    }
};

pub const MmapSearchBackend = struct {
    wi: *word_index.WordIndex,
    tri: *trigram_index_mod.TrigramIndex,
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator, wi: *word_index.WordIndex, tri: *trigram_index_mod.TrigramIndex) MmapSearchBackend {
        return .{
            .wi = wi,
            .tri = tri,
            .allocator = allocator,
        };
    }

    pub fn backend(self: *MmapSearchBackend) SearchBackend {
        return .{
            .ptr = @ptrCast(self),
            .vtable = &.{
                .searchKeyword = mmapSearchKeyword,
                .searchPrefix = mmapSearchPrefix,
                .searchTrigram = mmapSearchTrigram,
                .getMetadata = mmapGetMetadata,
                .close = mmapClose,
            },
        };
    }

    fn mmapSearchKeyword(ptr: *anyopaque, allocator: std.mem.Allocator, word: []const u8) anyerror![]SearchHit {
        const self: *MmapSearchBackend = @ptrCast(@alignCast(ptr));
        const hits = self.wi.searchDeduped(word, allocator) catch &.{};
        defer if (hits.len > 0) allocator.free(hits);
        const results = try allocator.alloc(SearchHit, hits.len);
        for (hits, 0..) |hit, i| {
            results[i] = .{
                .file_path = self.wi.hitPath(hit),
                .line_num = hit.line_num,
                .score = 1.0,
            };
        }
        return results;
    }

    fn mmapSearchPrefix(ptr: *anyopaque, allocator: std.mem.Allocator, prefix: []const u8, max: usize) anyerror![]SearchHit {
        const self: *MmapSearchBackend = @ptrCast(@alignCast(ptr));
        const hits = self.wi.searchPrefix(prefix, allocator, max) catch &.{};
        defer if (hits.len > 0) allocator.free(hits);
        const results = try allocator.alloc(SearchHit, hits.len);
        for (hits, 0..) |hit, i| {
            results[i] = .{
                .file_path = self.wi.hitPath(hit),
                .line_num = hit.line_num,
                .score = 0.8,
            };
        }
        return results;
    }

    fn mmapSearchTrigram(ptr: *anyopaque, tri: [3]u8) []SearchHit {
        _ = ptr;
        _ = tri;
        return &.{};
    }

    fn mmapGetMetadata(ptr: *anyopaque, file_path: []const u8) anyerror!?FileMetadata {
        _ = ptr;
        _ = file_path;
        return null;
    }

    fn mmapClose(ptr: *anyopaque) void {
        _ = ptr;
    }
};

const testing = std.testing;

test "SqliteSearchBackend searchKeyword" {
    var wi = word_index.WordIndex.init(testing.allocator);
    defer wi.deinit();
    try wi.indexFile("src/test.zig", "pub fn filterStages() void");

    var backend_impl = SqliteSearchBackend.init(testing.allocator, &wi);
    const be = backend_impl.backend();
    const results = try be.searchKeyword(testing.allocator, "filterstages");
    defer testing.allocator.free(results);
    try testing.expect(results.len >= 1);
}