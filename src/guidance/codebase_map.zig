//! codebase_map.zig — Structural discovery layer for `guidance explain`.
//!
//! Implements M3 of ROADMAP_20260403_FLUENT_GUIDANCE.md:
//! Build a language-agnostic map of the codebase from README.md, STRUCTURE.md,
//! filesystem walk, and entry point detection.
//!
//! This module is stateless (pure functions) except for CodebaseMap which owns
//! its strings. Call deinit() to release.

const std = @import("std");

// =============================================================================
// Public types
// =============================================================================

/// Defines a build system configuration with ownership, fixed buffers, and invariants; managed centrally.
pub const BuildSystem = enum {
    zig_build,
    make,
    cargo,
    npm,
    gradle,
    cmake,
    poetry,
    unknown,

    pub fn buildCommand(self: BuildSystem) []const u8 {
        return switch (self) {
            .zig_build => "zig build",
            .make => "make",
            .cargo => "cargo build",
            .npm => "npm run build",
            .gradle => "./gradlew build",
            .cmake => "cmake --build .",
            .poetry => "poetry build",
            .unknown => "unknown",
        };
    }
};

/// Manages entry point logic with fixed buffers; owned by the module; ensures consistent initialization.
pub const EntryPoint = struct {
    /// Function/struct name: "main", "cmdExplain", "handleRequest"
    name: []const u8,
    /// Relative file path: "src/guidance/query_engine.zig"
    file_path: []const u8,
    /// Line number within the file (if known)
    line: ?u32,
    /// What kind of entry point this is
    kind: enum { main_fn, cmd_fn, cli_handler, test_fn, server_fn, other },
};

/// Manages directory entries with ownership and invariants; designed for single ownership and not thread-safe.
pub const DirectoryEntry = struct {
    /// Relative path from workspace root
    path: []const u8,
    kind: enum { dir, file },
    /// File extension (e.g., ".zig", ".py") — null for directories
    extension: ?[]const u8,
};

/// Language distribution counts.
pub const LanguageCount = struct {
    extension: []const u8,
    count: u32,
};

/// Tracks code module boundaries; manages references; ensures ownership remains with the codebase.
pub const CodebaseMap = struct {
    allocator: std.mem.Allocator,

    /// Project description from the first paragraph of README.md (or null).
    root_description: ?[]const u8,

    /// Hierarchical directory tree (entries in filesystem order).
    tree: []DirectoryEntry,

    /// Entry points discovered via pattern matching.
    entry_points: []EntryPoint,

    /// Language distribution sorted by count descending.
    language_counts: []LanguageCount,

    /// Detected build system.
    build_system: BuildSystem,

    /// Capability document directories found (e.g., "doc/capabilities").
    capability_dirs: [][]const u8,

    /// Skill document directories found (e.g., "doc/skills", ".guidance/.skills").
    skill_dirs: [][]const u8,

    pub fn deinit(self: *CodebaseMap) void {
        if (self.root_description) |d| self.allocator.free(d);

        for (self.tree) |e| {
            self.allocator.free(e.path);
            if (e.extension) |ext| self.allocator.free(ext);
        }
        self.allocator.free(self.tree);

        for (self.entry_points) |ep| {
            self.allocator.free(ep.name);
            self.allocator.free(ep.file_path);
        }
        self.allocator.free(self.entry_points);

        for (self.language_counts) |lc| {
            self.allocator.free(lc.extension);
        }
        self.allocator.free(self.language_counts);

        for (self.capability_dirs) |d| self.allocator.free(d);
        self.allocator.free(self.capability_dirs);

        for (self.skill_dirs) |d| self.allocator.free(d);
        self.allocator.free(self.skill_dirs);
    }
};

// =============================================================================
// Discovery entry point
// =============================================================================

