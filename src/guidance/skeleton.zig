//! skeleton.zig — File and struct skeleton extraction for token-efficient discovery.
//!
//! Implements the "File Skeleton" pattern from VISION.md:
//! - TIER 2: `guidance explain "filename.zig"` → show signatures + comments only
//! - TIER 3: `guidance explain "StructName"` → show struct members only
//!
//! Private members are omitted from skeleton views to reduce noise and token usage.

const std = @import("std");
const types = @import("types.zig");
const llm = @import("llm");
const json_store_mod = @import("sync/json_store.zig");
const comment_inserter = @import("comments/inserter.zig");

// Import string functions from common
const truncateAtSentence = @import("common").truncateAtSentence;
const firstCommentLine = @import("common").firstCommentLine;

/// Result of matching a query against known entities.
pub const TierMatch = union(enum) {
    /// Query matches a capability name (e.g., "vector-search")
    capability: []const u8,
    /// Query matches a file path (e.g., "src/guidance/query_engine.zig")
    file_path: []const u8,
    /// Query matches a struct/enum name (e.g., "GuidanceDb")
    struct_name: []const u8,
    /// Query matches multiple files by basename, needs disambiguation
    disambiguate: []const []const u8,
    /// No tier match, proceed to regular search
    none: void,
};

/// Frees the TierMatch union value (caller must free variants with owned data).
pub fn freeTierMatch(allocator: std.mem.Allocator, tier_match: TierMatch) void {
    switch (tier_match) {
        .capability => |cap| allocator.free(cap),
        .file_path => |fp| allocator.free(fp),
        .struct_name => |sn| allocator.free(sn),
        .disambiguate => |paths| {
            for (paths) |p| allocator.free(p);
            allocator.free(paths);
        },
        .none => {},
    }
}

/// Checks if a query matches a capability name, file path, struct name, or needs disambiguation.
/// Returns the match type and matched string (owned by caller, must free).
/// Priority: capability > file path/disambiguate > struct name.
pub fn classifyQuery(
    allocator: std.mem.Allocator,
    query: []const u8,
    capabilities_dir: []const u8,
    workspace: []const u8,
    guidance_dir: []const u8,
) !TierMatch {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len == 0) return .none;

    // TIER 1: Check for capability name match
    if (try isCapabilityName(allocator, trimmed, capabilities_dir)) |cap_name| {
        return .{ .capability = cap_name };
    }

    // TIER 2: Check for file path match (src/foo.zig) or basename disambiguation
    if (try resolveFilePath(allocator, trimmed, workspace, guidance_dir)) |result| {
        return result;
    }

    // TIER 3: Check for struct name match (via AST node lookup)
    if (try isStructNameMatch(allocator, trimmed, guidance_dir)) |struct_name| {
        return .{ .struct_name = struct_name };
    }

    return .none;
}

/// Checks if query matches a known capability name.
/// Returns owned capability name if found, null otherwise.
fn isCapabilityName(
    allocator: std.mem.Allocator,
    query: []const u8,
    capabilities_dir: []const u8,
) error{OutOfMemory}!?[]const u8 {
    // Normalize query: lowercase, replace spaces/hyphens with dashes
    const normalized = try normalizeCapabilityName(allocator, query);
    defer allocator.free(normalized);

    // Check if CAPABILITY.md exists for this name
    const cap_path = try std.fs.path.join(allocator, &.{ capabilities_dir, normalized, "CAPABILITY.md" });
    defer allocator.free(cap_path);

    std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), cap_path, .{}) catch return null;
    return try allocator.dupe(u8, normalized);
}

