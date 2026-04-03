//! document_indexer.zig — DocumentIndexer VTable for unified document abstraction.
//!
//! Implements M1 of ROADMAP_20260403_FLUENT_GUIDANCE.md:
//! Replace ad-hoc stage assembly with a polymorphic DocumentIndexer interface
//! that unifies guidance JSON, CAPABILITY.md, SKILL.md, and source files.
//!
//! VTable pattern follows fluent-wvr (see doc/skills/fluent-wvr/SKILL.md):
//!   {ptr: *anyopaque, vtable: *const VTable} — two pointers, no inheritance.
//!
//! Memory contract:
//!   - DocumentIndexer owns its implementation struct; caller calls deinit().
//!   - produce_stages() returns an owned slice; caller calls types.freeStages()
//!     then allocator.free(slice).

const std = @import("std");
const types = @import("types.zig");

// =============================================================================
// DocumentMetadata
// =============================================================================

/// Extracted metadata from a document — keywords, capabilities, skills, anchors.
/// All slices borrow from the indexer's allocator; deinit with freeMetadata().
pub const DocumentMetadata = struct {
    keywords: []const []const u8 = &.{},
    capabilities: []const []const u8 = &.{},
    anchors: []const []const u8 = &.{},
    skills: []const []const u8 = &.{},
    used_by: []const []const u8 = &.{},
    /// Human-readable description (from comment or frontmatter).
    description: ?[]const u8 = null,
};

/// Releases allocated memory associated with a DocumentMetadata object using the provided allocator.
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
// DocumentIndexer VTable
// =============================================================================

/// Polymorphic interface for any document type that can be indexed and queried.
/// Two pointers, no inheritance — fluent-wvr pattern.
pub const DocumentIndexer = struct {
    ptr: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        /// Document type identifier: "guidance_json", "capability", "skill", "source_file"
        doc_type: *const fn (ptr: *anyopaque) []const u8,

        /// Unique identifier for deduplication (e.g., "src/guidance/staged.zig")
        doc_id: *const fn (ptr: *anyopaque) []const u8,

        /// Extract metadata: keywords, capabilities, anchors, skills.
        /// Caller owns all strings in the returned DocumentMetadata.
        extract_metadata: *const fn (
            ptr: *anyopaque,
            allocator: std.mem.Allocator,
        ) anyerror!DocumentMetadata,

        /// Produce Stage[] for explain output, filtered by query relevance.
        /// Caller owns the returned slice; free with types.freeStages() + allocator.free().
        produce_stages: *const fn (
            ptr: *anyopaque,
            allocator: std.mem.Allocator,
            query: []const u8,
            score: f32,
        ) anyerror![]types.Stage,

        /// Confidence that this document is relevant to the given query tokens (0.0–1.0).
        relevance: *const fn (
            ptr: *anyopaque,
            query_tokens: []const []const u8,
        ) f32,

        /// Release all resources owned by the implementation struct.
        deinit: *const fn (ptr: *anyopaque) void,
    };

    // ── Convenience wrappers ───────────────────────────────────────────────────

    pub fn docType(self: DocumentIndexer) []const u8 {
        return self.vtable.doc_type(self.ptr);
    }

    pub fn docId(self: DocumentIndexer) []const u8 {
        return self.vtable.doc_id(self.ptr);
    }

    pub fn extractMetadata(
        self: DocumentIndexer,
        allocator: std.mem.Allocator,
    ) !DocumentMetadata {
        return self.vtable.extract_metadata(self.ptr, allocator);
    }

    pub fn produceStages(
        self: DocumentIndexer,
        allocator: std.mem.Allocator,
        query: []const u8,
        score: f32,
    ) ![]types.Stage {
        return self.vtable.produce_stages(self.ptr, allocator, query, score);
    }

    pub fn relevance(
        self: DocumentIndexer,
        query_tokens: []const []const u8,
    ) f32 {
        return self.vtable.relevance(self.ptr, query_tokens);
    }

    pub fn deinit(self: DocumentIndexer) void {
        self.vtable.deinit(self.ptr);
    }
};

