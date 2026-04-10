//! orphan.zig — Phase 0: Orphaned source file detection for `guidance codehealth`.
//!
//! An "orphaned" file is a .zig source file that:
//!   - Is not imported by any other file via @import
//!   - Is not a recognised entry point
//!
//! Entry points (always excluded from orphan detection):
//!   - Files containing `pub fn main` (CLI executables)
//!   - Files referenced as root_source_file in build.zig
//!   - Test files: names matching `*_test.zig` or `*_tests.zig`
//!   - Files containing the comment `// CODEHEALTH: entry-point`

const std = @import("std");

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A source file that is not imported by any other file and is not an entry point.
pub const OrphanedFile = struct {
    /// Workspace-relative path, e.g. "src/guidance/vector_db.zig".
    source: []const u8,
    last_modified: i64,
};

/// The set of files referenced as root_source_file in build.zig.
/// Parsed from the literal strings passed to `b.path(...)`.
pub const BuildZigRoots = struct {
    paths: []const []const u8,

    pub fn contains(self: BuildZigRoots, path: []const u8) bool {
        for (self.paths) |p| {
            if (std.mem.eql(u8, p, path)) return true;
        }
        return false;
    }
};

// ---------------------------------------------------------------------------
// Build.zig root extraction
// ---------------------------------------------------------------------------

/// Parse build.zig source text and return all paths passed to `b.path("...")`.
/// The caller owns the returned slice and each contained string.
pub fn extractBuildZigRoots(allocator: std.mem.Allocator, build_zig_src: []const u8) ![][]const u8 {
    var roots: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (roots.items) |r| allocator.free(r);
        roots.deinit(allocator);
    }

    // Simple text scan: find occurrences of `.path("` and extract the quoted string.
    // This is deliberately text-based rather than AST-based to avoid depending on
    // build.zig having a parseable AST under all conditions.
    var pos: usize = 0;
    while (pos < build_zig_src.len) {
        const needle = ".path(\"";
        const found = std.mem.indexOfPos(u8, build_zig_src, pos, needle) orelse break;
        const start = found + needle.len;
        const end = std.mem.indexOfScalarPos(u8, build_zig_src, start, '"') orelse break;
        const raw = build_zig_src[start..end];
        if (raw.len > 0 and !std.mem.startsWith(u8, raw, "//")) {
            try roots.append(allocator, try allocator.dupe(u8, raw));
        }
        pos = end + 1;
    }

    return roots.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Import extraction (single file)
// ---------------------------------------------------------------------------

