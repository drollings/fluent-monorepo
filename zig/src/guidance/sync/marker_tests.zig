//! Tests for marker.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const marker_mod = @import("marker.zig");

test "fileNeedsProcessing: absent JSON → stale" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
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

    const tmp_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
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

    const tmp_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(tmp_path);

    // Create source file first
    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "validated.zig" });
    defer std.testing.allocator.free(src_abs);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src_abs, .{});
        try f.sync(io);
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

    const tmp_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(tmp_path);

    // Create JSON first (older)
    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "stale.zig.json" });
    defer std.testing.allocator.free(json_abs);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, json_abs, .{});
        try f.sync(io);
        defer f.close(io);
    }

    // Sleep to ensure mtime difference > 1 second
    { const req = std.os.linux.timespec{ .sec = 1, .nsec = 100_000_000 }; _ = std.os.linux.nanosleep(&req, null); } // 1.1 seconds

    // Create source file (newer)
    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "stale.zig" });
    defer std.testing.allocator.free(src_abs);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src_abs, .{});
        try f.sync(io);
        defer f.close(io);
    }

    // JSON is older than source by >1 second → needs processing
    try std.testing.expect(marker_mod.fileNeedsProcessing(src_abs, json_abs));
}
test "testsCanBeSkipped: no marker → false" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
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

    const tmp_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
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

    const tmp_path = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(tmp_path);

    const marker = try marker_mod.testMarkerPath(std.testing.allocator, tmp_path);
    defer std.testing.allocator.free(marker);
    try marker_mod.touchTestMarker(marker);

    // Ensure marker mtime is flushed to filesystem
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const mf = try std.Io.Dir.openFileAbsolute(io, marker, .{});
        defer mf.close(io);
        try mf.sync(io);
    }

    // Sleep to ensure filesystem mtime resolution captures difference.
    // Many filesystems have 1-10ms mtime precision; use 50ms for reliability.
    { const req = std.os.linux.timespec{ .sec = 0, .nsec = 50_000_000 }; _ = std.os.linux.nanosleep(&req, null); }

    // Create source AFTER marker (simulate edit)
    const src = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.zig" });
    defer std.testing.allocator.free(src);
    {
        const io = std.Io.Threaded.global_single_threaded.io();
        const f = try std.Io.Dir.createFileAbsolute(io, src, .{});
        try f.sync(io); // Ensure mtime is written
        defer f.close(io);
    }

    const files = [_][]const u8{src};
    try std.testing.expect(!marker_mod.testsCanBeSkipped(marker, &files));
}

// ---------------------------------------------------------------------------
// Phase 3: contentHash, fileRecord, fileNeedsProcessingHash
// ---------------------------------------------------------------------------

test "contentHash: same content returns same hash" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.Io.Threaded.global_single_threaded.io();

    const path_buf = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(path_buf);
    const file_path = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "test.zig" });
    defer std.testing.allocator.free(file_path);

    const content = "pub fn hello() void {}";
    try tmp.dir.writeFile(io, .{ .sub_path = "test.zig", .data = content });

    const h1 = marker_mod.contentHash(file_path, 1024 * 1024);
    const h2 = marker_mod.contentHash(file_path, 1024 * 1024);
    try std.testing.expect(h1 != null);
    try std.testing.expectEqual(h1.?, h2.?);
}

test "contentHash: different content returns different hash" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.Io.Threaded.global_single_threaded.io();

    const path_buf = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(path_buf);

    const path1 = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "a.zig" });
    defer std.testing.allocator.free(path1);
    const path2 = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "b.zig" });
    defer std.testing.allocator.free(path2);

    try tmp.dir.writeFile(io, .{ .sub_path = "a.zig", .data = "pub fn foo() void {}" });
    try tmp.dir.writeFile(io, .{ .sub_path = "b.zig", .data = "pub fn bar() void {}" });

    const h1 = marker_mod.contentHash(path1, 1024 * 1024);
    const h2 = marker_mod.contentHash(path2, 1024 * 1024);
    try std.testing.expect(h1 != null);
    try std.testing.expect(h2 != null);
    try std.testing.expect(h1.? != h2.?);
}