// =============================================================================
// GuidanceJsonIndexer — wraps types.GuidanceDoc
// =============================================================================
//
// Produces stages from the guidance JSON metadata: prose from detail/comment,
// code from source excerpts (when available), metadata from keywords/skills.

/// Manages guidance JSON indexing logic, owns struct state, ensures consistent key structures across operations.
pub const GuidanceJsonIndexerImpl = struct {
    allocator: std.mem.Allocator,
    doc: *const types.GuidanceDoc,
    workspace: []const u8,
};

fn guidanceJsonDocType(_: *anyopaque) []const u8 {
    return "guidance_json";
}

fn guidanceJsonDocId(ptr: *anyopaque) []const u8 {
    const self: *GuidanceJsonIndexerImpl = @ptrCast(@alignCast(ptr));
    return self.doc.meta.source;
}

fn guidanceJsonExtractMetadata(
    ptr: *anyopaque,
    allocator: std.mem.Allocator,
) anyerror!DocumentMetadata {
    const self: *GuidanceJsonIndexerImpl = @ptrCast(@alignCast(ptr));
    const doc = self.doc;

    var keywords: std.ArrayList([]const u8) = .{};
    errdefer {
        for (keywords.items) |k| allocator.free(k);
        keywords.deinit(allocator);
    }
    for (doc.keywords) |kw| try keywords.append(allocator, try allocator.dupe(u8, kw));

    var caps: std.ArrayList([]const u8) = .{};
    errdefer {
        for (caps.items) |c| allocator.free(c);
        caps.deinit(allocator);
    }
    for (doc.capabilities) |cap| try caps.append(allocator, try allocator.dupe(u8, cap));

    var skills: std.ArrayList([]const u8) = .{};
    errdefer {
        for (skills.items) |s| allocator.free(s);
        skills.deinit(allocator);
    }
    for (doc.skills) |sk| try skills.append(allocator, try allocator.dupe(u8, sk.ref));

    var used_by: std.ArrayList([]const u8) = .{};
    errdefer {
        for (used_by.items) |u| allocator.free(u);
        used_by.deinit(allocator);
    }
    for (doc.used_by) |u| try used_by.append(allocator, try allocator.dupe(u8, u));

    return .{
        .keywords = try keywords.toOwnedSlice(allocator),
        .capabilities = try caps.toOwnedSlice(allocator),
        .anchors = &.{},
        .skills = try skills.toOwnedSlice(allocator),
        .used_by = try used_by.toOwnedSlice(allocator),
        .description = if (doc.comment) |c| try allocator.dupe(u8, c) else null,
    };
}

fn guidanceJsonProduceStages(
    ptr: *anyopaque,
    allocator: std.mem.Allocator,
    query: []const u8,
    score: f32,
) anyerror![]types.Stage {
    _ = query;
    _ = score;
    const self: *GuidanceJsonIndexerImpl = @ptrCast(@alignCast(ptr));
    const doc = self.doc;

    var stages: std.ArrayList(types.Stage) = .{};
    errdefer {
        types.freeStages(allocator, stages.items);
        stages.deinit(allocator);
    }

    // Prose from module detail (comprehensive documentation)
    if (doc.detail) |detail| {
        if (detail.len >= 50) {
            try stages.append(allocator, .{
                .kind = .prose,
                .content = try allocator.dupe(u8, detail[0..@min(800, detail.len)]),
                .source = try allocator.dupe(u8, doc.meta.source),
            });
        }
    }

    // Prose from module comment (brief description)
    if (doc.comment) |comment| {
        if (comment.len >= 10) {
            try stages.append(allocator, .{
                .kind = .prose,
                .content = try allocator.dupe(u8, comment),
                .source = try allocator.dupe(u8, doc.meta.source),
            });
        }
    }

    // Metadata stage (keywords + skills)
    if (doc.keywords.len > 0 or doc.capabilities.len > 0) {
        var meta_buf: std.ArrayList(u8) = .{};
        defer meta_buf.deinit(allocator);
        const mw = meta_buf.writer(allocator);

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

        if (meta_buf.items.len > 0) {
            try stages.append(allocator, .{
                .kind = .metadata,
                .content = try meta_buf.toOwnedSlice(allocator),
                .source = try allocator.dupe(u8, doc.meta.source),
            });
        }
    }

    return stages.toOwnedSlice(allocator);
}

