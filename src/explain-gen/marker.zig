//! Mtime-based change detection for explain-gen's incremental RALPH loop.
//!
//! ## Design
//!
//! The guidance JSON file's mtime IS the per-file marker.  When a provider
//! (Zig built-in, explain-gen-py, or any future language tool) finishes its
//! full test→lint→fmt→guidance cycle for a source file, it writes the JSON,
//! advancing its mtime.  explain-gen then uses a simple mtime comparison to
//! decide whether re-processing is needed:
//!
//!   source mtime > JSON mtime  →  stale, needs processing
//!   source mtime ≤ JSON mtime  →  fresh, skip
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
//! The Zig provider implements steps 1-4 inside explain-gen itself.
//! External providers (e.g. explain-gen-py) honour the same contract in their
//! own implementation.

const std = @import("std");

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