/// Normalizes a string for capability matching: lowercase, spaces/hyphens/underscores → single dash.
fn normalizeCapabilityName(allocator: std.mem.Allocator, input: []const u8) error{OutOfMemory}![]const u8 {
    var buf: std.ArrayList(u8) = .empty;
    errdefer buf.deinit(allocator);
    var prev_was_dash = false;
    for (input) |c| {
        if (c == ' ' or c == '-' or c == '_') {
            if (!prev_was_dash) {
                try buf.append(allocator, '-');
                prev_was_dash = true;
            }
        } else if (std.ascii.isAlphabetic(c) or std.ascii.isDigit(c)) {
            try buf.append(allocator, std.ascii.toLower(c));
            prev_was_dash = false;
        }
    }
    // Trim trailing dashes
    while (buf.items.len > 0 and buf.items[buf.items.len - 1] == '-') {
        buf.items.len -= 1;
    }
    return try buf.toOwnedSlice(allocator);
}

/// Resolves a file path query to either a unique match or disambiguation.
/// For full paths (e.g., "src/foo.zig"): returns file_path if exists.
/// For basenames (e.g., "foo.zig"): searches guidance JSON files for matches.
/// Returns null if query doesn't look like a file path (no extension).
fn resolveFilePath(
    allocator: std.mem.Allocator,
    query: []const u8,
    workspace: []const u8,
    guidance_dir: []const u8,
) error{OutOfMemory}!?TierMatch {
    const source_extensions = [_][]const u8{ ".zig", ".py", ".rs", ".go", ".ts", ".js", ".md" };
    const has_path_sep = std.mem.indexOfAny(u8, query, "/\\") != null;
    const has_ext = blk: {
        for (source_extensions) |ext| {
            if (std.mem.endsWith(u8, query, ext)) break :blk true;
        }
        break :blk false;
    };
    if (!has_path_sep and !has_ext) return null;

    // Full path: try to find exact match
    if (has_path_sep) {
        if (try checkFileExists(allocator, query, workspace, guidance_dir)) |path| {
            return .{ .file_path = path };
        }
        return null;
    }

    // Basename (no path separator): search for matches
    const matches = try findFilesByBasename(allocator, query, guidance_dir) orelse return null;

    if (matches.len == 1) {
        // Unique match: return as file_path
        const path = matches[0];
        allocator.free(matches);
        return .{ .file_path = path };
    }

    // Multiple matches: return for disambiguation
    return .{ .disambiguate = matches };
}

/// Finds all files matching a basename in the guidance JSON directory.
/// Returns owned array of owned paths relative to repo root (e.g., "src/foo/bar.zig").
fn findFilesByBasename(
    allocator: std.mem.Allocator,
    basename: []const u8,
    guidance_dir: []const u8,
) error{OutOfMemory}!?[]const []const u8 {
    const json_dir = std.fs.path.join(allocator, &.{ guidance_dir, "src" }) catch return null;
    defer allocator.free(json_dir);

    var dir = std.Io.Dir.cwd().openDir(std.Io.Threaded.global_single_threaded.io(), json_dir, .{ .iterate = true }) catch return null;
    defer dir.close(std.Io.Threaded.global_single_threaded.io());

    var walker = dir.walk(allocator) catch return null;
    defer walker.deinit();

    var matches: std.ArrayList([]const u8) = .empty;
    defer {
        for (matches.items) |m| allocator.free(m);
        matches.deinit(allocator);
    }

    while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.basename, ".json")) continue;

        // Strip .json extension to get source filename
        const base_no_ext = entry.basename[0 .. entry.basename.len - ".json".len];
        if (!std.mem.eql(u8, base_no_ext, basename)) continue;

        // Convert relative path from walker to source path
        // entry.path = "llm/root.zig.json" (relative to .guidance/src/)
        // Convert to: "src/llm/root.zig"
        const src_path = blk: {
            // Remove .json suffix
            const without_json = entry.path[0 .. entry.path.len - ".json".len];
            break :blk std.fmt.allocPrint(allocator, "src/{s}", .{without_json}) catch null;
        };

        if (src_path) |sp| {
            try matches.append(allocator, sp);
        }
    }

    if (matches.items.len == 0) return null;

    // Transfer ownership to caller
    return try matches.toOwnedSlice(allocator);
}

