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
// Fix: add uncovered *_tests.zig files to build.zig
// ---------------------------------------------------------------------------

/// Stats returned by fixUncoveredTestFiles.
pub const FixBuildStats = struct {
    checked: usize = 0,
    added: usize = 0,
    skipped: usize = 0,
};

/// For each `*_tests.zig` path in `uncovered`, check whether its companion
/// `*.zig` is referenced in build.zig and, if so, insert a minimal addTest
/// block + test_step.dependOn line.
///
/// "Valid" means:
///   1. `stem.zig` exists on disk.
///   2. `stem.zig` is already referenced in build.zig (so its modules are set up).
///
/// Insertion anchors used:
///   - addTest block: inserted immediately before the `// ---...` separator
///     that precedes section "4. Benchmark step".
///   - dependOn line: appended after the last `test_step.dependOn(` line.
///
/// If no benchmark separator exists the insertion falls back to appending before
/// the first blank line after the last `test_step.dependOn` call.
pub fn fixUncoveredTestFiles(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    uncovered: []const []const u8,
    dry_run: bool,
) !FixBuildStats {
    var stats = FixBuildStats{};
    if (uncovered.len == 0) return stats;

    const build_zig_path = try std.fmt.allocPrint(allocator, "{s}/build.zig", .{workspace});
    defer allocator.free(build_zig_path);

    // Read build.zig once; we'll re-read after each write so the offsets stay valid.
    for (uncovered) |rel_path| {
        stats.checked += 1;
        const added = addOneTestTarget(allocator, build_zig_path, workspace, rel_path, dry_run) catch |err| {
            std.debug.print("[fix-build] {s}: {s}\n", .{ rel_path, @errorName(err) });
            stats.skipped += 1;
            continue;
        };
        if (added) stats.added += 1 else stats.skipped += 1;
    }
    return stats;
}

/// Add one test target for `tests_zig_rel` (e.g. `src/testing/mock_vtable_tests.zig`).
/// Returns true if build.zig was modified (or would be in dry_run).
fn addOneTestTarget(
    allocator: std.mem.Allocator,
    build_zig_path: []const u8,
    workspace: []const u8,
    tests_zig_rel: []const u8,
    dry_run: bool,
) !bool {
    const basename = std.fs.path.basename(tests_zig_rel);
    if (!std.mem.endsWith(u8, basename, "_tests.zig")) return false;

    // Companion: strip _tests suffix → stem.zig
    const stem = basename[0 .. basename.len - "_tests.zig".len];
    const dir = std.fs.path.dirname(tests_zig_rel) orelse "";
    const companion_rel = if (dir.len > 0)
        try std.fmt.allocPrint(allocator, "{s}/{s}.zig", .{ dir, stem })
    else
        try std.fmt.allocPrint(allocator, "{s}.zig", .{stem});
    defer allocator.free(companion_rel);

    const src = try std.fs.cwd().readFileAlloc(allocator, build_zig_path, 5 * 1024 * 1024);
    defer allocator.free(src);

    // Must not already be referenced.
    if (std.mem.indexOf(u8, src, tests_zig_rel) != null) return false;

    // Companion must be referenced in build.zig (validates it's a "built" file).
    if (std.mem.indexOf(u8, src, companion_rel) == null) return false;

    // Companion must exist on disk.
    const companion_abs = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ workspace, companion_rel });
    defer allocator.free(companion_abs);
    std.fs.cwd().access(companion_abs, .{}) catch return false;

    // Derive a Zig identifier for the variable name from the file stem.
    // e.g. "mock_vtable_tests.zig" → "mock_vtable_tests"
    const var_name = basename[0 .. basename.len - ".zig".len];

    if (dry_run) {
        std.debug.print("[fix-build] would add test target '{s}' for {s}\n", .{ var_name, tests_zig_rel });
        return true;
    }

    // Build the addTest block.
    const add_block = try std.fmt.allocPrint(allocator,
        \\
        \\    const {s} = b.addTest(.{{
        \\        .root_module = b.createModule(.{{
        \\            .root_source_file = b.path("{s}"),
        \\            .target = target,
        \\            .optimize = optimize,
        \\        }}),
        \\    }});
        \\
    , .{ var_name, tests_zig_rel });
    defer allocator.free(add_block);

    // Build the dependOn line.
    const depend_line = try std.fmt.allocPrint(
        allocator,
        "    test_step.dependOn(&b.addRunArtifact({s}).step);\n",
        .{var_name},
    );
    defer allocator.free(depend_line);

    // ── Locate insertion point for addTest block ──────────────────────────────
    // Prefer: immediately before the "// ---..." separator of section 4 (Benchmark).
    // Fallback: before the first blank line after the last test_step.dependOn call.
    const benchmark_sep = "// 4. Benchmark";
    const block_insert: usize = blk: {
        if (std.mem.indexOf(u8, src, benchmark_sep)) |bpos| {
            // Walk back to find the start of the separator comment block (the dashes line).
            const dashes = "// ---";
            if (std.mem.lastIndexOf(u8, src[0..bpos], dashes)) |dpos| {
                // Find the newline before the dashes line.
                if (std.mem.lastIndexOf(u8, src[0..dpos], "\n")) |nl| {
                    break :blk nl + 1;
                }
                break :blk dpos;
            }
            break :blk bpos;
        }
        // Fallback: after last `test_step.dependOn` line.
        const needle = "test_step.dependOn(";
        var last_end: usize = 0;
        var pos: usize = 0;
        while (std.mem.indexOfPos(u8, src, pos, needle)) |p| {
            last_end = (std.mem.indexOfScalarPos(u8, src, p, '\n') orelse src.len - 1) + 1;
            pos = last_end;
        }
        break :blk last_end;
    };

    // ── Locate insertion point for dependOn line ──────────────────────────────
    // After the last `test_step.dependOn(` line.
    const needle = "test_step.dependOn(";
    var depend_insert: usize = 0;
    var pos: usize = 0;
    while (std.mem.indexOfPos(u8, src, pos, needle)) |p| {
        depend_insert = (std.mem.indexOfScalarPos(u8, src, p, '\n') orelse src.len - 1) + 1;
        pos = depend_insert;
    }
    if (depend_insert == 0) return error.NoDependOnFound;

    // Build new source (block_insert < depend_insert always, since addTest
    // declarations come before the test_step.dependOn calls in build.zig).
    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    try out.appendSlice(allocator, src[0..block_insert]);
    try out.appendSlice(allocator, add_block);
    try out.appendSlice(allocator, src[block_insert..depend_insert]);
    try out.appendSlice(allocator, depend_line);
    try out.appendSlice(allocator, src[depend_insert..]);

    try std.fs.cwd().writeFile(.{ .sub_path = build_zig_path, .data = out.items });
    std.debug.print("[fix-build] added test target '{s}' for {s}\n", .{ var_name, tests_zig_rel });
    return true;
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

