//! Mtime-based change detection for guidance's incremental RALPH loop.
//!
//! ## Design
//!
//! The guidance JSON file's mtime IS the per-file marker.  When a provider
//! (Zig built-in, guidance-py, or any future language tool) finishes its
//! full test→lint→fmt→guidance cycle for a source file, it writes the JSON,
//! advancing its mtime.  guidance then uses a simple mtime comparison to
//! decide whether re-processing is needed:
//!
//!   source mtime > JSON mtime  →  stale, needs processing
//!   source mtime ≤ JSON mtime  →  fresh, skip
//!
//! ## Test marker
//!
//! A separate marker file `.guidance/.marks/test_passed` records the last
//! successful test run.  If no source file is newer than this marker, tests
//! can be skipped.  This enables fast incremental runs of lint→fmt→guidance
//! without re-running the full test suite.
//!
//! ## Provider contract
//!
//! Each provider (built-in or external) must:
//!   1. Run the language test suite (once, whole suite).
//!   2. Run lint on the file.
//!   3. Run the formatter on the file (must precede AST parse so line numbers
//!      in the JSON reflect the final formatted source).
//!   4. Generate or update the guidance JSON.
//!
//! The Zig provider implements steps 1-4 inside guidance itself.
//! External providers (e.g. guidance-py) honour the same contract in their
//! own implementation.

const std = @import("std");
const llm = @import("common");

// ---------------------------------------------------------------------------
// Per-file change detection
// ---------------------------------------------------------------------------

/// Returns true when `src_abs` needs to be (re-)processed.
///
/// A file is stale when its guidance JSON is absent or older than the source.
/// `json_abs` is the absolute path to the expected `.json` guidance file.
pub fn fileNeedsProcessing(src_abs: []const u8, json_abs: []const u8) bool {
    const json_mtime = fileMtime(json_abs) orelse return true; // JSON absent → stale
    const src_mtime = fileMtime(src_abs) orelse return false; // unreadable src → skip
    return src_mtime > json_mtime;
}

/// Return the mtime (nanoseconds since epoch) of `path`, or null when the
/// file is absent or cannot be stat'd.
pub fn fileMtime(path: []const u8) ?i128 {
    const f = std.fs.openFileAbsolute(path, .{}) catch return null;
    defer f.close();
    const stat = f.stat() catch return null;
    return stat.mtime;
}

// ---------------------------------------------------------------------------
// Test marker for skip-test optimization
// ---------------------------------------------------------------------------

/// Default test marker path relative to guidance directory.
pub const TEST_MARKER_NAME = "test_passed";

/// Build the absolute path to the test_passed marker file.
/// Returns an owned allocation; caller must free.
pub fn testMarkerPath(allocator: std.mem.Allocator, guidance_root: []const u8) ![]const u8 {
    return std.fs.path.join(allocator, &.{ guidance_root, ".marks", TEST_MARKER_NAME });
}

/// Check if tests can be skipped because the test_passed marker is at least
/// as new as all source files in `src_files`.
///
/// Returns `true` if:
///   - The test_passed marker exists
///   - AND no source file has mtime > marker mtime
///
/// Returns `false` if:
///   - Marker doesn't exist
///   - OR any source file is newer than the marker
///   - OR any source file can't be stat'd
pub fn testsCanBeSkipped(marker_path: []const u8, src_files: []const []const u8) bool {
    const marker_mtime = fileMtime(marker_path) orelse return false;
    for (src_files) |src| {
        const src_mtime = fileMtime(src) orelse return false;
        if (src_mtime > marker_mtime) return false;
    }
    return true;
}

/// Create or touch the test_passed marker file to record a successful test run.
/// Creates parent `.marks/` directory if needed.
pub fn touchTestMarker(marker_path: []const u8) !void {
    const parent = std.fs.path.dirname(marker_path) orelse return error.InvalidPath;
    try llm.makePathAbsolute(parent);
    const f = try std.fs.createFileAbsolute(marker_path, .{});
    f.close();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "fileNeedsProcessing: absent JSON → stale" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "foo.zig" });
    defer std.testing.allocator.free(src_abs);
    {
        const f = try std.fs.createFileAbsolute(src_abs, .{});
        f.close();
    }
    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "foo.zig.json" });
    defer std.testing.allocator.free(json_abs);

    // JSON does not exist → stale.
    try std.testing.expect(fileNeedsProcessing(src_abs, json_abs));
}

test "fileNeedsProcessing: JSON written after source → fresh" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const src_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "foo.zig" });
    defer std.testing.allocator.free(src_abs);
    {
        const f = try std.fs.createFileAbsolute(src_abs, .{});
        f.close();
    }

    const json_abs = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "foo.zig.json" });
    defer std.testing.allocator.free(json_abs);

    // Create JSON after source so its mtime is >= source mtime.
    {
        const f = try std.fs.createFileAbsolute(json_abs, .{});
        f.close();
    }

    try std.testing.expect(!fileNeedsProcessing(src_abs, json_abs));
}

test "testsCanBeSkipped: no marker → false" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const marker = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, ".marks", TEST_MARKER_NAME });
    defer std.testing.allocator.free(marker);

    const src = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.zig" });
    defer std.testing.allocator.free(src);
    {
        const f = try std.fs.createFileAbsolute(src, .{});
        f.close();
    }

    const files = [_][]const u8{src};
    try std.testing.expect(!testsCanBeSkipped(marker, &files));
}

test "testsCanBeSkipped: marker newer than source → true" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const src = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.zig" });
    defer std.testing.allocator.free(src);
    {
        const f = try std.fs.createFileAbsolute(src, .{});
        f.close();
    }

    const marker = try testMarkerPath(std.testing.allocator, tmp_path);
    defer std.testing.allocator.free(marker);
    try touchTestMarker(marker);

    const files = [_][]const u8{src};
    try std.testing.expect(testsCanBeSkipped(marker, &files));
}

test "testsCanBeSkipped: source newer than marker → false" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    const marker = try testMarkerPath(std.testing.allocator, tmp_path);
    defer std.testing.allocator.free(marker);
    try touchTestMarker(marker);

    // Create source AFTER marker (simulate edit)
    std.Thread.sleep(1_000_000); // 1ms to ensure mtime difference
    const src = try std.fs.path.join(std.testing.allocator, &.{ tmp_path, "test.zig" });
    defer std.testing.allocator.free(src);
    {
        const f = try std.fs.createFileAbsolute(src, .{});
        f.close();
    }

    const files = [_][]const u8{src};
    try std.testing.expect(!testsCanBeSkipped(marker, &files));
}