/// Checks if a file exists using the provided allocator, workspace, and guidance direction.
fn checkFileExists(
    allocator: std.mem.Allocator,
    rel_path: []const u8,
    workspace: []const u8,
    guidance_dir: []const u8,
) error{OutOfMemory}!?[]const u8 {
    // Check source file
    const src_abs = std.fs.path.join(allocator, &.{ workspace, rel_path }) catch return null;
    defer allocator.free(src_abs);
    std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), src_abs, .{}) catch {
        // Check JSON file as fallback
        const json_path = srcToGuidanceJson(allocator, rel_path, guidance_dir) catch return null;
        defer allocator.free(json_path);
        std.Io.Dir.accessAbsolute(std.Io.Threaded.global_single_threaded.io(), json_path, .{}) catch return null;
        return try allocator.dupe(u8, rel_path);
    };
    return try allocator.dupe(u8, rel_path);
}

/// Converts a Zig source path to a guidance JSON representation, handling memory allocation and returning an error if out of memory.
fn srcToGuidanceJson(allocator: std.mem.Allocator, src_path: []const u8, guidance_dir: []const u8) error{OutOfMemory}![]const u8 {
    // src/foo/bar.zig → guidance_dir/src/foo/bar.zig.json
    // Remove leading "src/" if present to avoid duplication
    const path = if (std.mem.startsWith(u8, src_path, "src/"))
        src_path["src/".len..]
    else
        src_path;
    return try std.fmt.allocPrint(allocator, "{s}/src/{s}.json", .{ guidance_dir, path });
}

/// Converts a JSON path array into a source path slice, returning the corresponding array of bytes.
fn jsonPathToSrcPath(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    // .guidance/src/foo/bar.zig.json → src/foo/bar.zig
    const prefix = ".guidance/src/";
    if (!std.mem.startsWith(u8, json_path, prefix)) return null;
    const rest = json_path[prefix.len..];
    if (!std.mem.endsWith(u8, rest, ".json")) return null;
    return allocator.dupe(u8, rest[0 .. rest.len - ".json".len]) catch null;
}

/// Checks if query matches a known struct/enum name via AST node lookup.
fn isStructNameMatch(
    allocator: std.mem.Allocator,
    query: []const u8,
    guidance_dir: []const u8,
) error{OutOfMemory}!?[]const u8 {
    // Single-identifier check
    if (!looksLikeIdentifier(query)) return null;

    // Scan JSON files for matching struct/enum name
    const json_dir = try std.fs.path.join(allocator, &.{ guidance_dir, "src" });
    defer allocator.free(json_dir);

    var dir = std.Io.Dir.cwd().openDir(std.Io.Threaded.global_single_threaded.io(), json_dir, .{ .iterate = true }) catch return null;
    defer dir.close(std.Io.Threaded.global_single_threaded.io());

    var walker = try dir.walk(allocator);
    defer walker.deinit();

    while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.basename, ".json")) continue;

        if (try findStructNameInJson(allocator, query, json_dir, entry.path)) |name| {
            return name;
        }
    }

    return null;
}

/// Search for a struct name within a JSON path, returning its slice or an error if memory is exhausted.
fn findStructNameInJson(
    allocator: std.mem.Allocator,
    query: []const u8,
    json_dir: []const u8,
    rel_path: []const u8,
) error{OutOfMemory}!?[]const u8 {
    const full_path = std.fs.path.join(allocator, &.{ json_dir, rel_path }) catch return null;
    defer allocator.free(full_path);

    const io = std.Io.Threaded.global_single_threaded.io();
    const content = std.Io.Dir.cwd().readFileAlloc(io, full_path, allocator, .limited(2 * 1024 * 1024)) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    if (parsed.value != .object) return null;
    const members = parsed.value.object.get("members") orelse return null;
    if (members != .array) return null;

    for (members.array.items) |m| {
        if (m != .object) continue;
        const type_val = m.object.get("type") orelse continue;
        if (type_val != .string) continue;
        const node_type = type_val.string;

        // Match struct or enum
        if (!std.mem.eql(u8, node_type, "struct") and !std.mem.eql(u8, node_type, "enum")) continue;

        const name_val = m.object.get("name") orelse continue;
        if (name_val != .string) continue;

        if (std.ascii.eqlIgnoreCase(name_val.string, query)) {
            return try allocator.dupe(u8, name_val.string);
        }
    }

    return null;
}

