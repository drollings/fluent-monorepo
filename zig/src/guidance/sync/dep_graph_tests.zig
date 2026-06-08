//! Tests for dep_graph.zig.
const std = @import("std");
const dep_graph = @import("dep_graph.zig");
const DepGraph = dep_graph.DepGraph;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Dupe a slice of string literals into heap-owned strings suitable for
/// passing to `setDeps` (which takes ownership).
fn dupeTargets(allocator: std.mem.Allocator, literals: []const []const u8) ![][]const u8 {
    const out = try allocator.alloc([]const u8, literals.len);
    for (literals, 0..) |lit, i| out[i] = try allocator.dupe(u8, lit);
    return out;
}

// ---------------------------------------------------------------------------
// Basic setDeps + getImportedBy
// ---------------------------------------------------------------------------

test "setDeps + getImportedBy: basic case returns correct importers" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var dg = DepGraph.init(allocator);
    defer dg.deinit();

    const targets = try dupeTargets(allocator, &.{"src/common/log.zig"});
    try dg.setDeps("src/foo/bar.zig", targets);

    const importers = try dg.getImportedBy("src/common/log.zig", allocator);
    defer {
        for (importers) |s| allocator.free(s);
        allocator.free(importers);
    }

    try std.testing.expectEqual(@as(usize, 1), importers.len);
    try std.testing.expectEqualStrings("src/foo/bar.zig", importers[0]);
}

test "setDeps + getImportedBy: multiple importers sorted" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var dg = DepGraph.init(allocator);
    defer dg.deinit();

    try dg.setDeps("src/b.zig", try dupeTargets(allocator, &.{"src/common/log.zig"}));
    try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{"src/common/log.zig"}));

    const importers = try dg.getImportedBy("src/common/log.zig", allocator);
    defer {
        for (importers) |s| allocator.free(s);
        allocator.free(importers);
    }

    try std.testing.expectEqual(@as(usize, 2), importers.len);
    // Results must be sorted.
    try std.testing.expectEqualStrings("src/a.zig", importers[0]);
    try std.testing.expectEqualStrings("src/b.zig", importers[1]);
}

// ---------------------------------------------------------------------------
// Update (setDeps called twice for same importer)
// ---------------------------------------------------------------------------

test "setDeps update: old reverse edges removed, new ones added" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var dg = DepGraph.init(allocator);
    defer dg.deinit();

    // Initial: a.zig imports old_dep.zig
    try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{"src/old_dep.zig"}));

    // Update: a.zig now imports new_dep.zig instead
    try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{"src/new_dep.zig"}));

    // old_dep.zig should have no importers
    const old_importers = try dg.getImportedBy("src/old_dep.zig", allocator);
    defer {
        for (old_importers) |s| allocator.free(s);
        allocator.free(old_importers);
    }
    try std.testing.expectEqual(@as(usize, 0), old_importers.len);

    // new_dep.zig should be imported by a.zig
    const new_importers = try dg.getImportedBy("src/new_dep.zig", allocator);
    defer {
        for (new_importers) |s| allocator.free(s);
        allocator.free(new_importers);
    }
    try std.testing.expectEqual(@as(usize, 1), new_importers.len);
    try std.testing.expectEqualStrings("src/a.zig", new_importers[0]);
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

test "remove: importer no longer appears in reverse results" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var dg = DepGraph.init(allocator);
    defer dg.deinit();

    try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{"src/common/log.zig"}));
    try dg.setDeps("src/b.zig", try dupeTargets(allocator, &.{"src/common/log.zig"}));

    dg.remove("src/a.zig");

    const importers = try dg.getImportedBy("src/common/log.zig", allocator);
    defer {
        for (importers) |s| allocator.free(s);
        allocator.free(importers);
    }
    try std.testing.expectEqual(@as(usize, 1), importers.len);
    try std.testing.expectEqualStrings("src/b.zig", importers[0]);
}

test "remove: target removal removes it from forward slices" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var dg = DepGraph.init(allocator);
    defer dg.deinit();

    try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{ "src/common/log.zig", "src/common/hash.zig" }));

    dg.remove("src/common/log.zig");

    // After removing the target, querying it returns empty.
    const importers = try dg.getImportedBy("src/common/log.zig", allocator);
    defer {
        for (importers) |s| allocator.free(s);
        allocator.free(importers);
    }
    try std.testing.expectEqual(@as(usize, 0), importers.len);
}

// ---------------------------------------------------------------------------
// Basename matching
// ---------------------------------------------------------------------------

test "basename matching: target found when importer uses short name" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var dg = DepGraph.init(allocator);
    defer dg.deinit();

    // Importer recorded with basename key (as if import was @import("log.zig")).
    const targets = try dupeTargets(allocator, &.{"log.zig"});
    try dg.setDeps("src/foo.zig", targets);

    // Query with full path — basename match should find src/foo.zig.
    const importers = try dg.getImportedBy("src/common/log.zig", allocator);
    defer {
        for (importers) |s| allocator.free(s);
        allocator.free(importers);
    }
    try std.testing.expectEqual(@as(usize, 1), importers.len);
    try std.testing.expectEqualStrings("src/foo.zig", importers[0]);
}

test "basename matching: no duplicates when both full-path and basename match" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var dg = DepGraph.init(allocator);
    defer dg.deinit();

    // One importer recorded under full path, another under basename.
    try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{"src/common/log.zig"}));
    try dg.setDeps("src/b.zig", try dupeTargets(allocator, &.{"log.zig"}));

    const importers = try dg.getImportedBy("src/common/log.zig", allocator);
    defer {
        for (importers) |s| allocator.free(s);
        allocator.free(importers);
    }
    // Both importers found, no duplicates.
    try std.testing.expectEqual(@as(usize, 2), importers.len);
}

// ---------------------------------------------------------------------------
// Empty result
// ---------------------------------------------------------------------------

test "getImportedBy: no importers returns empty slice" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var dg = DepGraph.init(allocator);
    defer dg.deinit();

    const importers = try dg.getImportedBy("src/nothing/here.zig", allocator);
    defer {
        for (importers) |s| allocator.free(s);
        allocator.free(importers);
    }
    try std.testing.expectEqual(@as(usize, 0), importers.len);
}

// ---------------------------------------------------------------------------
// GPA leak check: all operations
// ---------------------------------------------------------------------------

test "GPA no leaks: setDeps + getImportedBy + deinit" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    {
        var dg = DepGraph.init(allocator);
        defer dg.deinit();

        try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{ "src/x.zig", "src/y.zig" }));
        try dg.setDeps("src/b.zig", try dupeTargets(allocator, &.{"src/x.zig"}));

        const importers = try dg.getImportedBy("src/x.zig", allocator);
        defer {
            for (importers) |s| allocator.free(s);
            allocator.free(importers);
        }
        try std.testing.expectEqual(@as(usize, 2), importers.len);
    }
}

test "GPA no leaks: update (setDeps twice) + deinit" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    {
        var dg = DepGraph.init(allocator);
        defer dg.deinit();

        try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{"src/old.zig"}));
        try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{"src/new.zig"}));
    }
}

test "GPA no leaks: remove + deinit" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    {
        var dg = DepGraph.init(allocator);
        defer dg.deinit();

        try dg.setDeps("src/a.zig", try dupeTargets(allocator, &.{"src/log.zig"}));
        try dg.setDeps("src/b.zig", try dupeTargets(allocator, &.{"src/log.zig"}));
        dg.remove("src/a.zig");
    }
}