fn guidanceJsonRelevance(ptr: *anyopaque, query_tokens: []const []const u8) f32 {
    const self: *GuidanceJsonIndexerImpl = @ptrCast(@alignCast(ptr));
    const doc = self.doc;
    var score: f32 = 0.0;

    for (query_tokens) |tok| {
        // Check source path
        if (std.ascii.indexOfIgnoreCase(doc.meta.source, tok) != null) score += 0.3;
        // Check module name
        if (std.ascii.indexOfIgnoreCase(doc.meta.module, tok) != null) score += 0.2;
        // Check keywords
        for (doc.keywords) |kw| {
            if (std.ascii.indexOfIgnoreCase(kw, tok) != null) { score += 0.3; break; }
        }
        // Check member names
        for (doc.members) |m| {
            if (std.ascii.indexOfIgnoreCase(m.name, tok) != null) { score += 0.5; break; }
        }
    }

    return @min(1.0, score);
}

fn guidanceJsonDeinit(ptr: *anyopaque) void {
    const self: *GuidanceJsonIndexerImpl = @ptrCast(@alignCast(ptr));
    self.allocator.destroy(self);
}

const guidance_json_vtable: DocumentIndexer.VTable = .{
    .doc_type        = guidanceJsonDocType,
    .doc_id          = guidanceJsonDocId,
    .extract_metadata = guidanceJsonExtractMetadata,
    .produce_stages  = guidanceJsonProduceStages,
    .relevance       = guidanceJsonRelevance,
    .deinit          = guidanceJsonDeinit,
};

// =============================================================================
// GuidanceJsonIndexerBuilder — value-copy builder pattern (fluent-wvr §2)
// =============================================================================

/// Manages GuidanceJsonIndexerBuilder's configuration and state, ensuring proper ownership and invariants for indexing operations.
pub const GuidanceJsonIndexerBuilder = struct {
    allocator: std.mem.Allocator,
    doc: *const types.GuidanceDoc,
    workspace: []const u8,

    pub fn build(self: GuidanceJsonIndexerBuilder) !DocumentIndexer {
        const impl = try self.allocator.create(GuidanceJsonIndexerImpl);
        impl.* = .{
            .allocator = self.allocator,
            .doc = self.doc,
            .workspace = self.workspace,
        };
        return .{ .ptr = impl, .vtable = &guidance_json_vtable };
    }
};

// =============================================================================
// Tests
// =============================================================================

test "GuidanceJsonIndexer: docType returns guidance_json" {
    const allocator = std.testing.allocator;
    const doc = types.GuidanceDoc{
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .comment = "Test module",
        .detail = "A comprehensive test module with many features.",
        .keywords = &.{"test", "unit"},
    };
    const builder = GuidanceJsonIndexerBuilder{
        .allocator = allocator,
        .doc = &doc,
        .workspace = "/tmp",
    };
    const indexer = try builder.build();
    defer indexer.deinit();

    try std.testing.expectEqualStrings("guidance_json", indexer.docType());
    try std.testing.expectEqualStrings("src/test.zig", indexer.docId());
}

test "GuidanceJsonIndexer: produceStages includes detail prose" {
    const allocator = std.testing.allocator;
    const doc = types.GuidanceDoc{
        .meta = .{ .module = "test", .source = "src/test.zig" },
        .detail = "A comprehensive test module that does many interesting things and has lots of content.",
        .keywords = &.{"test"},
    };
    const builder = GuidanceJsonIndexerBuilder{
        .allocator = allocator,
        .doc = &doc,
        .workspace = "/tmp",
    };
    const indexer = try builder.build();
    defer indexer.deinit();

    const stages = try indexer.produceStages(allocator, "test", 1.0);
    defer {
        types.freeStages(allocator, stages);
        allocator.free(stages);
    }

    try std.testing.expect(stages.len >= 1);
    try std.testing.expectEqual(types.StageKind.prose, stages[0].kind);
}