/// Checks if a given byte slice matches the pattern of a valid identifier in Zig.
fn looksLikeIdentifier(query: []const u8) bool {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len < 2 or trimmed.len > 64) return false;
    if (std.mem.indexOfAny(u8, trimmed, " \t\n\r/") != null) return false;
    if (!std.ascii.isAlphabetic(trimmed[0]) and trimmed[0] != '_') return false;
    for (trimmed) |c| {
        if (!std.ascii.isAlphanumeric(c) and c != '_') return false;
    }
    return true;
}

/// Generates a file skeleton: signatures + doc comments, omitting private members.
pub fn generateFileSkeleton(
    allocator: std.mem.Allocator,
    src_path: []const u8,
    workspace: []const u8,
    guidance_dir: []const u8,
) ?[]const u8 {
    // Try to load JSON metadata first
    const json_path = srcToGuidanceJson(allocator, src_path, guidance_dir) catch return null;
    defer allocator.free(json_path);

    const io = std.Io.Threaded.global_single_threaded.io();
    const json_content = std.Io.Dir.cwd().readFileAlloc(io, json_path, allocator, .limited(2 * 1024 * 1024)) catch return null;
    defer allocator.free(json_content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, json_content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    if (parsed.value != .object) return null;
    const root = parsed.value.object;

    // Get source path from meta for comment extraction
    const meta = root.get("meta") orelse return null;
    if (meta != .object) return null;
    const source_path = meta.object.get("source") orelse return null;
    if (source_path != .string) return null;

    // Load source file for comment extraction
    const abs_src_path = std.fs.path.join(allocator, &.{ workspace, source_path.string }) catch return null;
    defer allocator.free(abs_src_path);
    const source_content = std.Io.Dir.cwd().readFileAlloc(io, abs_src_path, allocator, .limited(10 * 1024 * 1024)) catch null;
    defer if (source_content) |sc| allocator.free(sc);

    var output_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer output_aw.deinit();
    const w = &output_aw.writer;

    // Module comment (from JSON for now, source extraction would be similar)
    if (root.get("comment")) |c| {
        if (c == .string and c.string.len > 0) {
            w.print("//! {s}\n\n", .{c.string}) catch return null;
        }
    }

    // Public members only
    const members = root.get("members") orelse return null;
    if (members != .array) return null;

    var seen: std.StringHashMapUnmanaged(void) = .empty;
    defer seen.deinit(allocator);

    for (members.array.items) |m| {
        if (m != .object) continue;

        // Skip private members
        const is_pub = blk: {
            const pv = m.object.get("is_pub") orelse break :blk false;
            break :blk pv == .bool and pv.bool;
        };
        if (!is_pub) continue;

        // Skip test declarations
        const type_val = m.object.get("type") orelse continue;
        if (type_val != .string) continue;
        if (std.mem.eql(u8, type_val.string, "test_decl")) continue;

        const name = blk: {
            const nv = m.object.get("name") orelse continue;
            if (nv != .string) continue;
            break :blk nv.string;
        };

        // Skip duplicates
        if (seen.contains(name)) continue;
        seen.put(allocator, name, {}) catch return null;

        // Get line number for comment extraction
        const line_num = blk: {
            const lv = m.object.get("line") orelse break :blk null;
            if (lv != .integer) break :blk null;
            break :blk @as(?u32, @intCast(lv.integer));
        };

        // Extract comment from source if available
        var extracted_comment: ?[]const u8 = null;
        defer if (extracted_comment) |ec| allocator.free(ec);
        if (source_content) |src| {
            if (line_num) |line| {
                extracted_comment = comment_inserter.extractCommentAtLine(allocator, src, line) catch null;
            }
        }

        // Format based on type
        if (std.mem.eql(u8, type_val.string, "fn_decl") or std.mem.eql(u8, type_val.string, "fn_private")) {
            // Include first line of comment if present
            if (extracted_comment) |comment| {
                const first_line = firstCommentLine(comment);
                if (first_line.len > 0) {
                    w.print("/// {s}\n", .{first_line}) catch return null;
                }
            }
            if (m.object.get("signature")) |sig| {
                if (sig == .string) {
                    w.print("pub {s}\n", .{sig.string}) catch return null;
                }
            }
        } else if (std.mem.eql(u8, type_val.string, "struct")) {
            // Include struct comment if present
            if (extracted_comment) |comment| {
                const first_line = firstCommentLine(comment);
                if (first_line.len > 0) {
                    w.print("/// {s}\n", .{first_line}) catch return null;
                }
            }
            w.print("\npub const {s} = struct {{\n", .{name}) catch return null;
            // Nested members (comments from JSON if available)
            if (m.object.get("members")) |nested| {
                if (nested == .array) {
                    for (nested.array.items) |nm| {
                        if (nm != .object) continue;
                        const n_is_pub = blk: {
                            const pv = nm.object.get("is_pub") orelse break :blk false;
                            break :blk pv == .bool and pv.bool;
                        };
                        if (!n_is_pub) continue;
                        const n_name = blk: {
                            const nv = nm.object.get("name") orelse continue;
                            if (nv != .string) continue;
                            break :blk nv.string;
                        };
                        const n_type = blk: {
                            const tv = nm.object.get("type") orelse continue;
                            if (tv != .string) continue;
                            break :blk tv.string;
                        };
                        // Nested members don't have line numbers in JSON, skip comment
                        if (std.mem.eql(u8, n_type, "fn_decl") or std.mem.eql(u8, n_type, "method")) {
                            if (nm.object.get("signature")) |sig| {
                                if (sig == .string) {
                                    w.print("    pub {s}\n", .{sig.string}) catch return null;
                                } else {
                                    w.print("    pub fn {s}(...) {{ ... }}\n", .{n_name}) catch return null;
                                }
                            } else {
                                w.print("    pub fn {s}(...) {{ ... }}\n", .{n_name}) catch return null;
                            }
                        } else {
                            w.print("    {s}: {s}\n", .{ n_name, "..." }) catch return null;
                        }
                    }
                }
            }
            w.print("}};\n", .{}) catch return null;
        } else if (std.mem.eql(u8, type_val.string, "enum")) {
            // Include enum comment if present
            if (extracted_comment) |comment| {
                const first_line = firstCommentLine(comment);
                if (first_line.len > 0) {
                    w.print("/// {s}\n", .{first_line}) catch return null;
                }
            }
            w.print("\npub const {s} = enum {{\n", .{name}) catch return null;
            if (m.object.get("members")) |nested| {
                if (nested == .array) {
                    for (nested.array.items) |nm| {
                        if (nm != .object) continue;
                        const n_name = blk: {
                            const nv = nm.object.get("name") orelse continue;
                            if (nv != .string) continue;
                            break :blk nv.string;
                        };
                        w.print("    {s},\n", .{n_name}) catch return null;
                    }
                }
            }
            w.print("}};\n", .{}) catch return null;
        }
    }

    if (output_aw.written().len == 0) return null;
    return output_aw.toOwnedSlice() catch null;
}

/// Generates a struct skeleton: members with public visibility only.
pub fn generateStructSkeleton(
    allocator: std.mem.Allocator,
    struct_name: []const u8,
    guidance_dir: []const u8,
) ?[]const u8 {
    const json_dir = std.fs.path.join(allocator, &.{ guidance_dir, "src" }) catch return null;
    defer allocator.free(json_dir);

    var dir = std.Io.Dir.cwd().openDir(std.Io.Threaded.global_single_threaded.io(), json_dir, .{ .iterate = true }) catch return null;
    defer dir.close(std.Io.Threaded.global_single_threaded.io());

    var walker = dir.walk(allocator) catch return null;
    defer walker.deinit();

    while (walker.next(std.Io.Threaded.global_single_threaded.io()) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.basename, ".json")) continue;

        const result = findAndGenerateStructSkeleton(allocator, struct_name, json_dir, entry.path);
        if (result) |s| return s;
    }

    return null;
}