/// Detects and returns the codebase structure map from the provided workspace.
pub fn discoverStructure(allocator: std.mem.Allocator, workspace: []const u8) !CodebaseMap {
    var tree: std.ArrayList(DirectoryEntry) = .{};
    errdefer {
        for (tree.items) |e| allocator.free(e.path);
        tree.deinit(allocator);
    }

    // Walk the filesystem
    try walkFilesystem(allocator, workspace, &tree);

    // Build system detection
    const build_sys = detectBuildSystem(workspace);

    // Language distribution
    const lang_counts = try countLanguages(allocator, tree.items);
    errdefer {
        for (lang_counts) |lc| allocator.free(lc.extension);
        allocator.free(lang_counts);
    }

    // Entry point detection
    const entry_points = try detectEntryPoints(allocator, tree.items, workspace);
    errdefer {
        for (entry_points) |ep| {
            allocator.free(ep.name);
            allocator.free(ep.file_path);
        }
        allocator.free(entry_points);
    }

    // Capability and skill directory discovery
    const cap_dirs = try findCapabilityDirs(allocator, workspace, tree.items);
    errdefer {
        for (cap_dirs) |d| allocator.free(d);
        allocator.free(cap_dirs);
    }

    const skill_dirs = try findSkillDirs(allocator, workspace, tree.items);
    errdefer {
        for (skill_dirs) |d| allocator.free(d);
        allocator.free(skill_dirs);
    }

    // README description
    const readme_desc = extractReadmeDescription(allocator, workspace) catch null;

    return CodebaseMap{
        .allocator = allocator,
        .root_description = readme_desc,
        .tree = try tree.toOwnedSlice(allocator),
        .entry_points = entry_points,
        .language_counts = lang_counts,
        .build_system = build_sys,
        .capability_dirs = cap_dirs,
        .skill_dirs = skill_dirs,
    };
}

// =============================================================================
// Build system detection
// =============================================================================

/// Detects the build system configuration from a workspace string and returns the corresponding BuildSystem type.
fn detectBuildSystem(workspace: []const u8) BuildSystem {
    const marker_names = [_][]const u8{
        "build.zig",    "Makefile",       "Cargo.toml",     "package.json",
        "build.gradle", "CMakeLists.txt", "pyproject.toml",
    };
    const marker_systems = [_]BuildSystem{
        .zig_build, .make, .cargo, .npm, .gradle, .cmake, .poetry,
    };
    for (marker_names, marker_systems) |name, sys| {
        var path_buf: [std.fs.max_path_bytes]u8 = undefined;
        const path = std.fmt.bufPrintZ(&path_buf, "{s}/{s}", .{ workspace, name }) catch continue;
        std.fs.accessAbsolute(path, .{}) catch continue;
        return sys;
    }
    return .unknown;
}

// =============================================================================
// Filesystem walker
// =============================================================================

const IGNORE_DIRS = [_][]const u8{
    ".git",         "zig-out", "zig-cache", "node_modules", "__pycache__",
    ".guidance.db", "target",  ".build",    "dist",         "build",
};

/// Checks if a given name slice should be ignored based on directory context.
fn shouldIgnoreDir(name: []const u8) bool {
    for (IGNORE_DIRS) |ig| {
        if (std.mem.eql(u8, name, ig)) return true;
    }
    if (name.len > 0 and name[0] == '.') {
        // Skip hidden dirs EXCEPT .guidance/ which is important
        if (!std.mem.eql(u8, name, ".guidance")) return true;
    }
    return false;
}

/// Traverses a directory tree using an allocator and returns a walkable path.
fn walkFilesystem(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    tree: *std.ArrayList(DirectoryEntry),
) !void {
    var dir = std.fs.openDirAbsolute(workspace, .{ .iterate = true }) catch return;
    defer dir.close();
    try walkDir(allocator, dir, "", tree, 0);
}

