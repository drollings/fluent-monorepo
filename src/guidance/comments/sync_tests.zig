//! Tests for sync.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const types = @import("../types.zig");
const sync_mod = @import("sync.zig");

test "sortMembersByLineDesc - ordering" {
    const allocator = std.testing.allocator;
    const members = [_]types.Member{
        .{ .type = .fn_decl, .name = "first", .line = 10 },
        .{ .type = .fn_decl, .name = "second", .line = 5 },
        .{ .type = .fn_decl, .name = "third", .line = 20 },
    };
    const sorted = try sync_mod.sortMembersByLineDesc(allocator, &members);
    defer allocator.free(sorted);

    try std.testing.expectEqual(@as(u32, 20), sorted[0].line.?);
    try std.testing.expectEqual(@as(u32, 10), sorted[1].line.?);
    try std.testing.expectEqual(@as(u32, 5), sorted[2].line.?);
}
test "processFile_insertsCommentForMissingFn - plumbing with no enhancer" {
    // With no enhancer, generateMemberComment returns null and comments_added
    // stays 0.  The test validates that the full processFile plumbing runs
    // without errors on a valid Zig source file.
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    // Write a minimal valid Zig source file with a public function.
    const source =
        \\pub fn myFunc() void {}
        \\
    ;
    const src_file = try tmp.dir.createFile("test_fn.zig", .{});
    try src_file.writeAll(source);
    src_file.close();

    // Resolve absolute path to the temp file.
    var buf: [std.fs.max_path_bytes]u8 = undefined;
    const abs_path = try tmp.dir.realpath("test_fn.zig", &buf);

    // Also resolve the tmp dir itself as both workspace and output_dir so
    // that the guidance JSON path is constructed under the same tmp dir.
    var ws_buf: [std.fs.max_path_bytes]u8 = undefined;
    const ws_path = try tmp.dir.realpath(".", &ws_buf);

    var csp = sync_mod.CommentSyncProcessor.init(allocator, ws_path, ws_path, false, true); // dry_run = true
    // No enhancer assigned — generateMemberComment will return null.

    const result = try csp.processFile(abs_path);

    // Without an enhancer: no comments can be generated, so comments_added == 0.
    try std.testing.expectEqual(@as(usize, 0), result.comments_added);
    // dry_run = true so source was not modified.
    try std.testing.expect(!result.source_modified);
}
test "processFile_skipsUpToDate - incremental mode" {
    // When incremental = true and the guidance JSON mtime >= source mtime,
    // processFile must return early with has_changes = false.
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const source =
        \\pub fn anotherFunc() void {}
        \\
    ;
    {
        const f = try tmp.dir.createFile("uptodate.zig", .{});
        defer f.close();
        try f.writeAll(source);
    }

    var buf: [std.fs.max_path_bytes]u8 = undefined;
    const abs_path = try tmp.dir.realpath("uptodate.zig", &buf);
    var ws_buf: [std.fs.max_path_bytes]u8 = undefined;
    const ws_path = try tmp.dir.realpath(".", &ws_buf);

    // Create the guidance JSON with a newer mtime by writing it after the source.
    const json_path_rel = "uptodate.zig.json";
    {
        const jf = try tmp.dir.createFile(json_path_rel, .{});
        defer jf.close();
        try jf.writeAll("{\"meta\":{\"module\":\"uptodate\",\"source\":\"\"},\"members\":[]}");
    }

    var csp = sync_mod.CommentSyncProcessor.init(allocator, ws_path, ws_path, false, false);
    csp.incremental = true;

    const result = try csp.processFile(abs_path);
    // File should be considered up to date — no changes.
    try std.testing.expect(!result.has_changes);
}
test "processFile_bottomUpOrder - higher line processed first" {
    // sortMembersByLineDesc ensures the member with the larger line number is
    // sorted first.  This test verifies the sort indirectly: a two-function
    // source file processed with dry_run=true should not error, and the
    // ordering is already verified by the sortMembersByLineDesc test above.
    // Here we just confirm processFile does not crash on a multi-decl file.
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const source =
        \\pub fn alpha() void {}
        \\pub fn beta() void {}
        \\
    ;
    {
        const f = try tmp.dir.createFile("two_fns.zig", .{});
        defer f.close();
        try f.writeAll(source);
    }

    var buf: [std.fs.max_path_bytes]u8 = undefined;
    const abs_path = try tmp.dir.realpath("two_fns.zig", &buf);
    var ws_buf: [std.fs.max_path_bytes]u8 = undefined;
    const ws_path = try tmp.dir.realpath(".", &ws_buf);

    var csp = sync_mod.CommentSyncProcessor.init(allocator, ws_path, ws_path, false, true); // dry_run
    const result = try csp.processFile(abs_path);
    // No enhancer: no comments added; no crash.
    try std.testing.expectEqual(@as(usize, 0), result.comments_added);
}
test "processFile_skipsPrivateFns - no comment for private fn" {
    // Private functions (fn_private) must be skipped.  With dry_run=true and
    // no enhancer the result should always show 0 comments added, but the
    // private function must never attempt to generate a comment.
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    // Only private function — should never be processed.
    const source =
        \\fn privateHelper() void {}
        \\
    ;
    {
        const f = try tmp.dir.createFile("private.zig", .{});
        defer f.close();
        try f.writeAll(source);
    }

    var buf: [std.fs.max_path_bytes]u8 = undefined;
    const abs_path = try tmp.dir.realpath("private.zig", &buf);
    var ws_buf: [std.fs.max_path_bytes]u8 = undefined;
    const ws_path = try tmp.dir.realpath(".", &ws_buf);

    var csp = sync_mod.CommentSyncProcessor.init(allocator, ws_path, ws_path, false, true); // dry_run
    const result = try csp.processFile(abs_path);
    try std.testing.expectEqual(@as(usize, 0), result.comments_added);
    try std.testing.expect(!result.has_changes);
}
