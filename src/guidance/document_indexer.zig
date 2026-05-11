//! document_indexer.zig — Document indexer for Guidance JSON documents.
//!
//! Anti-pattern fixed: VTable had exactly one implementation.
//! Replaced with direct struct methods on GuidanceJsonIndexerImpl.

const std = @import("std");
const types = @import("types.zig");

// =============================================================================
// DocumentMetadata
// =============================================================================

pub const DocumentMetadata = struct {
    keywords: []const []const u8 = &.{},
    capabilities: []const []const u8 = &.{},
    anchors: []const []const u8 = &.{},
    skills: []const []const u8 = &.{},
    used_by: []const []const u8 = &.{},
    description: ?[]const u8 = null,
};

pub fn freeMetadata(allocator: std.mem.Allocator, m: DocumentMetadata) void {
    for (m.keywords) |k| allocator.free(k);
    allocator.free(m.keywords);
    for (m.capabilities) |c| allocator.free(c);
    allocator.free(m.capabilities);
    for (m.anchors) |a| allocator.free(a);
    allocator.free(m.anchors);
    for (m.skills) |s| allocator.free(s);
    allocator.free(m.skills);
    for (m.used_by) |u| allocator.free(u);
    allocator.free(m.used_by);
    if (m.description) |d| allocator.free(d);
}

// =============================================================================
// GuidanceJsonIndexerImpl
// =============================================================================

pub const GuidanceJsonIndexerImpl = struct {
    allocator: std.mem.Allocator,
    doc: *const types.GuidanceDoc,
    workspace: []const u8,

    pub fn init(allocator: std.mem.Allocator, doc: *const types.GuidanceDoc, workspace: []const u8) GuidanceJsonIndexerImpl {
        return .{ .allocator = allocator, .doc = doc, .workspace = workspace };
    }

    pub fn docType(_: *const GuidanceJsonIndexerImpl) []const u8 {
        return "guidance_json";
    }

    pub fn docId(self: *const GuidanceJsonIndexerImpl) []const u8 {
        return self.doc.meta.source;
    }

    pub fn extractMetadata(self: *GuidanceJsonIndexerImpl, alloc: std.mem.Allocator) !DocumentMetadata {
        const doc = self.doc;

        var keywords: std.ArrayList([]const u8) = .empty;
        errdefer {
            for (keywords.items) |k| alloc.free(k);
            keywords.deinit(alloc);
        }
        for (doc.keywords) |kw| try keywords.append(alloc, try alloc.dupe(u8, kw));

        var caps: std.ArrayList([]const u8) = .empty;
        errdefer {
            for (caps.items) |c| alloc.free(c);
            caps.deinit(alloc);
        }
        for (doc.capabilities) |cap| try caps.append(alloc, try alloc.dupe(u8, cap));

        var skills: std.ArrayList([]const u8) = .empty;
        errdefer {
            for (skills.items) |s| alloc.free(s);
            skills.deinit(alloc);
        }
        for (doc.skills) |sk| try skills.append(alloc, try alloc.dupe(u8, sk.ref));

        var used_by: std.ArrayList([]const u8) = .empty;
        errdefer {
            for (used_by.items) |u| alloc.free(u);
            used_by.deinit(alloc);
        }
        for (doc.used_by) |u| try used_by.append(alloc, try alloc.dupe(u8, u));

        return .{
            .keywords = try keywords.toOwnedSlice(alloc),
            .capabilities = try caps.toOwnedSlice(alloc),
            .anchors = &.{},
            .skills = try skills.toOwnedSlice(alloc),
            .used_by = try used_by.toOwnedSlice(alloc),
            .description = if (doc.comment) |c| try alloc.dupe(u8, c) else null,
        };
    }

    pub fn produceStages(self: *GuidanceJsonIndexerImpl, alloc: std.mem.Allocator, query: []const u8, score: f32) ![]types.Stage {
        _ = query;
        _ = score;
        const doc = self.doc;

        var stages: std.ArrayList(types.Stage) = .empty;
        errdefer {
            types.freeStages(alloc, stages.items);
            stages.deinit(alloc);
        }

        if (doc.detail) |detail| {
            if (detail.len >= 50) {
                try stages.append(alloc, .{
                    .kind = .prose,
                    .content = try alloc.dupe(u8, detail[0..@min(800, detail.len)]),
                    .source = try alloc.dupe(u8, doc.meta.source),
                });
            }
        }

        if (doc.comment) |comment| {
            if (comment.len >= 10) {
                try stages.append(alloc, .{
                    .kind = .prose,
                    .content = try alloc.dupe(u8, comment),
                    .source = try alloc.dupe(u8, doc.meta.source),
                });
            }
        }

        if (doc.keywords.len > 0 or doc.capabilities.len > 0) {
            var meta_buf_aw: std.Io.Writer.Allocating = .init(alloc);
            errdefer meta_buf_aw.deinit();
            const mw = &meta_buf_aw.writer;

            if (doc.keywords.len > 0) {
                try mw.writeAll("Keywords: ");
                for (doc.keywords, 0..) |kw, i| {
                    if (i > 0) try mw.writeAll(", ");
                    try mw.writeAll(kw);
                }
                try mw.writeAll("\n");
            }

            if (doc.capabilities.len > 0) {
                try mw.writeAll("Capabilities: ");
                for (doc.capabilities, 0..) |cap, i| {
                    if (i > 0) try mw.writeAll(", ");
                    try mw.writeAll(cap);
                }
                try mw.writeAll("\n");
            }

            if (meta_buf_aw.written().len > 0) {
                try stages.append(alloc, .{
                    .kind = .metadata,
                    .content = try meta_buf_aw.toOwnedSlice(),
                    .source = try alloc.dupe(u8, doc.meta.source),
                });
            }
        }

        return stages.toOwnedSlice(alloc);
    }

    pub fn relevance(self: *const GuidanceJsonIndexerImpl, query_tokens: []const []const u8) f32 {
        const doc = self.doc;
        var score: f32 = 0.0;

        for (query_tokens) |tok| {
            if (std.ascii.indexOfIgnoreCase(doc.meta.source, tok) != null) score += 0.3;
            if (std.ascii.indexOfIgnoreCase(doc.meta.module, tok) != null) score += 0.2;
            for (doc.keywords) |kw| {
                if (std.ascii.indexOfIgnoreCase(kw, tok) != null) {
                    score += 0.3;
                    break;
                }
            }
            for (doc.members) |m| {
                if (std.ascii.indexOfIgnoreCase(m.name, tok) != null) {
                    score += 0.5;
                    break;
                }
            }
        }

        return @min(1.0, score);
    }

    pub fn deinit(self: *GuidanceJsonIndexerImpl) void {
        self.allocator.destroy(self);
    }
};

