//! stage_builder.zig — Stage builder for typed, pre-allocated stage production.
//!
//! Anti-pattern fixed: VTable had exactly one implementation.
//! Replaced with direct struct methods on GuidanceJsonStageBuilderImpl.

const std = @import("std");
const types = @import("types.zig");

// =============================================================================
// GuidanceJsonStageBuilderImpl
// =============================================================================

pub const GuidanceJsonStageBuilderImpl = struct {
    allocator: std.mem.Allocator,
    doc: *const types.GuidanceDoc,
    workspace: []const u8,

    pub fn init(allocator: std.mem.Allocator, doc: *const types.GuidanceDoc, workspace: []const u8) GuidanceJsonStageBuilderImpl {
        return .{ .allocator = allocator, .doc = doc, .workspace = workspace };
    }

    pub fn stageCount(self: *const GuidanceJsonStageBuilderImpl) usize {
        const doc = self.doc;
        var n: usize = 0;
        if (doc.detail != null and (doc.detail.?.len >= 50)) n += 1;
        if (doc.comment != null and (doc.comment.?.len >= 10)) n += 1;
        if (doc.keywords.len > 0 or doc.capabilities.len > 0) n += 1;
        return @max(1, n);
    }

    pub fn fillStages(self: *GuidanceJsonStageBuilderImpl, alloc: std.mem.Allocator, out: []types.Stage) usize {
        const doc = self.doc;
        var i: usize = 0;

        if (doc.detail) |detail| {
            if (detail.len >= 50 and i < out.len) {
                out[i] = .{
                    .kind = .prose,
                    .content = alloc.dupe(u8, detail[0..@min(800, detail.len)]) catch return i,
                    .source = alloc.dupe(u8, doc.meta.source) catch return i,
                };
                i += 1;
            }
        }

        if (doc.comment) |comment| {
            if (comment.len >= 10 and i < out.len) {
                out[i] = .{
                    .kind = .prose,
                    .content = alloc.dupe(u8, comment) catch return i,
                    .source = alloc.dupe(u8, doc.meta.source) catch return i,
                };
                i += 1;
            }
        }

        if ((doc.keywords.len > 0 or doc.capabilities.len > 0) and i < out.len) {
            var meta_buf_aw: std.Io.Writer.Allocating = .init(alloc);
            errdefer meta_buf_aw.deinit();
            const mw = &meta_buf_aw.writer;

            if (doc.keywords.len > 0) {
                mw.writeAll("Keywords: ") catch return i;
                for (doc.keywords, 0..) |kw, ki| {
                    if (ki > 0) mw.writeAll(", ") catch {};
                    mw.writeAll(kw) catch {};
                }
                mw.writeByte('\n') catch {};
            }

            if (doc.capabilities.len > 0) {
                mw.writeAll("Capabilities: ") catch return i;
                for (doc.capabilities, 0..) |cap, ci| {
                    if (ci > 0) mw.writeAll(", ") catch {};
                    mw.writeAll(cap) catch {};
                }
                mw.writeByte('\n') catch {};
            }

            if (meta_buf_aw.written().len > 0) {
                out[i] = .{
                    .kind = .metadata,
                    .content = meta_buf_aw.toOwnedSlice() catch return i,
                    .source = alloc.dupe(u8, doc.meta.source) catch return i,
                };
                i += 1;
            }
        }

        return i;
    }

    pub fn isRelevant(self: *const GuidanceJsonStageBuilderImpl, query_tokens: []const []const u8) bool {
        const doc = self.doc;

        for (query_tokens) |tok| {
            if (std.ascii.indexOfIgnoreCase(doc.meta.source, tok) != null) return true;
            if (std.ascii.indexOfIgnoreCase(doc.meta.module, tok) != null) return true;
            for (doc.keywords) |kw| {
                if (std.ascii.indexOfIgnoreCase(kw, tok) != null) return true;
            }
        }
        return false;
    }

    pub fn buildStages(self: *GuidanceJsonStageBuilderImpl, allocator: std.mem.Allocator) ![]types.Stage {
        const n = self.stageCount();
        const buf = try allocator.alloc(types.Stage, n);
        errdefer allocator.free(buf);
        const filled = self.fillStages(allocator, buf);
        return buf[0..filled];
    }

    pub fn deinit(self: *GuidanceJsonStageBuilderImpl) void {
        self.allocator.destroy(self);
    }
};

pub fn createStageBuilder(allocator: std.mem.Allocator, doc: *const types.GuidanceDoc, workspace: []const u8) !*GuidanceJsonStageBuilderImpl {
    const impl = try allocator.create(GuidanceJsonStageBuilderImpl);
    impl.* = GuidanceJsonStageBuilderImpl.init(allocator, doc, workspace);
    return impl;
}

// =============================================================================
// Dispatcher: run a list of builders and collect relevant stages
// =============================================================================

pub fn collectRelevantStages(
    allocator: std.mem.Allocator,
    builders: []const *GuidanceJsonStageBuilderImpl,
    query_tokens: []const []const u8,
    max_stages: usize,
) ![]types.Stage {
    var all: std.ArrayList(types.Stage) = .empty;
    errdefer {
        types.freeStages(allocator, all.items);
        all.deinit(allocator);
    }

    for (builders) |builder| {
        if (all.items.len >= max_stages) break;
        if (!builder.isRelevant(query_tokens)) continue;

        const stages = try builder.buildStages(allocator);
        defer allocator.free(stages);

        for (stages) |s| {
            if (all.items.len >= max_stages) {
                types.freeStage(allocator, s);
                continue;
            }
            try all.append(allocator, s);
        }
    }

    return all.toOwnedSlice(allocator);
}

// =============================================================================
// Tests
// =============================================================================

const testing = std.testing;

test "GuidanceJsonStageBuilder: stageCount returns non-zero for doc with detail" {
    const allocator = testing.allocator;
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .detail = "A comprehensive test module that does many interesting things.",
        .keywords = &.{"test"},
    };
    const builder = try createStageBuilder(allocator, &doc, "/tmp");
    defer builder.deinit();

    try testing.expect(builder.stageCount() >= 1);
}

test "GuidanceJsonStageBuilder: buildStages produces prose stage from detail" {
    const allocator = testing.allocator;
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .detail = "A comprehensive test module that does many interesting things.",
        .keywords = &.{"test"},
    };
    const builder = try createStageBuilder(allocator, &doc, "/tmp");
    defer builder.deinit();

    const stages = try builder.buildStages(allocator);
    defer {
        types.freeStages(allocator, stages);
        allocator.free(stages);
    }

    try testing.expect(stages.len >= 1);
    try testing.expectEqual(types.StageKind.prose, stages[0].kind);
}

test "GuidanceJsonStageBuilder: isRelevant matches source path token" {
    const allocator = testing.allocator;
    const doc = types.GuidanceDoc{
        .arena = undefined,
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .detail = "Test detail",
        .keywords = &.{"test"},
    };
    const builder = try createStageBuilder(allocator, &doc, "/tmp");
    defer builder.deinit();

    try testing.expect(builder.isRelevant(&.{"test"}));
    try testing.expect(!builder.isRelevant(&.{"nonexistent"}));
}