/// Traverses a directory structure recursively, handling allocators and depth tracking.
fn walkDir(
    allocator: std.mem.Allocator,
    dir: std.fs.Dir,
    rel_prefix: []const u8,
    tree: *std.ArrayList(DirectoryEntry),
    depth: u32,
) !void {
    if (depth > 8) return; // Max depth guard

    var it = dir.iterate();
    while (try it.next()) |entry| {
        if (entry.name.len == 0) continue;

        const rel_path = if (rel_prefix.len == 0)
            try allocator.dupe(u8, entry.name)
        else
            try std.fs.path.join(allocator, &.{ rel_prefix, entry.name });

        switch (entry.kind) {
            .directory => {
                if (shouldIgnoreDir(entry.name)) {
                    allocator.free(rel_path);
                    continue;
                }
                try tree.append(allocator, .{
                    .path = rel_path,
                    .kind = .dir,
                    .extension = null,
                });
                var sub = dir.openDir(entry.name, .{ .iterate = true }) catch continue;
                defer sub.close();
                try walkDir(allocator, sub, rel_path, tree, depth + 1);
            },
            .file => {
                const ext_raw = std.fs.path.extension(entry.name);
                // Dupe the extension — entry.name is owned by the dir iterator
                // and becomes invalid on the next iteration.
                const ext = if (ext_raw.len > 0) try allocator.dupe(u8, ext_raw) else null;
                try tree.append(allocator, .{
                    .path = rel_path,
                    .kind = .file,
                    .extension = ext,
                });
            },
            else => allocator.free(rel_path),
        }
    }
}

// =============================================================================
// Language distribution
// =============================================================================

/// Counts language directories in a Zig project structure and returns their counts.
fn countLanguages(allocator: std.mem.Allocator, tree: []const DirectoryEntry) ![]LanguageCount {
    // Collect all extensions then sort and count — avoids StringHashMap growth panics
    // on large repos where the map resizes with borrowed-slice keys.
    var exts: std.ArrayList([]const u8) = .{};
    defer exts.deinit(allocator);

    for (tree) |e| {
        if (e.kind != .file) continue;
        const ext = e.extension orelse continue;
        try exts.append(allocator, ext);
    }

    std.sort.block([]const u8, exts.items, {}, struct {
        fn lt(_: void, a: []const u8, b: []const u8) bool {
            return std.mem.lessThan(u8, a, b);
        }
    }.lt);

    var result: std.ArrayList(LanguageCount) = .{};
    errdefer {
        for (result.items) |lc| allocator.free(lc.extension);
        result.deinit(allocator);
    }

    var i: usize = 0;
    while (i < exts.items.len) {
        const ext = exts.items[i];
        var count: u32 = 0;
        while (i < exts.items.len and std.mem.eql(u8, exts.items[i], ext)) : (i += 1) {
            count += 1;
        }
        try result.append(allocator, .{
            .extension = try allocator.dupe(u8, ext),
            .count = count,
        });
    }

    std.sort.block(LanguageCount, result.items, {}, struct {
        fn lt(_: void, a: LanguageCount, b: LanguageCount) bool {
            return a.count > b.count;
        }
    }.lt);

    return result.toOwnedSlice(allocator);
}

// =============================================================================
// Entry point detection
// =============================================================================

const ENTRY_PREFIXES = [_][]const u8{ "cmd", "handle", "run", "serve", "start" };
const ENTRY_SUFFIXES = [_][]const u8{ "Cli", "Handler", "Server", "Main" };
const ENTRY_EXACT = [_][]const u8{ "main", "run", "start" };

/// Determines the type of an entry point from a Zig string slice.
fn classifyEntryPoint(name: []const u8) @TypeOf(@as(EntryPoint, undefined).kind) {
    if (std.mem.eql(u8, name, "main")) return .main_fn;
    if (std.mem.startsWith(u8, name, "cmd")) return .cmd_fn;
    if (std.mem.endsWith(u8, name, "Handler") or
        std.mem.startsWith(u8, name, "handle")) return .cli_handler;
    if (std.mem.startsWith(u8, name, "serve") or
        std.mem.endsWith(u8, name, "Server")) return .server_fn;
    return .other;
}