/// Determines and generates a skeleton structure for a given Zig file path using an allocator and path components.
fn findAndGenerateStructSkeleton(
    allocator: std.mem.Allocator,
    struct_name: []const u8,
    json_dir: []const u8,
    rel_path: []const u8,
) ?[]const u8 {
    const full_path = std.fs.path.join(allocator, &.{ json_dir, rel_path }) catch return null;
    defer allocator.free(full_path);

    const io = std.Io.Threaded.global_single_threaded.io();
    const content = std.Io.Dir.cwd().readFileAlloc(io, full_path, allocator, .limited(2 * 1024 * 1024)) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    if (parsed.value != .object) return null;
    const root = parsed.value.object;
    const members = root.get("members") orelse return null;
    if (members != .array) return null;

    // Get source path for comment extraction
    const meta = root.get("meta");
    const source_path: ?[]const u8 = if (meta) |m| blk: {
        if (m != .object) break :blk null;
        const src = m.object.get("source") orelse break :blk null;
        if (src != .string) break :blk null;
        break :blk src.string;
    } else null;

    // Load source file for comment extraction
    var source_content: ?[]const u8 = null;
    defer if (source_content) |sc| allocator.free(sc);
    if (source_path) |sp| blk: {
        const cwd = std.process.currentPathAlloc(std.Io.Threaded.global_single_threaded.io(), allocator) catch break :blk;
        defer allocator.free(cwd);
        const abs_src = std.fs.path.join(allocator, &.{ cwd, sp }) catch break :blk;
        defer allocator.free(abs_src);
        source_content = std.Io.Dir.cwd().readFileAlloc(std.Io.Threaded.global_single_threaded.io(), abs_src, allocator, .limited(10 * 1024 * 1024)) catch break :blk;
    }

    for (members.array.items) |m| {
        if (m != .object) continue;

        const type_val = m.object.get("type") orelse continue;
        if (type_val != .string) continue;
        if (!std.mem.eql(u8, type_val.string, "struct")) continue;

        const name_val = m.object.get("name") orelse continue;
        if (name_val != .string) continue;

        if (std.ascii.eqlIgnoreCase(name_val.string, struct_name)) {
            return generateStructSkeletonFromJson(allocator, m.object, name_val.string, source_content);
        }
    }

    return null;
}

