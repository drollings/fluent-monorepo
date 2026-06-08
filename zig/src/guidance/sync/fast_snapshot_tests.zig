//! Tests for fast_snapshot.zig — binary snapshot round-trips, lookup helpers,
//! corrupt-file safety, and GPA leak detection.

const std = @import("std");
const snap = @import("fast_snapshot.zig");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a SnapshotBuilder with `n` files and build it.
fn buildSnapshot(allocator: std.mem.Allocator, n: usize) !snap.FastSnapshot {
    var builder = snap.SnapshotBuilder.init(allocator);
    // Do NOT errdefer builder.deinit() here — build() transfers ownership.
    for (0..n) |i| {
        const path = try std.fmt.allocPrint(allocator, "/tmp/test_file_{d}.zig", .{i});
        defer allocator.free(path);
        const mh = try allocator.dupe(snap.MemberHash, &[_]snap.MemberHash{
            .{
                .name = try allocator.dupe(u8, "funcA"),
                .hash = try allocator.dupe(u8, "aabbccdd00112233aabbccdd00112233aabbccdd"),
            },
        });
        try builder.addFile(path, @as(i128, @intCast(i)) * 1_000_000_000, @as(u64, i) + 1, mh);
    }
    return try builder.build(null);
}

// ---------------------------------------------------------------------------
// Round-trip test
// ---------------------------------------------------------------------------

test "FastSnapshot: round-trip write/read preserves all data" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    var snapshot = try buildSnapshot(allocator, 3);
    defer snapshot.deinit();

    const tmp_path = "/tmp/guidance_snap_test_roundtrip.snap";

    try snapshot.write(tmp_path);
    defer std.Io.Dir.deleteFileAbsolute(std.Io.Threaded.global_single_threaded.io(), tmp_path) catch {};

    var loaded = snap.FastSnapshot.read(allocator, tmp_path) orelse {
        return error.SnapshotReadFailed;
    };
    defer loaded.deinit();

    try std.testing.expectEqual(@as(usize, 3), loaded.files.len);

    for (0..3) |i| {
        const expected_path = try std.fmt.allocPrint(allocator, "/tmp/test_file_{d}.zig", .{i});
        defer allocator.free(expected_path);

        const f = loaded.files[i];
        try std.testing.expectEqualStrings(expected_path, f.path);
        try std.testing.expectEqual(@as(i128, @intCast(i)) * 1_000_000_000, f.src_mtime_ns);
        try std.testing.expectEqual(@as(u64, i) + 1, f.content_hash);
        try std.testing.expectEqual(@as(usize, 1), f.member_hashes.len);
        try std.testing.expectEqualStrings("funcA", f.member_hashes[0].name);
        try std.testing.expectEqualStrings(
            "aabbccdd00112233aabbccdd00112233aabbccdd",
            f.member_hashes[0].hash,
        );
    }
}

// ---------------------------------------------------------------------------
// lookupStoredHash
// ---------------------------------------------------------------------------

test "FastSnapshot: lookupStoredHash returns correct hash for known path" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    var snapshot = try buildSnapshot(allocator, 5);
    defer snapshot.deinit();

    // File 2 should have content_hash = 3 (i + 1 where i=2).
    const h = snapshot.lookupStoredHash("/tmp/test_file_2.zig");
    try std.testing.expectEqual(@as(u64, 3), h);
}

test "FastSnapshot: lookupStoredHash returns 0 for unknown path" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    var snapshot = try buildSnapshot(allocator, 2);
    defer snapshot.deinit();

    const h = snapshot.lookupStoredHash("/tmp/no_such_file.zig");
    try std.testing.expectEqual(@as(u64, 0), h);
}

// ---------------------------------------------------------------------------
// lookupMemberHash
// ---------------------------------------------------------------------------

test "FastSnapshot: lookupMemberHash returns hash for known member" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    var snapshot = try buildSnapshot(allocator, 1);
    defer snapshot.deinit();

    const h = snapshot.lookupMemberHash("/tmp/test_file_0.zig", "funcA");
    try std.testing.expect(h != null);
    try std.testing.expectEqualStrings("aabbccdd00112233aabbccdd00112233aabbccdd", h.?);
}

