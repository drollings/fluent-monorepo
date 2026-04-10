//! stage_builder.zig — StageBuilder VTable for typed, pre-allocated stage production.
//!
//! Implements M5 of ROADMAP_20260403_FLUENT_GUIDANCE.md:
//! Each document type produces its own stages through a uniform interface,
//! reducing coupling to the monolithic executeStagedWithAliasesOriginal() function.
//!
//! VTable pattern follows fluent-wvr (see doc/skills/fluent-wvr/SKILL.md):
//!   {ptr: *anyopaque, vtable: *const VTable} — two pointers, no inheritance.
//!
//! Interface vs DocumentIndexer.produce_stages():
//!   - produce_stages(): allocates its own []Stage; caller frees
//!   - StageBuilder.fill_stages(): fills a caller-provided []Stage; zero allocation
//!   The zero-allocation path is suitable for tight loops and pre-sized buffers.

const std = @import("std");
const types = @import("types.zig");

// =============================================================================
// StageBuilder VTable
// =============================================================================

pub const StageBuilder = struct {
    ptr: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        /// Number of stages this builder will produce (upper bound for pre-allocation).
        stage_count: *const fn (ptr: *anyopaque) usize,

        /// Fill `out[0..stage_count()]` with stages. Each stage's strings are
        /// allocated with `allocator`; the caller frees them via types.freeStage().
        /// May produce fewer stages than stage_count() if content is unavailable.
        fill_stages: *const fn (
            ptr: *anyopaque,
            allocator: std.mem.Allocator,
            out: []types.Stage,
        ) usize,

        /// True if this builder is relevant to the given query tokens.
        /// Deterministic — no allocations allowed.
        is_relevant: *const fn (ptr: *anyopaque, query_tokens: []const []const u8) bool,

        /// Release resources owned by the implementation struct.
        deinit: *const fn (ptr: *anyopaque) void,
    };

    // ── Convenience wrappers ───────────────────────────────────────────────────

    pub fn stageCount(self: StageBuilder) usize {
        return self.vtable.stage_count(self.ptr);
    }

    /// Allocate and fill a stage slice. Caller must free with types.freeStages + allocator.free.
    pub fn buildStages(self: StageBuilder, allocator: std.mem.Allocator) ![]types.Stage {
        const n = self.stageCount();
        const buf = try allocator.alloc(types.Stage, n);
        errdefer allocator.free(buf);
        const filled = self.vtable.fill_stages(self.ptr, allocator, buf);
        // Shrink to actual count (no realloc — caller still owns original length).
        return buf[0..filled];
    }

    pub fn isRelevant(self: StageBuilder, query_tokens: []const []const u8) bool {
        return self.vtable.is_relevant(self.ptr, query_tokens);
    }

    pub fn deinit(self: StageBuilder) void {
        self.vtable.deinit(self.ptr);
    }
};

// =============================================================================
// GuidanceJsonStageBuilder — wraps types.GuidanceDoc
// =============================================================================
//
// Produces up to 4 stages: prose from detail, prose from comment,
// code from top member excerpts, metadata from keywords/capabilities.

/// Manages guidance stage construction with fixed buffers; owned by the builder; ensures safe initialization.
pub const GuidanceJsonStageBuilderImpl = struct {
    allocator: std.mem.Allocator,
    doc: *const types.GuidanceDoc,
    workspace: []const u8,
};

/// Counts valid stage indices in a Zig source file, returning the total count.
fn gjsbStageCount(ptr: *anyopaque) usize {
    const self: *GuidanceJsonStageBuilderImpl = @ptrCast(@alignCast(ptr));
    const doc = self.doc;
    var n: usize = 0;
    if (doc.detail != null and (doc.detail.?.len >= 50)) n += 1;
    if (doc.comment != null and (doc.comment.?.len >= 10)) n += 1;
    if (doc.keywords.len > 0 or doc.capabilities.len > 0) n += 1;
    return @max(1, n); // At least 1 (metadata fallback).
}

