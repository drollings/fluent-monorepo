//! infer_capabilities.zig — M4: InferCapabilities — Capability Discovery Without CAPABILITY.md
//!
//! Implements M4 of ROADMAP_20260403_FLUENT_GUIDANCE.md:
//! When doc/capabilities/ doesn't exist (or is incomplete), infer capabilities
//! from codebase structure, naming conventions, and module comments.
//!
//! Four-pass inference (decreasing confidence):
//!   Pass 1: Naming conventions  (0.9) — "vector_db.zig" + "vector_search.zig" → "vector-search"
//!   Pass 2: Import graph cluster (0.8) — modules that import each other form capability clusters
//!   Pass 3: Module comments      (0.7) — "//! Vector similarity search..." → "vector-search"
//!   Pass 4: File grouping        (0.5) — all files in src/vector/ → "vector" capability
//!
//! Memory: all strings in InferredCapability are allocator-owned. Call freeInferredCapabilities().

const std = @import("std");
const codebase_map_mod = @import("codebase_map.zig");
const vector_db_mod = @import("vector");

const CodebaseMap = codebase_map_mod.CodebaseMap;
const DirectoryEntry = codebase_map_mod.DirectoryEntry;
const GuidanceDb = vector_db_mod.GuidanceDb;

// =============================================================================
// InferredCapability
// =============================================================================

/// Defines an inference method for capability analysis, managing ownership and ensuring correctness in inference workflows.
pub const InferenceMethod = enum {
    naming_convention,
    import_cluster,
    module_comment,
    file_grouping,
};

/// Tracks inferred capabilities for inference tasks, managed centrally with ownership model; key invariant is accurate capability representation.
pub const InferredCapability = struct {
    name: []const u8,
    description: []const u8,
    source_files: []const []const u8,
    confidence: f32,
    method: InferenceMethod,
};

/// Releases an inferred capability from an allocator, ensuring proper memory cleanup.
pub fn freeInferredCapability(allocator: std.mem.Allocator, cap: InferredCapability) void {
    allocator.free(cap.name);
    allocator.free(cap.description);
    for (cap.source_files) |f| allocator.free(f);
    allocator.free(cap.source_files);
}

/// Releases allocated memory by freeing inferred capabilities for the given allocator.
pub fn freeInferredCapabilities(allocator: std.mem.Allocator, caps: []InferredCapability) void {
    for (caps) |cap| freeInferredCapability(allocator, cap);
    allocator.free(caps);
}

// =============================================================================
// Main entry point
// =============================================================================

/// Analyzes allocation data to infer system capabilities, returning a list of detected capability types.
pub fn inferCapabilities(
    allocator: std.mem.Allocator,
    map: *const CodebaseMap,
    db: *GuidanceDb,
) ![]InferredCapability {
    var caps: std.ArrayList(InferredCapability) = .{};
    errdefer {
        for (caps.items) |cap| freeInferredCapability(allocator, cap);
        caps.deinit(allocator);
    }

    // Pass 1: Naming conventions (confidence 0.9)
    try inferFromNaming(allocator, map, &caps);

    // Pass 2: Import graph clustering (confidence 0.8)
    try inferFromImportGraph(allocator, db, &caps);

    // Pass 3: Module comment analysis (confidence 0.7)
    try inferFromModuleComments(allocator, db, &caps);

    // Pass 4: File grouping by directory (confidence 0.5)
    try inferFromFileGrouping(allocator, map, &caps);

    return deduplicateCapabilities(allocator, caps.items);
}

// =============================================================================
// Pass 1: Naming conventions
// =============================================================================

/// Extracts the leading byte of a Zig string slice, returning it as a u8.
fn extractPrefix(stem: []const u8) []const u8 {
    // Find first separator: underscore, hyphen, or number boundary.
    for (stem, 0..) |c, i| {
        if (c == '_' or c == '-') return stem[0..i];
        // CamelCase: lowercase→uppercase transition after position 0
        if (i > 2 and std.ascii.isUpper(c) and std.ascii.isLower(stem[i - 1])) return stem[0..i];
    }
    return stem;
}

/// Converts a path array into a stemmed string slice for Zig processing.
fn pathStem(path: []const u8) []const u8 {
    const base = std.fs.path.basename(path);
    const ext_pos = std.mem.lastIndexOfScalar(u8, base, '.') orelse return base;
    return base[0..ext_pos];
}

