//! build_validation.zig — Phase 1.5: build.zig consistency validation.
//!
//! Parses build.zig for `b.addTest` / `b.addExecutable` targets and checks:
//!   1. Orphaned test targets: root_source_file references a file that does not exist.
//!   2. Stale references: file was deleted but the build target was not removed.
//!
//! This is a text-based scanner (not full AST) to remain simple and fast.
//! It looks for the pattern:
//!   .root_source_file = b.path("src/foo.zig"),
//! and verifies that `src/foo.zig` exists relative to `workspace`.

const std = @import("std");

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

pub const AnomalyKind = enum {
    /// The file referenced by root_source_file does not exist.
    missing_file,
};

/// An anomaly found in build.zig.
pub const BuildAnomaly = struct {
    kind: AnomalyKind,
    /// The path string as written in build.zig (e.g. "src/guidance/vector_db.zig").
    referenced_path: []const u8,
    /// Line number in build.zig where the reference was found (1-based).
    line: u32,
};

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Scan build.zig source text for all `b.path("...")` path literals and record
/// their line numbers.
const PathRef = struct {
    path: []const u8, // arena-allocated
    line: u32,
};

fn extractPathRefs(allocator: std.mem.Allocator, src: []const u8) ![]PathRef {
    var refs: std.ArrayList(PathRef) = .empty;
    errdefer refs.deinit(allocator);

    const needle = ".path(\"";
    var pos: usize = 0;
    var line: u32 = 1;

    while (pos < src.len) {
        // Advance line counter.
        if (src[pos] == '\n') {
            line += 1;
            pos += 1;
            continue;
        }

        // Check for needle.
        if (std.mem.startsWith(u8, src[pos..], needle)) {
            const start = pos + needle.len;
            const end = std.mem.indexOfScalarPos(u8, src, start, '"') orelse {
                pos += needle.len;
                continue;
            };
            const path = src[start..end];
            // Only record .zig file references (not doc/asset paths).
            if (std.mem.endsWith(u8, path, ".zig")) {
                try refs.append(allocator, .{
                    .path = try allocator.dupe(u8, path),
                    .line = line,
                });
            }
            pos = end + 1;
            continue;
        }

        pos += 1;
    }

    return refs.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate build.zig against the filesystem under `workspace`.
/// Returns anomalies where referenced .zig files do not exist.
/// Caller owns the returned slice and each `referenced_path` string.
pub fn validateBuildZig(
    allocator: std.mem.Allocator,
    workspace: []const u8,
) ![]BuildAnomaly {
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    // Read build.zig.
    const build_zig_path = try std.fmt.allocPrint(aa, "{s}/build.zig", .{workspace});
    const src = std.fs.cwd().readFileAlloc(aa, build_zig_path, 5 * 1024 * 1024) catch |err| {
        std.debug.print("[build_validation] cannot read build.zig: {s}\n", .{@errorName(err)});
        return allocator.alloc(BuildAnomaly, 0);
    };

    const refs = try extractPathRefs(aa, src);

    var anomalies: std.ArrayList(BuildAnomaly) = .empty;
    errdefer {
        for (anomalies.items) |a| allocator.free(a.referenced_path);
        anomalies.deinit(allocator);
    }

    for (refs) |ref| {
        const abs = try std.fmt.allocPrint(aa, "{s}/{s}", .{ workspace, ref.path });
        const exists = blk: {
            std.fs.cwd().access(abs, .{}) catch {
                break :blk false;
            };
            break :blk true;
        };
        if (!exists) {
            try anomalies.append(allocator, .{
                .kind = .missing_file,
                .referenced_path = try allocator.dupe(u8, ref.path),
                .line = ref.line,
            });
        }
    }

    return anomalies.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Output formatters
// ---------------------------------------------------------------------------

/// Write AI-format section for build.zig anomalies to `w`.
pub fn writeAiOutput(w: *std.Io.Writer, anomalies: []const BuildAnomaly) !void {
    if (anomalies.len == 0) return;

    try w.writeAll(
        \\## ⚠️ BUILD.ZIG ANOMALIES (High Confidence)
        \\
        \\> These build.zig targets reference files that do not exist on disk.
        \\
        \\
    );
    for (anomalies) |a| {
        switch (a.kind) {
            .missing_file => {
                try w.print("### Missing: `{s}` (build.zig line {d})\n\n", .{ a.referenced_path, a.line });
                try w.writeAll("**Status:** File does not exist\n");
                try w.writeAll("**Action:** Remove this target from build.zig or restore the file\n\n---\n\n");
            },
        }
    }
}

/// Write human-format section for build.zig anomalies to `w`.
pub fn writeHumanOutput(w: *std.Io.Writer, anomalies: []const BuildAnomaly) !void {
    if (anomalies.len == 0) return;
    try w.print("\nBuild.zig anomalies ({d}):\n", .{anomalies.len});
    for (anomalies) |a| {
        try w.print("  [line {d}] missing: {s}\n", .{ a.line, a.referenced_path });
    }
}

/// Append JSON fields for build.zig anomalies.
/// Writes a `"build_anomalies":[...]` array fragment (no surrounding braces).
pub fn writeJsonFragment(w: *std.Io.Writer, anomalies: []const BuildAnomaly) !void {
    try w.writeAll("\"build_anomalies\":[");
    for (anomalies, 0..) |a, i| {
        if (i > 0) try w.writeAll(",");
        try w.print(
            "{{\"kind\":\"{s}\",\"path\":\"{s}\",\"line\":{d}}}",
            .{ @tagName(a.kind), a.referenced_path, a.line },
        );
    }
    try w.writeAll("]");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "build_validation: extractPathRefs finds .zig paths with line numbers" {
    const allocator = std.testing.allocator;
    const src =
        \\const foo_tests = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/foo.zig"),
        \\    }),
        \\});
        \\const bar_tests = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/bar.zig"),
        \\    }),
        \\});
    ;
    const refs = try extractPathRefs(allocator, src);
    defer {
        for (refs) |r| allocator.free(r.path);
        allocator.free(refs);
    }
    try std.testing.expectEqual(@as(usize, 2), refs.len);
    try std.testing.expectEqualStrings("src/foo.zig", refs[0].path);
    try std.testing.expectEqualStrings("src/bar.zig", refs[1].path);
    // Lines: line 3 for foo.zig, line 8 for bar.zig (zero-based newline counting).
    try std.testing.expectEqual(@as(u32, 3), refs[0].line);
    try std.testing.expectEqual(@as(u32, 8), refs[1].line);
}