/// Extract all literal string arguments to @import() calls in `source`.
/// Returns workspace-relative paths where possible; paths starting with "./"
/// or "../" need resolution relative to `file_dir`.
/// Caller owns the returned slice and each string.
pub fn extractImportPaths(
    allocator: std.mem.Allocator,
    source: [:0]const u8,
) ![][]const u8 {
    var paths: std.ArrayList([]const u8) = .empty;
    errdefer {
        for (paths.items) |p| allocator.free(p);
        paths.deinit(allocator);
    }

    var tree = std.zig.Ast.parse(allocator, source, .zig) catch return paths.toOwnedSlice(allocator);
    defer tree.deinit(allocator);

    // Walk all nodes looking for @import builtin calls.
    for (0..tree.nodes.len) |i| {
        const node: std.zig.Ast.Node.Index = @enumFromInt(i);
        const tag = tree.nodeTag(node);
        // @import appears as builtin_call_two (one argument).
        if (tag != .builtin_call_two and tag != .builtin_call_two_comma and
            tag != .builtin_call and tag != .builtin_call_comma) continue;

        const main_tok = tree.nodeMainToken(node);
        const builtin_name = tree.tokenSlice(main_tok);
        if (!std.mem.eql(u8, builtin_name, "@import")) continue;

        // Extract the string argument (first opt node for builtin_call_two).
        const arg_node = switch (tag) {
            .builtin_call_two, .builtin_call_two_comma => blk: {
                const opts = tree.nodeData(node).opt_node_and_opt_node;
                break :blk opts[0].unwrap() orelse continue;
            },
            .builtin_call, .builtin_call_comma => blk: {
                // Extra args stored as a span; first arg is at data.node_and_token[0].
                const d = tree.nodeData(node).node_and_token;
                break :blk d[0];
            },
            else => continue,
        };

        if (tree.nodeTag(arg_node) != .string_literal) continue;
        const str_tok = tree.nodeMainToken(arg_node);
        const raw = tree.tokenSlice(str_tok);
        if (raw.len < 2) continue;
        const path = raw[1 .. raw.len - 1]; // strip quotes

        // Skip non-relative imports (e.g. "std", "builtin", module names)
        // We only care about file paths — they contain "/" or end in ".zig".
        if (!std.mem.endsWith(u8, path, ".zig") and !std.mem.containsAtLeast(u8, path, 1, "/")) continue;

        try paths.append(allocator, try allocator.dupe(u8, path));
    }

    return paths.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Entry-point detection
// ---------------------------------------------------------------------------

/// Returns true if the source text contains `pub fn main`.
pub fn hasPubFnMain(source: []const u8) bool {
    return std.mem.indexOf(u8, source, "pub fn main") != null;
}

/// Returns true if the source text contains the `// CODEHEALTH: entry-point` marker.
pub fn hasEntryPointMarker(source: []const u8) bool {
    return std.mem.indexOf(u8, source, "// CODEHEALTH: entry-point") != null;
}

/// Returns true if the filename matches the test-file convention:
/// `*_test.zig`, `*_tests.zig`, or `tests.zig`.
pub fn isTestFileName(basename: []const u8) bool {
    if (std.mem.eql(u8, basename, "tests.zig")) return true;
    if (std.mem.endsWith(u8, basename, "_test.zig")) return true;
    if (std.mem.endsWith(u8, basename, "_tests.zig")) return true;
    return false;
}

// ---------------------------------------------------------------------------
// Orphan detection
// ---------------------------------------------------------------------------

/// Walk all .zig source files under `workspace` and return those that are not
/// imported by any other file and are not entry points.
///
/// Algorithm:
///   1. Discover all .zig files in workspace.
///   2. For each file, extract @import paths and resolve them to workspace-relative
///      canonical paths.
///   3. Build the "imported" set.
///   4. Files not in the imported set, not in build.zig roots, and not entry points
///      are orphaned.
///
/// Caller owns the returned slice and each OrphanedFile's `source` string.
pub fn findOrphanedFiles(
    allocator: std.mem.Allocator,
    workspace: []const u8,
) ![]OrphanedFile {
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    // ── 1. Discover all .zig files ────────────────────────────────────────────
    var all_files: std.ArrayList([]const u8) = .empty;
    defer all_files.deinit(aa);

    // Map: workspace-relative path → stat.mtime
    var mtime_map = std.StringHashMap(i64).init(aa);
    defer mtime_map.deinit();

    {
        var base_dir = std.fs.cwd().openDir(workspace, .{ .iterate = true }) catch |err| {
            std.debug.print("[orphan] cannot open workspace '{s}': {s}\n", .{ workspace, @errorName(err) });
            return allocator.alloc(OrphanedFile, 0);
        };
        defer base_dir.close();

        var walker = try base_dir.walk(aa);
        defer walker.deinit();

        while (try walker.next()) |entry| {
            if (entry.kind != .file) continue;
            if (!std.mem.endsWith(u8, entry.path, ".zig")) continue;
            // Exclude generated/vendor directories.
            if (std.mem.startsWith(u8, entry.path, "zig-cache/")) continue;
            if (std.mem.startsWith(u8, entry.path, "zig-out/")) continue;

            const rel_path = try aa.dupe(u8, entry.path);
            try all_files.append(aa, rel_path);

            // Record mtime (best effort).
            const stat = base_dir.statFile(rel_path) catch continue;
            try mtime_map.put(rel_path, @intCast(@divTrunc(stat.mtime, std.time.ns_per_s)));
        }
    }

    // ── 2. Load and parse build.zig roots ────────────────────────────────────
    var build_roots_paths: [][]const u8 = &.{};
    const build_zig_src = std.fs.cwd().readFileAlloc(
        aa,
        try std.fmt.allocPrint(aa, "{s}/build.zig", .{workspace}),
        5 * 1024 * 1024,
    ) catch null;
    if (build_zig_src) |src| {
        build_roots_paths = extractBuildZigRoots(aa, src) catch &.{};
    }
    const build_roots = BuildZigRoots{ .paths = build_roots_paths };

    // ── 3. Build the "imported" set ───────────────────────────────────────────
    // Key: workspace-relative canonical path.
    var imported: std.StringHashMap(void) = std.StringHashMap(void).init(aa);
    defer imported.deinit();

    for (all_files.items) |rel_path| {
        const abs_path = try std.fmt.allocPrint(aa, "{s}/{s}", .{ workspace, rel_path });
        const src_raw = std.fs.cwd().readFileAlloc(aa, abs_path, 5 * 1024 * 1024) catch continue;
        const src_z = try aa.dupeZ(u8, src_raw);

        const dir_of_file = std.fs.path.dirname(rel_path) orelse "";

        const import_paths = extractImportPaths(aa, src_z) catch continue;
        for (import_paths) |imp| {
            // Resolve relative to the importing file's directory.
            const resolved = resolveImportPath(aa, workspace, dir_of_file, imp) catch continue;
            try imported.put(resolved, {});
        }
    }

    // ── 4. Identify orphans ────────────────────────────────────────────────────
    var orphans: std.ArrayList(OrphanedFile) = .empty;
    errdefer {
        for (orphans.items) |o| allocator.free(o.source);
        orphans.deinit(allocator);
    }

    for (all_files.items) |rel_path| {
        // Skip if imported by someone.
        if (imported.contains(rel_path)) continue;

        // Skip build.zig itself (it's never imported).
        if (std.mem.eql(u8, rel_path, "build.zig")) continue;
        if (std.mem.eql(u8, rel_path, "build.zig.zon")) continue;

        // Skip if in build.zig root_source_file list.
        if (build_roots.contains(rel_path)) continue;

        // Skip test file names.
        const basename = std.fs.path.basename(rel_path);
        if (isTestFileName(basename)) continue;

        // Skip root.zig files — they're typically the module root imported by build.zig
        // via an alias rather than a direct path.
        if (std.mem.eql(u8, basename, "root.zig")) continue;

        // Read source for entry-point markers.
        const abs_path = try std.fmt.allocPrint(aa, "{s}/{s}", .{ workspace, rel_path });
        const src_raw = std.fs.cwd().readFileAlloc(aa, abs_path, 5 * 1024 * 1024) catch continue;

        if (hasPubFnMain(src_raw)) continue;
        if (hasEntryPointMarker(src_raw)) continue;

        const mtime = mtime_map.get(rel_path) orelse 0;
        try orphans.append(allocator, .{
            .source = try allocator.dupe(u8, rel_path),
            .last_modified = mtime,
        });
    }

    return orphans.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Path resolution helper
// ---------------------------------------------------------------------------

/// Resolve an @import path relative to the importing file's directory,
/// returning a workspace-relative canonical path (e.g. "src/foo/bar.zig").
/// Returns an error if the path cannot be resolved to something inside workspace.
fn resolveImportPath(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    file_dir: []const u8,
    import_path: []const u8,
) ![]const u8 {
    // Pure module name imports (e.g. "std", "builtin", "common") have no path
    // separators and don't end in .zig when they are module names — but sibling
    // file imports like `@import("foo.zig")` also have no separator.  The filter
    // in extractImportPaths already restricts to paths ending in ".zig" or
    // containing "/", so anything arriving here without "/" that ends in ".zig"
    // is a sibling file reference.  We treat those as relative ("./" prefix).
    if (!std.mem.startsWith(u8, import_path, ".") and
        !std.mem.startsWith(u8, import_path, "/") and
        !std.mem.endsWith(u8, import_path, ".zig") and
        !std.mem.containsAtLeast(u8, import_path, 1, "/"))
    {
        return error.NotAFilePath;
    }

    // Build a raw joined path.
    const joined = if (file_dir.len > 0)
        try std.fmt.allocPrint(allocator, "{s}/{s}", .{ file_dir, import_path })
    else
        try allocator.dupe(u8, import_path);
    defer allocator.free(joined);

    // Normalise (resolve "..") to get workspace-relative path.
    // We do this manually since std.fs.path.resolve requires absolute paths.
    const abs_workspace = try std.fs.cwd().realpathAlloc(allocator, workspace);
    defer allocator.free(abs_workspace);

    const abs_joined = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ abs_workspace, joined });
    defer allocator.free(abs_joined);

    const abs_resolved = try std.fs.cwd().realpathAlloc(allocator, abs_joined);
    defer allocator.free(abs_resolved);

    // Strip workspace prefix to get workspace-relative path.
    if (!std.mem.startsWith(u8, abs_resolved, abs_workspace)) return error.OutsideWorkspace;
    const stripped = abs_resolved[abs_workspace.len..];
    const rel = if (std.mem.startsWith(u8, stripped, "/")) stripped[1..] else stripped;
    return allocator.dupe(u8, rel);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "orphan: isTestFileName" {
    try std.testing.expect(isTestFileName("tests.zig"));
    try std.testing.expect(isTestFileName("foo_test.zig"));
    try std.testing.expect(isTestFileName("foo_tests.zig"));
    try std.testing.expect(!isTestFileName("foo.zig"));
    try std.testing.expect(!isTestFileName("test_helper.zig"));
}

test "orphan: hasPubFnMain" {
    try std.testing.expect(hasPubFnMain("pub fn main() void {}"));
    try std.testing.expect(!hasPubFnMain("fn helper() void {}"));
}

test "orphan: hasEntryPointMarker" {
    try std.testing.expect(hasEntryPointMarker("// CODEHEALTH: entry-point\npub fn foo() void {}"));
    try std.testing.expect(!hasEntryPointMarker("// just a comment\n"));
}

test "orphan: extractBuildZigRoots finds b.path strings" {
    const allocator = std.testing.allocator;
    const src =
        \\const tests = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/foo.zig"),
        \\    }),
        \\});
        \\const other = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/bar/baz.zig"),
        \\    }),
        \\});
    ;
    const roots = try extractBuildZigRoots(allocator, src);
    defer {
        for (roots) |r| allocator.free(r);
        allocator.free(roots);
    }
    try std.testing.expectEqual(@as(usize, 2), roots.len);
    var found_foo = false;
    var found_baz = false;
    for (roots) |r| {
        if (std.mem.eql(u8, r, "src/foo.zig")) found_foo = true;
        if (std.mem.eql(u8, r, "src/bar/baz.zig")) found_baz = true;
    }
    try std.testing.expect(found_foo);
    try std.testing.expect(found_baz);
}