/// Convenience type alias — callers hold a pointer to the implementation.
pub const Indexer = GuidanceJsonIndexerImpl;

pub fn createIndexer(allocator: std.mem.Allocator, doc: *const types.GuidanceDoc, workspace: []const u8) !*GuidanceJsonIndexerImpl {
    const impl = try allocator.create(GuidanceJsonIndexerImpl);
    impl.* = GuidanceJsonIndexerImpl.init(allocator, doc, workspace);
    return impl;
}

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;

test "GuidanceJsonIndexer: docType returns guidance_json" {
    const allocator = testing.allocator;
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .comment = "Test module",
        .detail = "A comprehensive test module with many features.",
        .keywords = &.{ "test", "unit" },
    };
    const indexer = try createIndexer(allocator, &doc, "/tmp");
    defer indexer.deinit();

    try testing.expectEqualStrings("guidance_json", indexer.docType());
    try testing.expectEqualStrings("src/test.zig", indexer.docId());
}

test "GuidanceJsonIndexer: produceStages includes detail prose" {
    const allocator = testing.allocator;
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .detail = "A comprehensive test module that does many interesting things and has lots of content.",
        .keywords = &.{"test"},
    };
    const indexer = try createIndexer(allocator, &doc, "/tmp");
    defer indexer.deinit();

    const stages = try indexer.produceStages(allocator, "test", 1.0);
    defer {
        types.freeStages(allocator, stages);
        allocator.free(stages);
    }

    try testing.expect(stages.len >= 1);
    try testing.expectEqual(types.StageKind.prose, stages[0].kind);
}