/// Processes a Zig source file to fill stage data structures with allocated memory.
fn gjsbFillStages(
    ptr: *anyopaque,
    allocator: std.mem.Allocator,
    out: []types.Stage,
) usize {
    const self: *GuidanceJsonStageBuilderImpl = @ptrCast(@alignCast(ptr));
    const doc = self.doc;
    var i: usize = 0;

    // Prose: detail (comprehensive documentation)
    if (doc.detail) |detail| {
        if (detail.len >= 50 and i < out.len) {
            out[i] = .{
                .kind = .prose,
                .content = allocator.dupe(u8, detail[0..@min(800, detail.len)]) catch return i,
                .source = allocator.dupe(u8, doc.meta.source) catch return i,
            };
            i += 1;
        }
    }

    // Prose: comment (brief description)
    if (doc.comment) |comment| {
        if (comment.len >= 10 and i < out.len) {
            out[i] = .{
                .kind = .prose,
                .content = allocator.dupe(u8, comment) catch return i,
                .source = allocator.dupe(u8, doc.meta.source) catch return i,
            };
            i += 1;
        }
    }

    // Metadata: keywords + capabilities
    if ((doc.keywords.len > 0 or doc.capabilities.len > 0) and i < out.len) {
        var meta_buf: std.ArrayList(u8) = .{};
        defer meta_buf.deinit(allocator);
        const mw = meta_buf.writer(allocator);

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

        if (meta_buf.items.len > 0) {
            out[i] = .{
                .kind = .metadata,
                .content = meta_buf.toOwnedSlice(allocator) catch return i,
                .source = allocator.dupe(u8, doc.meta.source) catch return i,
            };
            i += 1;
        }
    }

    return i;
}

/// Checks if a given pointer and tokens are valid Zig syntax, returning true or false.
fn gjsbIsRelevant(ptr: *anyopaque, query_tokens: []const []const u8) bool {
    const self: *GuidanceJsonStageBuilderImpl = @ptrCast(@alignCast(ptr));
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

/// Cleans up resources by deallocating the provided pointer in Zig.
fn gjsbDeinit(ptr: *anyopaque) void {
    const self: *GuidanceJsonStageBuilderImpl = @ptrCast(@alignCast(ptr));
    self.allocator.destroy(self);
}

const guidance_json_stage_vtable: StageBuilder.VTable = .{
    .stage_count = gjsbStageCount,
    .fill_stages = gjsbFillStages,
    .is_relevant = gjsbIsRelevant,
    .deinit = gjsbDeinit,
};

/// Manages guidance stage builder factories, owns configuration contracts, ensures stable access patterns.
pub const GuidanceJsonStageBuilderFactory = struct {
    allocator: std.mem.Allocator,
    doc: *const types.GuidanceDoc,
    workspace: []const u8,

    pub fn build(self: GuidanceJsonStageBuilderFactory) !StageBuilder {
        const impl = try self.allocator.create(GuidanceJsonStageBuilderImpl);
        impl.* = .{
            .allocator = self.allocator,
            .doc = self.doc,
            .workspace = self.workspace,
        };
        return .{ .ptr = impl, .vtable = &guidance_json_stage_vtable };
    }
};

// =============================================================================
// Dispatcher: run a list of StageBuilders and collect relevant stages
// =============================================================================

/// Collects relevant Zig stage structures from allocator and builders, returning a slice of stages.
pub fn collectRelevantStages(
    allocator: std.mem.Allocator,
    builders: []const StageBuilder,
    query_tokens: []const []const u8,
    max_stages: usize,
) ![]types.Stage {
    var all: std.ArrayList(types.Stage) = .{};
    errdefer {
        types.freeStages(allocator, all.items);
        all.deinit(allocator);
    }

    for (builders) |builder| {
        if (all.items.len >= max_stages) break;
        if (!builder.isRelevant(query_tokens)) continue;

        const stages = try builder.buildStages(allocator);
        defer allocator.free(stages); // Only the outer slice; strings are moved below.

        for (stages) |s| {
            if (all.items.len >= max_stages) {
                // Free stages we won't use.
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
