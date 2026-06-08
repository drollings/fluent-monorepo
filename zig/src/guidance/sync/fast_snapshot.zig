//! Binary snapshot for warm-startup optimization in the guidance sync pipeline.
//!
//! `.guidance/.snap` records the state of every source file at the last
//! successful sync.  On the next `guidance gen` run:
//!  1. Load snapshot (mmap-friendly, O(1) parse per file).
//!  2. Compare git HEAD.  If HEAD matches, use stored hashes to skip the
//!     per-file stat-of-JSON entirely for unchanged files.
//!  3. Only stat and process files whose content_hash differs.
//!
//! ## Binary format (all integers little-endian)
//!
//! Header (64 bytes):
//!   magic         [4]u8   = "GFS\x01"
//!   version       u16     = 1
//!   flags         u16     = 0 (reserved)
//!   git_head      [40]u8  (hex SHA or zeroes if no git)
//!   file_count    u32
//!   written_at    i64     (Unix seconds)
//!   _padding      [6]u8
//!
//! Per-file record (variable length, file_count times):
//!   path_len              u16
//!   path                  [path_len]u8
//!   src_mtime_ns          i128  (16 bytes)
//!   content_hash          u64
//!   match_hash_count      u16
//!   match_hashes[]        for each:
//!     name_len  u16
//!     name      [name_len]u8
//!     hash_len  u16
//!     hash      [hash_len]u8   (hex SHA-256)

const std = @import("std");
const common = @import("common");
const MAGIC = "GFS\x01";
const VERSION: u16 = 1;
const HEADER_SIZE: usize = 64;

/// A single member's match_hash entry in the snapshot.
pub const MemberHash = struct {
    name: []const u8, // owned
    hash: []const u8, // owned (hex SHA-256)
};

/// Per-file snapshot record.
pub const FileSnap = struct {
    path: []const u8, // owned
    src_mtime_ns: i128,
    content_hash: u64,
    member_hashes: []MemberHash, // owned
};

