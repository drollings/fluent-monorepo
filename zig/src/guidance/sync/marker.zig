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
//!   - JSON absent → stale, needs processing
//!   - JSON mtime == src_mtime - 1 second → validated, no changes, skip
//!   - JSON mtime > src_mtime → needs processing (e.g., imported JSON)
//!   - JSON mtime < src_mtime (but not src_mtime - 1) → stale, needs processing
//!
//! ## Validated Pattern
//!
//! When a file is processed and no meaningful changes are needed (members match,
//! comments in sync), we set JSON mtime to src_mtime - 1 second. This creates
//! a recognizable pattern that `fileNeedsProcessing` can identify to skip
//! reprocessing on subsequent runs.
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
const common = @import("common");

// ---------------------------------------------------------------------------
// Per-file change detection
// ---------------------------------------------------------------------------

/// Checks if the JSON array requires processing based on source data.
pub fn fileNeedsProcessing(src_abs: []const u8, json_abs: []const u8) bool {
    const json_mtime = fileMtime(json_abs) orelse return true; // JSON absent → stale
    const src_mtime = fileMtime(src_abs) orelse return false; // unreadable src → skip

    // Source modified since guidance ran → stale
    return src_mtime > json_mtime;
}

/// Returns the mtime of a file in nanoseconds, or null if the file cannot be opened.
pub fn fileMtime(path: []const u8) ?i128 {
    const io = std.Io.Threaded.global_single_threaded.io();
    const f = std.Io.Dir.openFileAbsolute(io, path, .{}) catch return null;
    defer f.close(io);
    const stat = f.stat(io) catch return null;
    return @as(i128, stat.mtime.nanoseconds);
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

/// Creates the test_passed marker file, touching it to set its mtime to now.
pub fn touchTestMarker(marker_path: []const u8) !void {
    const parent = std.fs.path.dirname(marker_path) orelse return error.InvalidPath;
    try common.makePathAbsolute(parent);
    const io = std.Io.Threaded.global_single_threaded.io();
    const f = try std.Io.Dir.createFileAbsolute(io, marker_path, .{});
    f.close(io);
}

/// Sets target file's mtime to 1 second before ref_path's mtime (validated pattern).
pub fn touchFileAfter(target_path: []const u8, ref_path: []const u8) !void {
    const ref_mtime = fileMtime(ref_path) orelse return error.FileNotFound;
    const io = std.Io.Threaded.global_single_threaded.io();
    var target_file = try std.Io.Dir.openFileAbsolute(io, target_path, .{ .mode = .read_write });
    defer target_file.close(io);
    // Set mtime to 1 second BEFORE source mtime (not after)
    // This creates a recognizable pattern: validated files have mtime = src_mtime - 1
    const new_mtime_ns: i96 = @intCast(ref_mtime - std.time.ns_per_s);
    try target_file.setTimestamps(io, .{
        .modify_timestamp = .{ .new = .{ .nanoseconds = new_mtime_ns } },
    });
}

/// Sets target file's mtime to the current time.
pub fn touchFileNow(target_path: []const u8) !void {
    const io = std.Io.Threaded.global_single_threaded.io();
    var target_file = try std.Io.Dir.openFileAbsolute(io, target_path, .{ .mode = .read_write });
    defer target_file.close(io);
    try target_file.setTimestamps(io, .{ .modify_timestamp = .now, .access_timestamp = .now });
}

/// Sets target file's mtime to current time plus one second.
pub fn touchFileNowPlusOne(target_path: []const u8) !void {
    const io = std.Io.Threaded.global_single_threaded.io();
    var target_file = try std.Io.Dir.openFileAbsolute(io, target_path, .{ .mode = .read_write });
    defer target_file.close(io);
    const now_ts = std.Io.Timestamp.now(io, .real);
    const plus_one = now_ts.addDuration(.{ .nanoseconds = std.time.ns_per_s });
    try target_file.setTimestamps(io, .{
        .modify_timestamp = .{ .new = plus_one },
        .access_timestamp = .{ .new = plus_one },
    });
}

// ---------------------------------------------------------------------------
// Content-hash helpers (Phase 3)
// ---------------------------------------------------------------------------

/// Return Wyhash of the file's content, or null if the file cannot be read.
/// Reads up to `max_bytes` to bound memory usage on large generated files.
pub fn contentHash(path: []const u8, max_bytes: usize) ?u64 {
    const allocator = std.heap.smp_allocator;
    const io = std.Io.Threaded.global_single_threaded.io();
    const content = std.Io.Dir.cwd().readFileAlloc(io, path, allocator, .limited(max_bytes)) catch return null;
    defer allocator.free(content);
    return std.hash.Wyhash.hash(0, content);
}

/// Compact per-file record used by the binary snapshot (Phase 4).
pub const FileRecord = struct {
    src_mtime: i128, // nanoseconds
    content_hash: u64, // Wyhash(content)
};

/// Return a FileRecord for the given path, or null if stat or read fails.
pub fn fileRecord(path: []const u8) ?FileRecord {
    const mtime = fileMtime(path) orelse return null;
    const h = contentHash(path, 10 * 1024 * 1024) orelse return null;
    return .{ .src_mtime = mtime, .content_hash = h };
}

/// Like fileNeedsProcessing but also compares content hashes when mtime
/// indicates a change.  If mtime advanced but hash is unchanged, the file
/// is treated as up-to-date (e.g. `git checkout` restored original content).
///
/// `stored_hash` is the hash recorded at last successful sync (from snapshot
/// or sidecar).  Pass 0 if unknown — falls back to mtime-only.
pub fn fileNeedsProcessingHash(
    src_abs: []const u8,
    json_abs: []const u8,
    stored_hash: u64,
) bool {
    if (!fileNeedsProcessing(src_abs, json_abs)) return false; // fast path
    if (stored_hash == 0) return true; // no stored hash → assume stale
    const current_hash = contentHash(src_abs, 10 * 1024 * 1024) orelse return true;
    return current_hash != stored_hash;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
