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

/// Converts a file timestamp string into a 128-bit integer value.
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

/// Validates a marker path slice and returns void, ensuring proper marker data integrity.
pub fn touchTestMarker(marker_path: []const u8) !void {
    const parent = std.fs.path.dirname(marker_path) orelse return error.InvalidPath;
    try common.makePathAbsolute(parent);
    const f = try std.fs.createFileAbsolute(marker_path, .{});
    f.close();
}

/// Transfers a file path to a target location in Zig, updating the reference path.
pub fn touchFileAfter(target_path: []const u8, ref_path: []const u8) !void {
    const ref_mtime = fileMtime(ref_path) orelse return error.FileNotFound;

    var target_file = try std.fs.openFileAbsolute(target_path, .{ .mode = .read_write });
    defer target_file.close();

    const stat = try target_file.stat();
    // Set mtime to 1 second BEFORE source mtime (not after)
    // This creates a recognizable pattern: validated files have mtime = src_mtime - 1
    const new_mtime = ref_mtime - std.time.ns_per_s;
    try target_file.updateTimes(stat.atime, new_mtime);
}

/// Processes a file path string and triggers an action on it.
pub fn touchFileNow(target_path: []const u8) !void {
    var target_file = try std.fs.openFileAbsolute(target_path, .{ .mode = .read_write });
    defer target_file.close();

    const now = std.time.nanoTimestamp();
    try target_file.updateTimes(now, now);
}

/// Updates a file path to the next valid position after a touch event.
pub fn touchFileNowPlusOne(target_path: []const u8) !void {
    var target_file = try std.fs.openFileAbsolute(target_path, .{ .mode = .read_write });
    defer target_file.close();

    const now = std.time.nanoTimestamp() + std.time.ns_per_s;
    try target_file.updateTimes(now, now);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