/// In-memory representation of a `.guidance/.snap` file.
pub const FastSnapshot = struct {
    git_head: ?[40]u8,
    written_at: i64,
    files: []FileSnap, // owned
    allocator: std.mem.Allocator,
    /// O(1) path → index into `files`.
    index: std.StringHashMapUnmanaged(u32),

    pub fn init(allocator: std.mem.Allocator) FastSnapshot {
        return .{
            .git_head = null,
            .written_at = 0,
            .files = &.{},
            .allocator = allocator,
            .index = .empty,
        };
    }

    pub fn deinit(self: *FastSnapshot) void {
        for (self.files) |*f| freeFileSnap(self.allocator, f);
        self.allocator.free(self.files);
        self.index.deinit(self.allocator);
    }

    /// Write snapshot to `path` atomically (write to tmp file, then rename).
    pub fn write(self: *const FastSnapshot, path: []const u8) !void {
        const io = std.Io.Threaded.global_single_threaded.io();
        const allocator = self.allocator;

        // Build tmp path alongside the target.
        const dir = std.fs.path.dirname(path) orelse ".";
        const now_ns = std.Io.Timestamp.now(io, .real).nanoseconds;
        const rand: u64 = @as(u64, @bitCast(@as(i64, @truncate(now_ns)))) ^ @as(u64, @intCast(@intFromPtr(path.ptr)));
        const tmp_path = try std.fmt.allocPrint(allocator, "{s}/.{x}.tmp", .{ dir, rand });
        defer allocator.free(tmp_path);

        // Clean stale tmp files (older than 60 s) before writing.
        cleanStaleTmp(allocator, dir);

        const tmp_file = try std.Io.Dir.createFileAbsolute(io, tmp_path, .{});
        var write_failed = true;
        defer {
            tmp_file.close(io);
            if (write_failed) {
                std.Io.Dir.deleteFileAbsolute(io, tmp_path) catch {};
            }
        }

        var buf: [4096]u8 = undefined;
        var fw = tmp_file.writer(io, &buf);
        const w = &fw.interface;

        // --- Header (64 bytes) ---
        try w.writeAll(MAGIC);
        try w.writeInt(u16, VERSION, .little);
        try w.writeInt(u16, 0, .little); // flags
        if (self.git_head) |head| {
            try w.writeAll(&head);
        } else {
            const zeroes40 = [_]u8{0} ** 40;
            try w.writeAll(&zeroes40);
        }
        try w.writeInt(u32, @intCast(self.files.len), .little);
        try w.writeInt(i64, self.written_at, .little);
        const zeroes4 = [_]u8{0} ** 4;
        try w.writeAll(&zeroes4); // padding to 64 bytes (4+2+2+40+4+8+4=64)
        try w.flush();

        // --- Per-file records ---
        for (self.files) |*f| {
            const path_len: u16 = @intCast(@min(f.path.len, std.math.maxInt(u16)));
            try w.writeInt(u16, path_len, .little);
            try w.writeAll(f.path[0..path_len]);

            // src_mtime_ns as 16 bytes little-endian
            var mtime_bytes: [16]u8 = undefined;
            std.mem.writeInt(i128, &mtime_bytes, f.src_mtime_ns, .little);
            try w.writeAll(&mtime_bytes);

            try w.writeInt(u64, f.content_hash, .little);

            const mh_count: u16 = @intCast(@min(f.member_hashes.len, std.math.maxInt(u16)));
            try w.writeInt(u16, mh_count, .little);
            for (f.member_hashes[0..mh_count]) |mh| {
                const name_len: u16 = @intCast(@min(mh.name.len, std.math.maxInt(u16)));
                try w.writeInt(u16, name_len, .little);
                try w.writeAll(mh.name[0..name_len]);
                const hash_len: u16 = @intCast(@min(mh.hash.len, std.math.maxInt(u16)));
                try w.writeInt(u16, hash_len, .little);
                try w.writeAll(mh.hash[0..hash_len]);
            }
            try w.flush();
        }

        write_failed = false;

        // Rename atomically.
        std.Io.Dir.renameAbsolute(tmp_path, path, io) catch |err| {
            std.Io.Dir.deleteFileAbsolute(io, tmp_path) catch {};
            return err;
        };
    }

    /// Read a snapshot from `path`.  Returns null if the file is absent,
    /// corrupt, or has a wrong magic/version.
    pub fn read(allocator: std.mem.Allocator, path: []const u8) ?FastSnapshot {
        const io = std.Io.Threaded.global_single_threaded.io();
        const content = std.Io.Dir.cwd().readFileAlloc(
            io,
            path,
            allocator,
            .limited(64 * 1024 * 1024),
        ) catch return null;
        defer allocator.free(content);
        return parseBytes(allocator, content);
    }

    /// Look up the stored content_hash for `src_abs`.  Returns 0 if not found.
    pub fn lookupStoredHash(self: *const FastSnapshot, src_abs: []const u8) u64 {
        const idx = self.index.get(src_abs) orelse return 0;
        if (idx >= self.files.len) return 0;
        return self.files[idx].content_hash;
    }

    /// Look up the stored match_hash for a named member of `src_abs`.
    /// Returns null if not found.
    pub fn lookupMemberHash(
        self: *const FastSnapshot,
        src_abs: []const u8,
        member_name: []const u8,
    ) ?[]const u8 {
        const idx = self.index.get(src_abs) orelse return null;
        if (idx >= self.files.len) return null;
        const f = &self.files[idx];
        for (f.member_hashes) |mh| {
            if (std.mem.eql(u8, mh.name, member_name)) return mh.hash;
        }
        return null;
    }
};

// ---------------------------------------------------------------------------
// SnapshotBuilder
// ---------------------------------------------------------------------------