/// Converts a JSON object into a Zig struct skeleton slice, handling allocations and structure mapping.
fn generateStructSkeletonFromJson(
    allocator: std.mem.Allocator,
    obj: std.json.ObjectMap,
    struct_name: []const u8,
    source_content: ?[]const u8,
) ?[]const u8 {
    var output_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer output_aw.deinit();
    const w = &output_aw.writer;

    // Collect members with comments
    const members = obj.get("members") orelse return null;
    if (members != .array) return null;

    const FnMember = struct {
        name: []const u8,
        signature: []const u8,
        comment: ?[]const u8,
    };
    var fn_members: std.ArrayList(FnMember) = .empty;
    defer {
        for (fn_members.items) |item| {
            allocator.free(item.name);
            allocator.free(item.signature);
            if (item.comment) |c| allocator.free(c);
        }
        fn_members.deinit(allocator);
    }

    for (members.array.items) |nm| {
        if (nm != .object) continue;

        const is_pub = blk: {
            const pv = nm.object.get("is_pub") orelse break :blk false;
            break :blk pv == .bool and pv.bool;
        };
        if (!is_pub) continue;

        const n_name = blk: {
            const nv = nm.object.get("name") orelse continue;
            if (nv != .string) continue;
            break :blk nv.string;
        };

        const n_type = blk: {
            const tv = nm.object.get("type") orelse continue;
            if (tv != .string) continue;
            break :blk tv.string;
        };

        const n_line = blk: {
            const lv = nm.object.get("line") orelse break :blk null;
            if (lv != .integer) break :blk null;
            break :blk @as(?u32, @intCast(lv.integer));
        };

        // Only include functions
        if (!std.mem.eql(u8, n_type, "fn_decl") and !std.mem.eql(u8, n_type, "method") and !std.mem.eql(u8, n_type, "fn_private")) continue;

        const n_sig = nm.object.get("signature") orelse continue;
        if (n_sig != .string) continue;

        // Extract comment from source file
        var comment: ?[]const u8 = null;
        if (source_content) |src| {
            if (n_line) |line| {
                const comment_result = comment_inserter.extractCommentAtLine(allocator, src, line) catch null;
                if (comment_result) |c| {
                    comment = c;
                }
            }
        }

        fn_members.append(allocator, .{
            .name = allocator.dupe(u8, n_name) catch return null,
            .signature = allocator.dupe(u8, n_sig.string) catch return null,
            .comment = comment,
        }) catch return null;
    }

    // Output: Description section with struct comment from JSON
    if (obj.get("comment")) |comment_val| {
        if (comment_val == .string and comment_val.string.len > 0) {
            w.print("### Description\n\n{s}\n\n", .{comment_val.string}) catch return null;
        }
    }

    // Check if any functions have comments
    var has_comments = false;
    for (fn_members.items) |item| {
        if (item.comment != null) {
            has_comments = true;
            break;
        }
    }

    // Output: Documentation section with function comments (above Interface)
    if (has_comments) {
        w.print("### Documentation\n\n", .{}) catch return null;
        for (fn_members.items) |item| {
            if (item.comment) |c| {
                w.print("**{s}**: {s}\n\n", .{ item.name, c }) catch return null;
            }
        }
    }

    // Output: Interface section - just the function signatures
    w.print("### Interface\n\n", .{}) catch return null;
    w.print("`{s}` has {d} public functions:\n\n", .{ struct_name, fn_members.items.len }) catch return null;

    // List functions with signatures only
    for (fn_members.items) |item| {
        w.print("- **{s}** — `{s}`\n", .{ item.name, item.signature }) catch return null;
    }

    // Output: Explore section with suggested queries
    if (fn_members.items.len > 0) {
        w.print("\n### Explore\n\n", .{}) catch return null;
        var count: usize = 0;
        for (fn_members.items) |item| {
            if (count >= 3) break;
            w.print("- `guidance explain \"{s}.{s}\"`\n", .{ struct_name, item.name }) catch return null;
            count += 1;
        }
    }

    return output_aw.toOwnedSlice() catch null;
}