test "build_validation: fixUncoveredTestFiles adds test target" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    // Create the companion source file.
    try tmp.dir.makePath("src/testing");
    try tmp.dir.writeFile(.{ .sub_path = "src/testing/mock_vtable.zig", .data = "pub fn foo() void {}\n" });
    try tmp.dir.writeFile(.{ .sub_path = "src/testing/mock_vtable_tests.zig", .data = "test \"x\" {}\n" });

    // Write a minimal build.zig that references mock_vtable.zig but not mock_vtable_tests.zig.
    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data =
        \\    const mock_vtable_tests = b.addTest(.{
        \\        .root_module = b.createModule(.{
        \\            .root_source_file = b.path("src/testing/mock_vtable.zig"),
        \\            .target = target,
        \\            .optimize = optimize,
        \\        }),
        \\    });
        \\
        \\    // -------------------------------------------------------------------------
        \\    // 4. Benchmark step (G5)
        \\    // -------------------------------------------------------------------------
        \\
        \\    test_step.dependOn(&b.addRunArtifact(mock_vtable_tests).step);
        ,
    });

    const uncovered = [_][]const u8{"src/testing/mock_vtable_tests.zig"};
    const stats = try fixUncoveredTestFiles(allocator, workspace, &uncovered, false);

    try std.testing.expectEqual(@as(usize, 1), stats.added);
    try std.testing.expectEqual(@as(usize, 0), stats.skipped);

    const build_zig_path = try std.fmt.allocPrint(allocator, "{s}/build.zig", .{workspace});
    defer allocator.free(build_zig_path);
    const result = try std.fs.cwd().readFileAlloc(allocator, build_zig_path, 1024 * 1024);
    defer allocator.free(result);

    try std.testing.expect(std.mem.indexOf(u8, result, "src/testing/mock_vtable_tests.zig") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "mock_vtable_tests_tests") == null);
}

test "build_validation: fixUncoveredTestFiles skips if companion not in build.zig" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const workspace = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(workspace);

    try tmp.dir.makePath("src");
    try tmp.dir.writeFile(.{ .sub_path = "src/unrelated.zig", .data = "pub fn x() void {}\n" });
    try tmp.dir.writeFile(.{ .sub_path = "src/orphan_tests.zig", .data = "test \"t\" {}\n" });

    // build.zig does NOT reference src/orphan.zig.
    try tmp.dir.writeFile(.{
        .sub_path = "build.zig",
        .data =
        \\    test_step.dependOn(&b.addRunArtifact(unrelated_tests).step);
        ,
    });

    const uncovered = [_][]const u8{"src/orphan_tests.zig"};
    const stats = try fixUncoveredTestFiles(allocator, workspace, &uncovered, false);

    try std.testing.expectEqual(@as(usize, 0), stats.added);
    try std.testing.expectEqual(@as(usize, 1), stats.skipped);
}