/// Accumulates per-file records during a sync run, then produces a FastSnapshot.
pub const SnapshotBuilder = struct {
    allocator: std.mem.Allocator,
    files: std.ArrayList(FileSnap),

    pub fn init(allocator: std.mem.Allocator) SnapshotBuilder {
        return .{
            .allocator = allocator,
            .files = .empty,
        };
    }

    pub fn deinit(self: *SnapshotBuilder) void {
        for (self.files.items) |*f| freeFileSnap(self.allocator, f);
        self.files.deinit(self.allocator);
    }

    /// Record one file's sync result.  Takes ownership of `member_hashes`
    /// and its elements; dupes `src_abs`.
    pub fn addFile(
        self: *SnapshotBuilder,
        src_abs: []const u8,
        src_mtime_ns: i128,
        content_hash: u64,
        member_hashes: []MemberHash,
    ) !void {
        const owned_path = try self.allocator.dupe(u8, src_abs);
        errdefer self.allocator.free(owned_path);
        try self.files.append(self.allocator, .{
            .path = owned_path,
            .src_mtime_ns = src_mtime_ns,
            .content_hash = content_hash,
            .member_hashes = member_hashes,
        });
    }

    /// Finalize into a FastSnapshot.  Caller owns; call deinit() when done.
    pub fn build(self: *SnapshotBuilder, git_head: ?[40]u8) !FastSnapshot {
        const allocator = self.allocator;
        const files = try self.files.toOwnedSlice(allocator);
        // self.files is now empty (ownership transferred).

        var index: std.StringHashMapUnmanaged(u32) = .empty;
        errdefer {
            index.deinit(allocator);
            for (files) |*f| freeFileSnap(allocator, f);
            allocator.free(files);
        }

        for (files, 0..) |*f, i| {
            try index.put(allocator, f.path, @intCast(i));
        }

        const io = std.Io.Threaded.global_single_threaded.io();
        return .{
            .git_head = git_head,
            .written_at = @as(i64, @intCast(@divTrunc(std.Io.Timestamp.now(io, .real).nanoseconds, std.time.ns_per_s))),
            .files = files,
            .allocator = allocator,
            .index = index,
        };
    }
};

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// // ---------------------------------------------------------------------------
fn freeFileSnap(allocator: std.mem.Allocator, f: *FileSnap) void {
    allocator.free(f.path);
    for (f.member_hashes) |mh| {
        allocator.free(mh.name);
        allocator.free(mh.hash);
    }
    allocator.free(f.member_hashes);
}

fn parseBytes(allocator: std.mem.Allocator, data: []const u8) ?FastSnapshot {
    if (data.len < HEADER_SIZE) return null;

    // Validate magic.
    if (!std.mem.eql(u8, data[0..4], MAGIC)) return null;

    const version = std.mem.readInt(u16, data[4..6], .little);
    if (version != VERSION) return null;

    // flags at bytes 6-7 (ignored for now).
    var git_head: ?[40]u8 = null;
    const head_bytes = data[8..48];
    const all_zero = for (head_bytes) |b| {
        if (b != 0) break false;
    } else true;
    if (!all_zero) {
        var gh: [40]u8 = undefined;
        @memcpy(&gh, head_bytes);
        git_head = gh;
    }

    const file_count = std.mem.readInt(u32, data[48..52], .little);
    const written_at = std.mem.readInt(i64, data[52..60], .little);
    // bytes 60-63: padding (skip).

    var pos: usize = HEADER_SIZE;

    var files = std.ArrayList(FileSnap).initCapacity(allocator, @min(file_count, 10_000)) catch return null;
    errdefer {
        for (files.items) |*f| freeFileSnap(allocator, f);
        files.deinit(allocator);
    }

    var i: u32 = 0;
    while (i < file_count) : (i += 1) {
        if (pos + 2 > data.len) return null;
        const path_len = std.mem.readInt(u16, data[pos..][0..2], .little);
        pos += 2;

        if (pos + path_len > data.len) return null;
        const path = allocator.dupe(u8, data[pos .. pos + path_len]) catch return null;
        pos += path_len;

        if (pos + 16 > data.len) {
            allocator.free(path);
            return null;
        }
        const src_mtime_ns = std.mem.readInt(i128, data[pos..][0..16], .little);
        pos += 16;

        if (pos + 8 > data.len) {
            allocator.free(path);
            return null;
        }
        const content_hash = std.mem.readInt(u64, data[pos..][0..8], .little);
        pos += 8;

        if (pos + 2 > data.len) {
            allocator.free(path);
            return null;
        }
        const mh_count = std.mem.readInt(u16, data[pos..][0..2], .little);
        pos += 2;

        var member_hashes = std.ArrayList(MemberHash).initCapacity(
            allocator,
            @min(mh_count, 1000),
        ) catch {
            allocator.free(path);
            return null;
        };
        errdefer {
            for (member_hashes.items) |mh| {
                allocator.free(mh.name);
                allocator.free(mh.hash);
            }
            member_hashes.deinit(allocator);
        }

        var j: u16 = 0;
        while (j < mh_count) : (j += 1) {
            if (pos + 2 > data.len) return null;
            const name_len = std.mem.readInt(u16, data[pos..][0..2], .little);
            pos += 2;
            if (pos + name_len > data.len) return null;
            const name = allocator.dupe(u8, data[pos .. pos + name_len]) catch return null;
            pos += name_len;

            if (pos + 2 > data.len) {
                allocator.free(name);
                return null;
            }
            const hash_len = std.mem.readInt(u16, data[pos..][0..2], .little);
            pos += 2;
            if (pos + hash_len > data.len) {
                allocator.free(name);
                return null;
            }
            const hash = allocator.dupe(u8, data[pos .. pos + hash_len]) catch {
                allocator.free(name);
                return null;
            };
            pos += hash_len;

            member_hashes.append(allocator, .{ .name = name, .hash = hash }) catch {
                allocator.free(name);
                allocator.free(hash);
                return null;
            };
        }

        files.append(allocator, .{
            .path = path,
            .src_mtime_ns = src_mtime_ns,
            .content_hash = content_hash,
            .member_hashes = member_hashes.toOwnedSlice(allocator) catch {
                for (member_hashes.items) |mh| {
                    allocator.free(mh.name);
                    allocator.free(mh.hash);
                }
                member_hashes.deinit(allocator);
                allocator.free(path);
                return null;
            },
        }) catch {
            for (member_hashes.items) |mh| {
                allocator.free(mh.name);
                allocator.free(mh.hash);
            }
            member_hashes.deinit(allocator);
            allocator.free(path);
            return null;
        };
    }

    var index: std.StringHashMapUnmanaged(u32) = .empty;
    const files_slice = files.toOwnedSlice(allocator) catch return null;
    for (files_slice, 0..) |*f, idx| {
        index.put(allocator, f.path, @intCast(idx)) catch {};
    }

    return FastSnapshot{
        .git_head = git_head,
        .written_at = written_at,
        .files = files_slice,
        .allocator = allocator,
        .index = index,
    };
}