/// Checks if a given byte slice represents an entry point name, returning true or false.
fn isEntryPointName(name: []const u8) bool {
    for (ENTRY_EXACT) |e| {
        if (std.mem.eql(u8, name, e)) return true;
    }
    for (ENTRY_PREFIXES) |p| {
        if (std.mem.startsWith(u8, name, p)) return true;
    }
    for (ENTRY_SUFFIXES) |s| {
        if (std.mem.endsWith(u8, name, s)) return true;
    }
    return false;
}

/// Detects entry points in a directory tree using an allocator and workspace parameters.
fn detectEntryPoints(
    allocator: std.mem.Allocator,
    tree: []const DirectoryEntry,
    workspace: []const u8,
) ![]EntryPoint {
    var entries: std.ArrayList(EntryPoint) = .{};
    errdefer {
        for (entries.items) |ep| {
            allocator.free(ep.name);
            allocator.free(ep.file_path);
        }
        entries.deinit(allocator);
    }

    for (tree) |e| {
        if (e.kind != .file) continue;
        const ext = e.extension orelse continue;

        // Only scan source files
        const is_source = blk: {
            const src_exts = [_][]const u8{ ".zig", ".py", ".rs", ".go", ".ts", ".js", ".c", ".cpp" };
            for (src_exts) |se| {
                if (std.ascii.eqlIgnoreCase(ext, se)) break :blk true;
            }
            break :blk false;
        };
        if (!is_source) continue;

        // Check guidance JSON for member names (fast path, best results)
        const json_name = try std.fmt.allocPrint(allocator, "{s}.json", .{e.path});
        defer allocator.free(json_name);
        const guidance_path = try std.fs.path.join(allocator, &.{ workspace, ".guidance", "src", json_name });
        defer allocator.free(guidance_path);

        const json_content = std.fs.cwd().readFileAlloc(allocator, guidance_path, 512 * 1024) catch continue;
        defer allocator.free(json_content);

        // Simple JSON scan: look for "name": "..." patterns
        var search_pos: usize = 0;
        while (std.mem.indexOfPos(u8, json_content, search_pos, "\"name\":")) |pos| {
            search_pos = pos + 7;
            // Skip whitespace
            var val_start = search_pos;
            while (val_start < json_content.len and
                (json_content[val_start] == ' ' or json_content[val_start] == '\t'))
            {
                val_start += 1;
            }
            if (val_start >= json_content.len or json_content[val_start] != '"') continue;
            val_start += 1;
            const val_end = std.mem.indexOfScalarPos(u8, json_content, val_start, '"') orelse continue;
            const name = json_content[val_start..val_end];

            if (isEntryPointName(name)) {
                try entries.append(allocator, .{
                    .name = try allocator.dupe(u8, name),
                    .file_path = try allocator.dupe(u8, e.path),
                    .line = null,
                    .kind = classifyEntryPoint(name),
                });
            }
            search_pos = val_end + 1;
        }
    }

    return entries.toOwnedSlice(allocator);
}

// =============================================================================
// Capability and skill directory discovery
// =============================================================================

const CAPABILITY_DIR_NAMES = [_][]const u8{ "capabilities", "docs/capabilities", "doc/capabilities" };
const SKILL_DIR_NAMES = [_][]const u8{ "skills", ".skills", "doc/skills", ".guidance/.skills", ".guidance/skills" };

