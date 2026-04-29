const std = @import("std");
const hash_mod = @import("hash.zig");

pub const FileLock = struct {
    lock_path: []const u8,
    file: ?std.Io.File,
    acquired: bool,
    allocator: std.mem.Allocator,

    pub fn acquire(source_file: []const u8, allocator: std.mem.Allocator) !FileLock {
        var hash_buf: [64]u8 = undefined;
        const hex = std.fmt.bytesToHex(hash_mod.blake3Hash(source_file), .lower);
        @memcpy(&hash_buf, &hex);
        const lock_name = hash_buf[0..16];

        const io = std.Io.Threaded.global_single_threaded.io();
        const lock_dir = ".guidance/locks";
        std.Io.Dir.cwd().createDirPath(io, lock_dir) catch {};

        const lock_path = try std.fmt.allocPrint(allocator, "{s}/{s}.lock", .{ lock_dir, lock_name });

        const f = std.Io.Dir.cwd().createFile(io, lock_path, .{ .exclusive = false }) catch |err| {
            if (err == error.PathDoesNotExist) {
                var dir = std.Io.Dir.cwd().openDir(io, lock_dir, .{}) catch
                    return FileLock{ .lock_path = lock_path, .file = null, .acquired = false, .allocator = allocator };
                const f2 = dir.createFile(io, lock_path[lock_dir.len + 1 ..], .{ .exclusive = true }) catch
                    return FileLock{ .lock_path = lock_path, .file = null, .acquired = false, .allocator = allocator };
                return FileLock{ .lock_path = lock_path, .file = f2, .acquired = true, .allocator = allocator };
            }
            return err;
        };

        const locked = posixLock(f);
        if (!locked) {
            f.close(io);
            return FileLock{ .lock_path = lock_path, .file = null, .acquired = false, .allocator = allocator };
        }

        return FileLock{ .lock_path = lock_path, .file = f, .acquired = true, .allocator = allocator };
    }

    pub fn release(self: *FileLock) void {
        const io = std.Io.Threaded.global_single_threaded.io();
        if (self.file) |f| {
            posixUnlock(f);
            f.close(io);
            self.file = null;
        }
        std.Io.Dir.cwd().deleteFile(io, self.lock_path) catch {};
        self.allocator.free(self.lock_path);
        self.acquired = false;
    }

    fn posixLock(f: std.Io.File) bool {
        std.posix.flock(f.handle, std.posix.LOCK.EX | std.posix.LOCK.NB) catch return false;
        return true;
    }

    fn posixUnlock(f: std.Io.File) void {
        std.posix.flock(f.handle, std.posix.LOCK.UN) catch {};
    }
};

const testing = std.testing;

test "FileLock acquire and release" {
    var lock = try FileLock.acquire("test_source_file.zig", testing.allocator);
    try testing.expect(lock.acquired);
    lock.release();
    try testing.expect(!lock.acquired);
}