test "orphan: extractImportPaths finds @import file paths" {
    const allocator = std.testing.allocator;
    const src: [:0]const u8 =
        \\const std = @import("std");
        \\const foo = @import("foo.zig");
        \\const bar = @import("../bar/baz.zig");
        \\pub fn main() void {}
    ;
    const paths = try extractImportPaths(allocator, src);
    defer {
        for (paths) |p| allocator.free(p);
        allocator.free(paths);
    }
    // "std" should be excluded; foo.zig and ../bar/baz.zig should be included.
    var found_foo = false;
    var found_baz = false;
    for (paths) |p| {
        if (std.mem.eql(u8, p, "foo.zig")) found_foo = true;
        if (std.mem.eql(u8, p, "../bar/baz.zig")) found_baz = true;
    }
    try std.testing.expect(found_foo);
    try std.testing.expect(found_baz);
}

test "orphan: findOrphanedFiles detects unimported file" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Create a file that imports nothing and is imported by nothing.
    try tmp.dir.writeFile(.{
        .sub_path = "orphaned.zig",
        .data = "pub fn unused() void {}\n",
    });
    // Create a file that is imported by another — not orphaned.
    try tmp.dir.writeFile(.{
        .sub_path = "imported.zig",
        .data = "pub fn helper() void {}\n",
    });
    // The importer references imported.zig.
    try tmp.dir.writeFile(.{
        .sub_path = "main.zig",
        .data = "const h = @import(\"imported.zig\");\npub fn main() void {}\n",
    });

    const orphans = try findOrphanedFiles(allocator, workspace);
    defer {
        for (orphans) |o| allocator.free(o.source);
        allocator.free(orphans);
    }

    // Only orphaned.zig should be flagged (main.zig has pub fn main, imported.zig is imported).
    try std.testing.expectEqual(@as(usize, 1), orphans.len);
    try std.testing.expectEqualStrings("orphaned.zig", orphans[0].source);
}

test "orphan: findOrphanedFiles skips entry points" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Entry point via pub fn main.
    try tmp.dir.writeFile(.{
        .sub_path = "main.zig",
        .data = "pub fn main() void {}\n",
    });
    // Entry point via CODEHEALTH marker.
    try tmp.dir.writeFile(.{
        .sub_path = "special.zig",
        .data = "// CODEHEALTH: entry-point\npub fn entry() void {}\n",
    });
    // Test file — skipped by naming convention.
    try tmp.dir.writeFile(.{
        .sub_path = "foo_test.zig",
        .data = "test \"foo\" {}\n",
    });

    const orphans = try findOrphanedFiles(allocator, workspace);
    defer {
        for (orphans) |o| allocator.free(o.source);
        allocator.free(orphans);
    }

    try std.testing.expectEqual(@as(usize, 0), orphans.len);
}