test "contentHash: missing file returns null" {
    const h = marker_mod.contentHash("/nonexistent/path/file.zig", 1024 * 1024);
    try std.testing.expect(h == null);
}

test "fileNeedsProcessingHash: returns false when mtime advanced but content unchanged" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.Io.Threaded.global_single_threaded.io();

    const path_buf = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(path_buf);

    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "src.zig" });
    defer std.testing.allocator.free(src_abs);
    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "src.zig.json" });
    defer std.testing.allocator.free(json_abs);

    const content = "pub fn stable() void {}";
    try tmp.dir.writeFile(io, .{ .sub_path = "src.zig", .data = content });
    const stored_hash = marker_mod.contentHash(src_abs, 1024 * 1024).?;

    // Create JSON with mtime > src mtime initially.
    try tmp.dir.writeFile(io, .{ .sub_path = "src.zig.json", .data = "{}" });
    try marker_mod.touchFileNowPlusOne(json_abs);

    // Now advance src mtime (same bytes — simulates git checkout).
    try marker_mod.touchFileNow(src_abs);

    // mtime says stale, but hash is unchanged — should return false.
    try std.testing.expect(!marker_mod.fileNeedsProcessingHash(src_abs, json_abs, stored_hash));
}

test "fileNeedsProcessingHash: returns true when both mtime and hash changed" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.Io.Threaded.global_single_threaded.io();

    const path_buf = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(path_buf);

    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "changed.zig" });
    defer std.testing.allocator.free(src_abs);
    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "changed.zig.json" });
    defer std.testing.allocator.free(json_abs);

    try tmp.dir.writeFile(io, .{ .sub_path = "changed.zig", .data = "pub fn old() void {}" });
    const stored_hash = marker_mod.contentHash(src_abs, 1024 * 1024).?;
    try tmp.dir.writeFile(io, .{ .sub_path = "changed.zig.json", .data = "{}" });

    { const req = std.os.linux.timespec{ .sec = 0, .nsec = 50_000_000 }; _ = std.os.linux.nanosleep(&req, null); }
    try tmp.dir.writeFile(io, .{ .sub_path = "changed.zig", .data = "pub fn new() void {}" });

    try std.testing.expect(marker_mod.fileNeedsProcessingHash(src_abs, json_abs, stored_hash));
}

test "fileNeedsProcessingHash: returns true when stored_hash == 0 regardless of content" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.Io.Threaded.global_single_threaded.io();

    const path_buf = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(path_buf);

    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "zero.zig" });
    defer std.testing.allocator.free(src_abs);
    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "zero.zig.json" });
    defer std.testing.allocator.free(json_abs);

    try tmp.dir.writeFile(io, .{ .sub_path = "zero.zig.json", .data = "{}" });
    { const req = std.os.linux.timespec{ .sec = 0, .nsec = 50_000_000 }; _ = std.os.linux.nanosleep(&req, null); }
    try tmp.dir.writeFile(io, .{ .sub_path = "zero.zig", .data = "pub fn foo() void {}" });

    try std.testing.expect(marker_mod.fileNeedsProcessingHash(src_abs, json_abs, 0));
}

test "fileNeedsProcessingHash: returns false when mtime says fresh (fast path)" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.Io.Threaded.global_single_threaded.io();

    const path_buf = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(path_buf);

    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "fresh.zig" });
    defer std.testing.allocator.free(src_abs);
    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ path_buf, "fresh.zig.json" });
    defer std.testing.allocator.free(json_abs);

    try tmp.dir.writeFile(io, .{ .sub_path = "fresh.zig", .data = "pub fn foo() void {}" });
    try tmp.dir.writeFile(io, .{ .sub_path = "fresh.zig.json", .data = "{}" });
    try marker_mod.touchFileNowPlusOne(json_abs);

    try std.testing.expect(!marker_mod.fileNeedsProcessingHash(src_abs, json_abs, 0));
}