test "FastSnapshot: lookupMemberHash returns null for unknown member" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    var snapshot = try buildSnapshot(allocator, 1);
    defer snapshot.deinit();

    const h = snapshot.lookupMemberHash("/tmp/test_file_0.zig", "nonExistentFn");
    try std.testing.expect(h == null);
}

// ---------------------------------------------------------------------------
// Corrupt / wrong magic
// ---------------------------------------------------------------------------

test "FastSnapshot: read returns null for truncated file" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const io = std.Io.Threaded.global_single_threaded.io();
    const corrupt_path = "/tmp/guidance_snap_test_corrupt.snap";
    defer std.Io.Dir.deleteFileAbsolute(io, corrupt_path) catch {};

    // Write 30 bytes of garbage.
    {
        const f = try std.Io.Dir.createFileAbsolute(io, corrupt_path, .{});
        defer f.close(io);
        var buf: [32]u8 = undefined;
        var fw = f.writer(io, &buf);
        for (0..30) |_| try fw.interface.writeByte(0xAB);
        try fw.interface.flush();
    }

    const result = snap.FastSnapshot.read(allocator, corrupt_path);
    try std.testing.expect(result == null);
}

test "FastSnapshot: read returns null for wrong magic" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const io = std.Io.Threaded.global_single_threaded.io();
    const wrong_magic_path = "/tmp/guidance_snap_test_wrong_magic.snap";
    defer std.Io.Dir.deleteFileAbsolute(io, wrong_magic_path) catch {};

    // Write a full 64-byte header but with wrong magic.
    {
        const f = try std.Io.Dir.createFileAbsolute(io, wrong_magic_path, .{});
        defer f.close(io);
        var buf: [128]u8 = undefined;
        var fw = f.writer(io, &buf);
        const w = &fw.interface;
        try w.writeAll("BAD!"); // wrong magic
        for (0..60) |_| try w.writeByte(0); // rest of header
        try w.flush();
    }

    const result = snap.FastSnapshot.read(allocator, wrong_magic_path);
    try std.testing.expect(result == null);
}

// ---------------------------------------------------------------------------
// GPA leak check on read + deinit
// ---------------------------------------------------------------------------

test "FastSnapshot: no memory leaks on read + deinit" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    var snapshot = try buildSnapshot(allocator, 4);

    const tmp_path = "/tmp/guidance_snap_test_leak.snap";
    try snapshot.write(tmp_path);
    defer std.Io.Dir.deleteFileAbsolute(std.Io.Threaded.global_single_threaded.io(), tmp_path) catch {};
    snapshot.deinit();

    var loaded = snap.FastSnapshot.read(allocator, tmp_path) orelse return error.SnapshotReadFailed;
    loaded.deinit();
    // GPA deinit at scope exit will detect any leaks.
}

// ---------------------------------------------------------------------------
// SnapshotBuilder: empty snapshot
// ---------------------------------------------------------------------------

test "SnapshotBuilder: build from empty produces valid empty snapshot" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    var builder = snap.SnapshotBuilder.init(allocator);
    var snapshot = try builder.build(null);
    defer snapshot.deinit();

    try std.testing.expectEqual(@as(usize, 0), snapshot.files.len);
    try std.testing.expect(snapshot.git_head == null);
}

// ---------------------------------------------------------------------------
// git_head round-trip
// ---------------------------------------------------------------------------

test "FastSnapshot: git_head is preserved across write/read" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const io = std.Io.Threaded.global_single_threaded.io();

    var git_head: [40]u8 = undefined;
    @memset(&git_head, 'a');

    var builder = snap.SnapshotBuilder.init(allocator);
    var snapshot = try builder.build(git_head);
    defer snapshot.deinit();

    const tmp_path = "/tmp/guidance_snap_test_githead.snap";
    try snapshot.write(tmp_path);
    defer std.Io.Dir.deleteFileAbsolute(io, tmp_path) catch {};

    var loaded = snap.FastSnapshot.read(allocator, tmp_path) orelse return error.SnapshotReadFailed;
    defer loaded.deinit();

    try std.testing.expect(loaded.git_head != null);
    try std.testing.expectEqualSlices(u8, &git_head, &loaded.git_head.?);
}