/// Delete snapshot tmp files older than 60 seconds from `dir`.
fn cleanStaleTmp(allocator: std.mem.Allocator, dir_path: []const u8) void {
    const io = std.Io.Threaded.global_single_threaded.io();
    const now_s: i64 = @as(i64, @intCast(@divTrunc(std.Io.Timestamp.now(io, .real).nanoseconds, std.time.ns_per_s)));

    var dir = std.Io.Dir.openDirAbsolute(io, dir_path, .{ .iterate = true }) catch return;
    defer dir.close(io);

    var walker = dir.walk(allocator) catch return;
    defer walker.deinit();

    while (walker.next(io) catch null) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.basename, ".tmp")) continue;
        // Only remove files that look like our tmp files (hex prefix).
        if (entry.basename.len < 4) continue;

        const full_path = std.fs.path.join(allocator, &.{ dir_path, entry.path }) catch continue;
        defer allocator.free(full_path);

        const f = std.Io.Dir.openFileAbsolute(io, full_path, .{}) catch continue;
        const stat = f.stat(io) catch {
            f.close(io);
            continue;
        };
        f.close(io);

        const file_ts = @as(i64, @intCast(@divFloor(stat.mtime.nanoseconds, std.time.ns_per_s)));
        if (now_s - file_ts > 60) {
            std.Io.Dir.deleteFileAbsolute(io, full_path) catch {};
        }
    }
}

/// Run `git rev-parse HEAD` from `project_root`.
/// Returns null if not a git repo or git is unavailable.
/// Result is a [40]u8 value (no free needed).
pub fn getGitHead(project_root: []const u8, allocator: std.mem.Allocator) ?[40]u8 {
    const io = common.io.singleIo();

    const argv = [_][]const u8{ "git", "-C", project_root, "rev-parse", "HEAD" };
    const result = std.process.run(allocator, io, .{ .argv = &argv }) catch return null;
    defer {
        allocator.free(result.stdout);
        allocator.free(result.stderr);
    }

    switch (result.term) {
        .exited => |code| if (code != 0) return null,
        else => return null,
    }

    const trimmed = std.mem.trim(u8, result.stdout, " \t\r\n");
    if (trimmed.len < 40) return null;

    var head: [40]u8 = undefined;
    @memcpy(&head, trimmed[0..40]);
    return head;
}