/// Renders a capability document for display, with optional LLM summarization.
pub fn renderCapabilityDocument(
    allocator: std.mem.Allocator,
    cap_name: []const u8,
    capabilities_dir: []const u8,
    llm_client: ?*llm.LlmClient,
    natural_lang_query: bool,
) error{OutOfMemory}!?[]const u8 {
    const cap_path = try std.fs.path.join(allocator, &.{ capabilities_dir, cap_name, "CAPABILITY.md" });
    defer allocator.free(cap_path);

    const content = std.Io.Dir.cwd().readFileAlloc(allocator, cap_path, 256 * 1024) catch return null;
    defer allocator.free(content);

    // Parse frontmatter to get description
    const desc = parseCapabilityDescription(allocator, content) catch null;

    // If no natural language, return full content
    if (!natural_lang_query or llm_client == null) {
        // Return content after frontmatter
        if (std.mem.indexOf(u8, content, "\n---\n")) |end_fm| {
            return try allocator.dupe(u8, content[end_fm + 5 ..]);
        }
        return try allocator.dupe(u8, content);
    }

    // Summarize for natural language queries
    const client = llm_client.?;
    const prompt = try std.fmt.allocPrint(allocator,
        \\Summarize this capability for a code search query. Be concise (under 150 words).
        \\Focus on: what it does, key entry points, and how to use it.
        \\Omit any blocks irrelevant to the query. Include file:line citations where helpful.
        \\
        \\Capability: {s}
        \\Description: {s}
        \\
        \\Content:
        \\{s}
        \\
        \\Return only the summary.
    , .{ cap_name, desc orelse "", content });
    defer allocator.free(prompt);

    const response = client.complete(prompt, 500, 0.1, null) catch {
        // Fallback to raw content
        if (std.mem.indexOf(u8, content, "\n---\n")) |end_fm| {
            return try allocator.dupe(u8, content[end_fm + 5 ..]);
        }
        return try allocator.dupe(u8, content);
    };
    defer if (response) |r| allocator.free(r);

    const raw = response orelse fallback: {
        if (std.mem.indexOf(u8, content, "\n---\n")) |end_fm| {
            return try allocator.dupe(u8, content[end_fm + 5 ..]);
        }
        break :fallback content;
    };

    const stripped = llm.stripThinkBlock(raw);
    return try allocator.dupe(u8, std.mem.trim(u8, stripped, " \t\n\r"));
}

