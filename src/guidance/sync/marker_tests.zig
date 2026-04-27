//! Tests for marker.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const marker_mod = @import("marker.zig");

test "fileNeedsProcessing: absent JSON → stale" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "foo.zig" });
    defer std.testing.allocator.free(src_abs);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src_abs, .{});
        defer f.close(io);
    }
    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "foo.zig.json" });
    defer std.testing.allocator.free(json_abs);

    // JSON does not exist → stale.
    try std.testing.expect(marker_mod.fileNeedsProcessing(src_abs, json_abs));
}
test "fileNeedsProcessing: JSON written after source → fresh" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "foo.zig" });
    defer std.testing.allocator.free(src_abs);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src_abs, .{});
        defer f.close(io);
    }

    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "foo.zig.json" });
    defer std.testing.allocator.free(json_abs);

    // Create JSON after source so its mtime is >= source mtime.
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, json_abs, .{});
        defer f.close(io);
    }

    try std.testing.expect(!marker_mod.fileNeedsProcessing(src_abs, json_abs));
}
test "fileNeedsProcessing: JSON mtime = now + 1s → validated (skip)" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    // Create source file first
    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "validated.zig" });
    defer std.testing.allocator.free(src_abs);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src_abs, .{});
        try f.sync();
        defer f.close(io);
    }

    // Create JSON file
    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "validated.zig.json" });
    defer std.testing.allocator.free(json_abs);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, json_abs, .{});
        defer f.close(io);
    }

    // Use touchFileNowPlusOne to set JSON mtime to "now + 1 second" (validated pattern)
    try marker_mod.touchFileNowPlusOne(json_abs);

    // fileNeedsProcessing should return false for validated files
    // JSON is in the future, so src_mtime <= json_mtime always holds
    try std.testing.expect(!marker_mod.fileNeedsProcessing(src_abs, json_abs));
}
test "fileNeedsProcessing: JSON older by >1s → needs processing" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    // Create JSON first (older)
    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "stale.zig.json" });
    defer std.testing.allocator.free(json_abs);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, json_abs, .{});
        try f.sync();
        defer f.close(io);
    }

    // Sleep to ensure mtime difference > 1 second
    std.Thread.sleep(1100_000_000); // 1.1 seconds

    // Create source file (newer)
    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "stale.zig" });
    defer std.testing.allocator.free(src_abs);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src_abs, .{});
        try f.sync();
        defer f.close(io);
    }

    // JSON is older than source by >1 second → needs processing
    try std.testing.expect(marker_mod.fileNeedsProcessing(src_abs, json_abs));
}
test "testsCanBeSkipped: no marker → false" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const marker = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, ".marks", marker_mod.TEST_MARKER_NAME });
    defer std.testing.allocator.free(marker);

    const src = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.zig" });
    defer std.testing.allocator.free(src);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src, .{});
        defer f.close(io);
    }

    const files = [_][]const u8{src};
    try std.testing.expect(!marker_mod.testsCanBeSkipped(marker, &files));
}
test "testsCanBeSkipped: marker newer than source → true" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const src = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.zig" });
    defer std.testing.allocator.free(src);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src, .{});
        defer f.close(io);
    }

    const marker = try marker_mod.testMarkerPath(std.testing.allocator, tmp_path);
    defer std.testing.allocator.free(marker);
    try marker_mod.touchTestMarker(marker);

    const files = [_][]const u8{src};
    try std.testing.expect(marker_mod.testsCanBeSkipped(marker, &files));
}
test "testsCanBeSkipped: source newer than marker → false" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const marker = try marker_mod.testMarkerPath(std.testing.allocator, tmp_path);
    defer std.testing.allocator.free(marker);
    try marker_mod.touchTestMarker(marker);

    // Ensure marker mtime is flushed to filesystem
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const mf = try std.Io.Dir.openFileAbsolute(io, marker, .{});
        defer mf.close(io);
        try mf.sync();
    }

    // Sleep to ensure filesystem mtime resolution captures difference.
    // Many filesystems have 1-10ms mtime precision; use 50ms for reliability.
    std.Thread.sleep(50_000_000);

    // Create source AFTER marker (simulate edit)
    const src = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.zig" });
    defer std.testing.allocator.free(src);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src, .{});
        try f.sync(); // Ensure mtime is written
        defer f.close(io);
    }

    const files = [_][]const u8{src};
    try std.testing.expect(!marker_mod.testsCanBeSkipped(marker, &files));
}