test "build_validation: validateBuildZig detects missing file" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Write a build.zig that references a non-existent file.
    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data =
        \\const tests = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/does_not_exist.zig"),
        \\    }),
        \\});
        ,
    });

    const anomalies = try validateBuildZig(allocator, workspace);
    defer {
        for (anomalies) |a| allocator.free(a.referenced_path);
        allocator.free(anomalies);
    }

    try std.testing.expectEqual(@as(usize, 1), anomalies.len);
    try std.testing.expectEqual(AnomalyKind.missing_file, anomalies[0].kind);
    try std.testing.expectEqualStrings("src/does_not_exist.zig", anomalies[0].referenced_path);
}

test "build_validation: validateBuildZig no anomaly for existing file" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Create the referenced file.
    try tmp.dir.makePath("src");
    try tmp.dir.writeFile(.{ .sub_path = "src/real.zig", .data = "pub fn foo() void {}\n" });

    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data =
        \\const tests = b.addTest(.{
        \\    .root_module = b.createModule(.{
        \\        .root_source_file = b.path("src/real.zig"),
        \\    }),
        \\});
        ,
    });

    const anomalies = try validateBuildZig(allocator, workspace);
    defer {
        for (anomalies) |a| allocator.free(a.referenced_path);
        allocator.free(anomalies);
    }

    try std.testing.expectEqual(@as(usize, 0), anomalies.len);
}

test "build_validation: no build.zig returns empty slice" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    const anomalies = try validateBuildZig(allocator, workspace);
    defer allocator.free(anomalies);
    try std.testing.expectEqual(@as(usize, 0), anomalies.len);
}