/// Parses the description field from YAML frontmatter.
fn parseCapabilityDescription(allocator: std.mem.Allocator, content: []const u8) error{OutOfMemory}!?[]const u8 {
    if (!std.mem.startsWith(u8, content, "---")) return null;

    const end_fm = std.mem.indexOf(u8, content[3..], "---") orelse return null;
    const frontmatter = content[3 .. 3 + end_fm];

    var lines = std.mem.splitScalar(u8, frontmatter, '\n');
    while (lines.next()) |line| {
        if (std.mem.startsWith(u8, line, "description:")) {
            const desc = std.mem.trim(u8, line["description:".len..], " \t\r");
            return try allocator.dupe(u8, desc);
        }
    }
    return null;
}

/// Output configuration for skeleton formatting.
pub const SkeletonOutputConfig = struct {
    title: []const u8,
    source_path: []const u8,
    start_line: ?u32 = null,
    end_line: ?u32 = null,
};

/// Formats skeleton output with consistent structure for file and struct skeletons.
/// Returns owned slice that caller must free.
pub fn formatSkeletonOutput(
    allocator: std.mem.Allocator,
    skeleton: []const u8,
    config: SkeletonOutputConfig,
) ![]const u8 {
    var output_aw: std.Io.Writer.Allocating = .init(allocator);
    errdefer output_aw.deinit();
    const w = &output_aw.writer;

    const trimmed = std.mem.trim(u8, skeleton, " \t\n\r");
    const trimmed_title = std.mem.trim(u8, config.title, " \t\n\r");

    try w.print("# {s}\n\n", .{trimmed_title});

    if (config.start_line) |start| {
        const end = config.end_line orelse start;
        try w.print("## Source location: `{s}:{d}-{d}`\n\n", .{ config.source_path, start, end });
    }

    if (trimmed.len > 0) {
        try w.print("{s}", .{trimmed});
        if (!std.mem.endsWith(u8, trimmed, "\n")) {
            try w.writeByte('\n');
        }
    }

    try w.writeAll("\n---\n\nRun `guidance explain \"<function-name>\"` to see specific function documentation.\n");

    return output_aw.toOwnedSlice();
}