/// Generates a descriptive slice from allocation and file data.
fn generateDescription(
    allocator: std.mem.Allocator,
    name: []const u8,
    files: []const []const u8,
) ![]u8 {
    return std.fmt.allocPrint(allocator, "Inferred capability '{s}' from {d} source file(s) by naming convention.", .{ name, files.len });
}

/// Analyzes code names to infer capability types, returning a map of inferred capabilities.
fn inferFromNaming(
    allocator: std.mem.Allocator,
    map: *const CodebaseMap,
    caps: *std.ArrayList(InferredCapability),
) !void {
    // Group files by stem prefix.
    var groups: std.StringHashMapUnmanaged(std.ArrayList([]const u8)) = .{};
    defer {
        var it = groups.valueIterator();
        while (it.next()) |v| v.deinit(allocator);
        groups.deinit(allocator);
    }

    for (map.tree) |entry| {
        if (entry.kind != .file) continue;
        const ext = entry.extension orelse continue;
        // Only index source files.
        if (!isSupportedExtension(ext)) continue;

        const stem = pathStem(entry.path);
        const prefix = extractPrefix(stem);
        if (prefix.len < 3) continue; // Skip trivially short prefixes.

        const gop = try groups.getOrPut(allocator, prefix);
        if (!gop.found_existing) {
            gop.value_ptr.* = .{};
        }
        try gop.value_ptr.append(allocator, entry.path);
    }

    var it = groups.iterator();
    while (it.next()) |entry| {
        const prefix = entry.key_ptr.*;
        const files = entry.value_ptr.*;
        if (files.items.len < 2) continue; // Need ≥ 2 files.

        var source_files = try allocator.alloc([]const u8, files.items.len);
        for (files.items, 0..) |f, i| source_files[i] = try allocator.dupe(u8, f);
        errdefer {
            for (source_files) |f| allocator.free(f);
            allocator.free(source_files);
        }

        const name = try toCapabilityName(allocator, prefix);
        errdefer allocator.free(name);
        const description = try generateDescription(allocator, name, source_files);
        errdefer allocator.free(description);

        try caps.append(allocator, .{
            .name = name,
            .description = description,
            .source_files = source_files,
            .confidence = 0.9,
            .method = .naming_convention,
        });
    }
}

// =============================================================================
// Pass 2: Import graph clustering
// =============================================================================

