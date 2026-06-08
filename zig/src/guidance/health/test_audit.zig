//! test_audit.zig — Phase 2: Test file convention enforcement.
//!
//! Validates that:
//!   1. Test files discovered in the workspace are covered by at least one build.zig target.

const std = @import("std");

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

pub const AnomalyKind = enum {
    /// A *_tests.zig file is not referenced by any build.zig test target.
    uncovered_test_file,
};

pub const TestAnomaly = struct {
    kind: AnomalyKind,
    /// Workspace-relative path.
    source: []const u8,
};

// ---------------------------------------------------------------------------
// Workspace audit
// ---------------------------------------------------------------------------

/// Walk `workspace` and report test-file anomalies.
/// Caller owns the returned slice and each TestAnomaly's string fields.
pub fn auditTestFiles(
    allocator: std.mem.Allocator,
    workspace: []const u8,
) ![]TestAnomaly {
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const aa = arena.allocator();

    // ── Collect build.zig path refs ───────────────────────────────────────────
    var build_paths = std.StringHashMap(void).init(aa);
    defer build_paths.deinit();

    {
        const build_zig_path = try std.fmt.allocPrint(aa, "{s}/build.zig", .{workspace});
        const build_src = std.Io.Dir.cwd().readFileAlloc(std.Io.Threaded.global_single_threaded.io(), build_zig_path, aa, .limited(5 * 1024 * 1024)) catch null;
        if (build_src) |src| {
            const needle = ".path(\"";
            var pos: usize = 0;
            while (pos < src.len) {
                const found = std.mem.indexOfPos(u8, src, pos, needle) orelse break;
                const start = found + needle.len;
                const end = std.mem.indexOfScalarPos(u8, src, start, '"') orelse break;
                const p = src[start..end];
                if (p.len > 0) try build_paths.put(p, {});
                pos = end + 1;
            }
        }
    }

    // ── Collect paths covered by one level of @import from each build.zig root ─
    // A *_tests.zig file compiled via a shim (e.g. subdirectory_tests.zig that does
    // `_ = @import("comments/core_tests.zig")`) is covered even though it is not
    // directly listed as a root_source_file in build.zig.
    var covered_paths = std.StringHashMap(void).init(aa);
    defer covered_paths.deinit();

    {
        const io = std.Io.Threaded.global_single_threaded.io();
        var it = build_paths.keyIterator();
        while (it.next()) |key| {
            const root_rel = key.*;
            if (!std.mem.endsWith(u8, root_rel, ".zig")) continue;
            const root_abs = try std.fmt.allocPrint(aa, "{s}/{s}", .{ workspace, root_rel });
            const root_src = std.Io.Dir.cwd().readFileAlloc(io, root_abs, aa, .limited(1 * 1024 * 1024)) catch continue;
            const root_dir = std.fs.path.dirname(root_rel) orelse ".";
            const import_needle = "@import(\"";
            var pos: usize = 0;
            while (pos < root_src.len) {
                const found = std.mem.indexOfPos(u8, root_src, pos, import_needle) orelse break;
                const start = found + import_needle.len;
                const end = std.mem.indexOfScalarPos(u8, root_src, start, '"') orelse break;
                const import_path = root_src[start..end];
                pos = end + 1;
                if (!std.mem.endsWith(u8, import_path, ".zig")) continue;
                if (std.mem.startsWith(u8, import_path, "/")) continue; // skip absolute paths
                const resolved = try std.fs.path.join(aa, &.{ root_dir, import_path });
                try covered_paths.put(resolved, {});
            }
        }
    }

    // ── Walk workspace for *_tests.zig files ─────────────────────────────────
    var anomalies: std.ArrayList(TestAnomaly) = .empty;
    errdefer {
        for (anomalies.items) |a| {
            allocator.free(a.source);
        }
        anomalies.deinit(allocator);
    }

    {
        const io = std.Io.Threaded.global_single_threaded.io();
        var base_dir = std.Io.Dir.cwd().openDir(io, workspace, .{ .iterate = true }) catch |err| {
            std.debug.print("[test_audit] cannot open workspace '{s}': {s}\n", .{ workspace, @errorName(err) });
            return allocator.alloc(TestAnomaly, 0);
        };
        defer base_dir.close(io);

        var walker = try base_dir.walk(aa);
        defer walker.deinit();

        while (try walker.next(std.Io.Threaded.global_single_threaded.io())) |entry| {
            if (entry.kind != .file) continue;
            const basename = std.fs.path.basename(entry.path);
            if (!std.mem.endsWith(u8, basename, "_tests.zig")) continue;
            // Skip zig-out and zig-cache.
            if (std.mem.startsWith(u8, entry.path, "zig-")) continue;

            const rel_path = entry.path;

            // Check: is the file covered by a build.zig target (directly or via shim import)?
            if (!build_paths.contains(rel_path) and !covered_paths.contains(rel_path)) {
                try anomalies.append(allocator, .{
                    .kind = .uncovered_test_file,
                    .source = try allocator.dupe(u8, rel_path),
                });
            }
        }
    }

    return anomalies.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Output formatters
// ---------------------------------------------------------------------------

/// Write AI-format section for test anomalies to `w`.
pub fn writeAiOutput(w: *std.Io.Writer, anomalies: []const TestAnomaly) !void {
    if (anomalies.len == 0) return;

    try w.writeAll(
        \\## ⚠️ TEST ORGANISATION ISSUES
        \\
        \\
    );
    for (anomalies) |a| {
        switch (a.kind) {
            .uncovered_test_file => {
                try w.print("### `{s}` not in build.zig\n\n", .{a.source});
                try w.writeAll("**Action:** Add a test target to build.zig or remove the file.\n\n---\n\n");
            },
        }
    }
}

/// Write human-format section for test anomalies.
pub fn writeHumanOutput(w: *std.Io.Writer, anomalies: []const TestAnomaly) !void {
    if (anomalies.len == 0) return;
    try w.print("\nTest organisation issues ({d}):\n", .{anomalies.len});
    for (anomalies) |a| {
        switch (a.kind) {
            .uncovered_test_file => try w.print("  [uncovered] {s}\n", .{a.source}),
        }
    }
}

/// Append a JSON `"test_anomalies":[...]` array fragment (no surrounding braces).
pub fn writeJsonFragment(w: *std.Io.Writer, anomalies: []const TestAnomaly) !void {
    try w.writeAll("\"test_anomalies\":[");
    for (anomalies, 0..) |a, i| {
        if (i > 0) try w.writeAll(",");
        try w.print("{{\"kind\":\"{s}\",\"source\":\"{s}\"}}", .{ @tagName(a.kind), a.source });
    }
    try w.writeAll("]");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