/// Identifies directory paths for allocation within a workspace tree using an allocator.
fn findCapabilityDirs(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    tree: []const DirectoryEntry,
) ![][]const u8 {
    var dirs: std.ArrayList([]const u8) = .{};
    errdefer {
        for (dirs.items) |d| allocator.free(d);
        dirs.deinit(allocator);
    }

    for (tree) |e| {
        if (e.kind != .dir) continue;
        for (CAPABILITY_DIR_NAMES) |cn| {
            if (std.mem.endsWith(u8, e.path, cn)) {
                try dirs.append(allocator, try allocator.dupe(u8, e.path));
                break;
            }
        }
    }

    // Also check known absolute paths
    for (CAPABILITY_DIR_NAMES) |cn| {
        const abs = try std.fs.path.join(allocator, &.{ workspace, cn });
        defer allocator.free(abs);
        std.fs.accessAbsolute(abs, .{}) catch continue;
        // Check not already in list
        var found = false;
        for (dirs.items) |d| {
            if (std.mem.eql(u8, d, cn)) {
                found = true;
                break;
            }
        }
        if (!found) try dirs.append(allocator, try allocator.dupe(u8, cn));
    }

    return dirs.toOwnedSlice(allocator);
}

/// Finds a list of directory entries based on allocator, workspace, and tree structure.
fn findSkillDirs(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    tree: []const DirectoryEntry,
) ![][]const u8 {
    _ = tree;
    var dirs: std.ArrayList([]const u8) = .{};
    errdefer {
        for (dirs.items) |d| allocator.free(d);
        dirs.deinit(allocator);
    }

    for (SKILL_DIR_NAMES) |sn| {
        const abs = try std.fs.path.join(allocator, &.{ workspace, sn });
        defer allocator.free(abs);
        std.fs.accessAbsolute(abs, .{}) catch continue;
        try dirs.append(allocator, try allocator.dupe(u8, sn));
    }

    return dirs.toOwnedSlice(allocator);
}

// =============================================================================
// README description extraction
// =============================================================================

/// Extracts a readable description from a workspace string, returning a Zig slice.
fn extractReadmeDescription(allocator: std.mem.Allocator, workspace: []const u8) ![]const u8 {
    const readme_names = [_][]const u8{ "README.md", "README.txt", "README" };

    for (readme_names) |rn| {
        const path = try std.fs.path.join(allocator, &.{ workspace, rn });
        defer allocator.free(path);
        const content = std.fs.cwd().readFileAlloc(allocator, path, 64 * 1024) catch continue;
        defer allocator.free(content);

        // Find first non-heading paragraph
        var lines = std.mem.splitScalar(u8, content, '\n');
        var paragraph_buf: std.ArrayList(u8) = .{};
        defer paragraph_buf.deinit(allocator);
        var in_paragraph = false;

        while (lines.next()) |line| {
            const trimmed = std.mem.trim(u8, line, " \t\r");
            if (trimmed.len == 0) {
                if (in_paragraph and paragraph_buf.items.len > 20) {
                    return paragraph_buf.toOwnedSlice(allocator);
                }
                in_paragraph = false;
                continue;
            }
            if (std.mem.startsWith(u8, trimmed, "#")) continue; // heading
            if (std.mem.startsWith(u8, trimmed, "```")) continue; // code block
            if (std.mem.startsWith(u8, trimmed, "---")) continue; // separator

            if (in_paragraph) {
                try paragraph_buf.append(allocator, ' ');
            }
            try paragraph_buf.appendSlice(allocator, trimmed);
            in_paragraph = true;

            if (paragraph_buf.items.len > 300) break;
        }

        if (paragraph_buf.items.len > 20) {
            return paragraph_buf.toOwnedSlice(allocator);
        }
    }

    return error.NoReadme;
}

// =============================================================================
// Tests
// =============================================================================

test "detectBuildSystem: returns zig_build for this repo" {
    const cwd = try std.process.getCwdAlloc(std.testing.allocator);
    defer std.testing.allocator.free(cwd);
    // This test runs from the project root, which has build.zig
    const bs = detectBuildSystem(cwd);
    try std.testing.expect(bs == .zig_build or bs == .make or bs == .unknown);
}

test "looksLikeIdentifier via ENTRY_EXACT: main is an entry point" {
    try std.testing.expect(isEntryPointName("main"));
    try std.testing.expect(isEntryPointName("cmdExplain"));
    try std.testing.expect(isEntryPointName("handleRequest"));
    try std.testing.expect(!isEntryPointName("parseToken"));
}
