/// Analyzes import graph data to infer capability types, returning a list of inferred capabilities.
fn inferFromImportGraph(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    caps: *std.ArrayList(InferredCapability),
) !void {
    // Query DB for modules with used_by relationships, group by directory prefix.
    // This is a simplified version: cluster modules that share a common source directory prefix.
    const results = db.searchWithAliases(allocator, "module import", 30, null) catch return;
    defer {
        for (results) |r| GuidanceDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    // Group by directory.
    var dir_groups: std.StringHashMapUnmanaged(std.ArrayList([]const u8)) = .{};
    defer {
        var it = dir_groups.valueIterator();
        while (it.next()) |v| v.deinit(allocator);
        dir_groups.deinit(allocator);
    }

    for (results) |r| {
        const dir = std.fs.path.dirname(r.source) orelse continue;
        // Skip top-level files (no parent directory grouping).
        if (std.mem.indexOfScalar(u8, dir, '/') == null and
            std.mem.indexOfScalar(u8, dir, '\\') == null) continue;

        const gop = try dir_groups.getOrPut(allocator, dir);
        if (!gop.found_existing) gop.value_ptr.* = .{};
        // Only add if not already present.
        var found = false;
        for (gop.value_ptr.items) |existing| {
            if (std.mem.eql(u8, existing, r.source)) {
                found = true;
                break;
            }
        }
        if (!found) try gop.value_ptr.append(allocator, r.source);
    }

    var it = dir_groups.iterator();
    while (it.next()) |entry| {
        const dir = entry.key_ptr.*;
        const files = entry.value_ptr.*;
        if (files.items.len < 2) continue;

        const dir_base = std.fs.path.basename(dir);
        const name = try toCapabilityName(allocator, dir_base);
        errdefer allocator.free(name);

        var source_files = try allocator.alloc([]const u8, files.items.len);
        for (files.items, 0..) |f, i| source_files[i] = try allocator.dupe(u8, f);
        errdefer {
            for (source_files) |f| allocator.free(f);
            allocator.free(source_files);
        }

        const description = try std.fmt.allocPrint(allocator, "Inferred capability '{s}' from import cluster ({d} modules).", .{ name, files.items.len });
        errdefer allocator.free(description);

        try caps.append(allocator, .{
            .name = name,
            .description = description,
            .source_files = source_files,
            .confidence = 0.8,
            .method = .import_cluster,
        });
    }
}

// =============================================================================
// Pass 3: Module comment analysis
// =============================================================================

/// Analyzes module comments to infer capabilities, returning a list of inferred types.
fn inferFromModuleComments(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    caps: *std.ArrayList(InferredCapability),
) !void {
    // Find modules with comments that describe a capability.
    // Query for "module" node type with detail content.
    const results = db.searchWithAliases(allocator, "provides implements manages", 20, null) catch return;
    defer {
        for (results) |r| GuidanceDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    for (results) |r| {
        const comment = r.comment orelse continue;
        if (comment.len < 20) continue;
        if (!std.mem.eql(u8, r.node_type, "module")) continue;

        // Extract capability name from comment: first noun phrase after verb.
        const cap_name = extractCapabilityFromComment(comment) orelse continue;

        const name = try allocator.dupe(u8, cap_name);
        errdefer allocator.free(name);
        const description = try std.fmt.allocPrint(allocator, "Inferred from module comment: {s}", .{comment[0..@min(120, comment.len)]});
        errdefer allocator.free(description);

        var source_files = try allocator.alloc([]const u8, 1);
        source_files[0] = try allocator.dupe(u8, r.source);
        errdefer {
            allocator.free(source_files[0]);
            allocator.free(source_files);
        }

        try caps.append(allocator, .{
            .name = name,
            .description = description,
            .source_files = source_files,
            .confidence = 0.7,
            .method = .module_comment,
        });
    }
}

// =============================================================================
// Pass 4: File grouping by directory
// =============================================================================

/// Analyzes file grouping data to infer capabilities, returning a map of inferred capabilities.
fn inferFromFileGrouping(
    allocator: std.mem.Allocator,
    map: *const CodebaseMap,
    caps: *std.ArrayList(InferredCapability),
) !void {
    var dir_files: std.StringHashMapUnmanaged(std.ArrayList([]const u8)) = .{};
    defer {
        var it = dir_files.valueIterator();
        while (it.next()) |v| v.deinit(allocator);
        dir_files.deinit(allocator);
    }

    for (map.tree) |entry| {
        if (entry.kind != .file) continue;
        const ext = entry.extension orelse continue;
        if (!isSupportedExtension(ext)) continue;

        const dir = std.fs.path.dirname(entry.path) orelse continue;
        // Skip files directly under workspace root (no meaningful directory grouping).
        if (dir.len == 0 or std.mem.eql(u8, dir, ".")) continue;

        const gop = try dir_files.getOrPut(allocator, dir);
        if (!gop.found_existing) gop.value_ptr.* = .{};
        try gop.value_ptr.append(allocator, entry.path);
    }

    var it = dir_files.iterator();
    while (it.next()) |entry| {
        const dir = entry.key_ptr.*;
        const files = entry.value_ptr.*;
        if (files.items.len < 3) continue; // Need ≥ 3 files for directory capability.

        const dir_base = std.fs.path.basename(dir);
        if (dir_base.len < 2) continue;

        const name = try toCapabilityName(allocator, dir_base);
        errdefer allocator.free(name);

        var source_files = try allocator.alloc([]const u8, files.items.len);
        for (files.items, 0..) |f, i| source_files[i] = try allocator.dupe(u8, f);
        errdefer {
            for (source_files) |f| allocator.free(f);
            allocator.free(source_files);
        }

        const description = try std.fmt.allocPrint(allocator, "Capability inferred from directory '{s}' ({d} source files).", .{ dir, files.items.len });
        errdefer allocator.free(description);

        try caps.append(allocator, .{
            .name = name,
            .description = description,
            .source_files = source_files,
            .confidence = 0.5,
            .method = .file_grouping,
        });
    }
}

// =============================================================================
// Deduplication
// =============================================================================

/// Removes duplicate inferred capability entries from the list.
fn deduplicateCapabilities(
    allocator: std.mem.Allocator,
    caps: []InferredCapability,
) ![]InferredCapability {
    if (caps.len == 0) return allocator.alloc(InferredCapability, 0);

    var seen: std.StringHashMapUnmanaged(usize) = .{};
    defer seen.deinit(allocator);

    var out: std.ArrayList(InferredCapability) = .{};
    errdefer {
        for (out.items) |cap| freeInferredCapability(allocator, cap);
        out.deinit(allocator);
    }

    for (caps) |cap| {
        if (seen.get(cap.name)) |idx| {
            // Keep higher-confidence entry.
            if (cap.confidence > out.items[idx].confidence) {
                freeInferredCapability(allocator, out.items[idx]);
                out.items[idx] = cap;
            } else {
                freeInferredCapability(allocator, cap);
            }
        } else {
            try seen.put(allocator, cap.name, out.items.len);
            try out.append(allocator, cap);
        }
    }

    return out.toOwnedSlice(allocator);
}

// =============================================================================
// Helpers
// =============================================================================

/// Converts a raw memory slice to a human-readable capability name using the allocator.
fn toCapabilityName(allocator: std.mem.Allocator, raw: []const u8) ![]u8 {
    var out: std.ArrayList(u8) = .{};
    errdefer out.deinit(allocator);

    for (raw, 0..) |c, i| {
        if (c == '_' or c == '-') {
            if (out.items.len > 0) try out.append(allocator, '-');
        } else if (std.ascii.isUpper(c) and i > 0 and std.ascii.isLower(raw[i - 1])) {
            try out.append(allocator, '-');
            try out.append(allocator, std.ascii.toLower(c));
        } else {
            try out.append(allocator, std.ascii.toLower(c));
        }
    }

    return out.toOwnedSlice(allocator);
}

/// Checks if a given byte slice represents a supported Zig extension identifier.
fn isSupportedExtension(ext: []const u8) bool {
    const supported = [_][]const u8{ ".zig", ".py", ".rs", ".go", ".ts", ".js", ".c", ".cpp", ".h" };
    for (supported) |s| {
        if (std.mem.eql(u8, ext, s)) return true;
    }
    return false;
}

/// Extracts capability data from a Zig comment string and returns it as a slice.
fn extractCapabilityFromComment(comment: []const u8) ?[]const u8 {
    // Look for patterns like "— <noun phrase>" or "for <noun phrase>"
    const sep = "— ";
    if (std.mem.indexOf(u8, comment, sep)) |pos| {
        const after = std.mem.trim(u8, comment[pos + sep.len ..], " \t");
        const end = @min(after.len, 40);
        // Take first word or up to first punctuation.
        var word_end: usize = 0;
        while (word_end < end and
            std.ascii.isAlphanumeric(after[word_end]) or
            (word_end < end and (after[word_end] == '-' or after[word_end] == '_'))) : (word_end += 1)
        {}
        if (word_end >= 3) return after[0..word_end];
    }
    return null;
}

// =============================================================================
// Tests
// =============================================================================

test "toCapabilityName: snake_case to kebab-case" {
    const allocator = std.testing.allocator;
    const result = try toCapabilityName(allocator, "vector_search");
    defer allocator.free(result);
    try std.testing.expectEqualStrings("vector-search", result);
}

test "toCapabilityName: camelCase to kebab-case" {
    const allocator = std.testing.allocator;
    const result = try toCapabilityName(allocator, "vectorDb");
    defer allocator.free(result);
    try std.testing.expectEqualStrings("vector-db", result);
}

test "extractPrefix: underscore-separated" {
    try std.testing.expectEqualStrings("vector", extractPrefix("vector_db"));
    try std.testing.expectEqualStrings("ast", extractPrefix("ast_parser"));
}

test "extractPrefix: single word" {
    try std.testing.expectEqualStrings("guidance", extractPrefix("guidance"));
}

test "isSupportedExtension: known extensions" {
    try std.testing.expect(isSupportedExtension(".zig"));
    try std.testing.expect(isSupportedExtension(".py"));
    try std.testing.expect(!isSupportedExtension(".md"));
    try std.testing.expect(!isSupportedExtension(".json"));
}
