const std = @import("std");

pub const HashAlgorithm = enum {
    sha256,
    sha512,
    blake3,

    pub fn digestLength(self: HashAlgorithm) usize {
        return switch (self) {
            .sha256 => 32,
            .sha512 => 64,
            .blake3 => 32,
        };
    }
};

pub fn hashFile(
    allocator: std.mem.Allocator,
    path: []const u8,
    algorithm: HashAlgorithm,
) ![]const u8 {
    const file = try std.fs.cwd().openFile(path, .{});
    defer file.close();

    var buf: [65536]u8 = undefined;

    switch (algorithm) {
        .sha256 => {
            var hasher = std.crypto.hash.sha2.Sha256.init(.{});
            while (true) {
                const n = file.read(&buf) catch break;
                if (n == 0) break;
                hasher.update(buf[0..n]);
            }
            var out: [32]u8 = undefined;
            hasher.final(&out);
            const hex = std.fmt.bytesToHex(out, .lower);
            return allocator.dupe(u8, &hex);
        },
        .sha512 => {
            var hasher = std.crypto.hash.sha2.Sha512.init(.{});
            while (true) {
                const n = file.read(&buf) catch break;
                if (n == 0) break;
                hasher.update(buf[0..n]);
            }
            var out: [64]u8 = undefined;
            hasher.final(&out);
            const hex = std.fmt.bytesToHex(out, .lower);
            return allocator.dupe(u8, &hex);
        },
        .blake3 => {
            var hasher = std.crypto.hash.Blake3.init(.{});
            while (true) {
                const n = file.read(&buf) catch break;
                if (n == 0) break;
                hasher.update(buf[0..n]);
            }
            var out: [32]u8 = undefined;
            hasher.final(&out);
            const hex = std.fmt.bytesToHex(out, .lower);
            return allocator.dupe(u8, &hex);
        },
    }
}

pub const BatchHashResult = struct {
    path: []const u8,
    hash: ?[]const u8,
    error_msg: ?[]const u8,
};

pub fn hashBatch(
    allocator: std.mem.Allocator,
    paths: []const []const u8,
    algorithm: HashAlgorithm,
    progress_callback: ?*const fn (usize, usize) void,
) ![]BatchHashResult {
    var results = try allocator.alloc(BatchHashResult, paths.len);
    errdefer allocator.free(results);

    for (paths, 0..) |path, i| {
        if (progress_callback) |cb| cb(i, paths.len);

        results[i] = .{ .path = path, .hash = null, .error_msg = null };

        const hash = hashFile(allocator, path, algorithm) catch |err| {
            results[i].error_msg = try std.fmt.allocPrint(allocator, "{}", .{err});
            continue;
        };
        results[i].hash = hash;
    }

    return results;
}

pub fn hashString(
    allocator: std.mem.Allocator,
    data: []const u8,
    algorithm: HashAlgorithm,
) ![]const u8 {
    switch (algorithm) {
        .sha256 => {
            var out: [32]u8 = undefined;
            std.crypto.hash.sha2.Sha256.hash(data, &out, .{});
            const hex = std.fmt.bytesToHex(out, .lower);
            return allocator.dupe(u8, &hex);
        },
        .sha512 => {
            var out: [64]u8 = undefined;
            std.crypto.hash.sha2.Sha512.hash(data, &out, .{});
            const hex = std.fmt.bytesToHex(out, .lower);
            return allocator.dupe(u8, &hex);
        },
        .blake3 => {
            var out: [32]u8 = undefined;
            std.crypto.hash.Blake3.hash(data, &out, .{});
            const hex = std.fmt.bytesToHex(out, .lower);
            return allocator.dupe(u8, &hex);
        },
    }
}

pub fn blake3Hash(data: []const u8) [32]u8 {
    var out: [32]u8 = undefined;
    std.crypto.hash.Blake3.hash(data, &out, .{});
    return out;
}

pub fn blake3Hex(allocator: std.mem.Allocator, data: []const u8) ![]const u8 {
    const h = blake3Hash(data);
    const hex = std.fmt.bytesToHex(h, .lower);
    return allocator.dupe(u8, &hex);
}

pub const HashState = struct {
    algorithm: HashAlgorithm,
    sha256: ?std.crypto.hash.sha2.Sha256 = null,
    sha512: ?std.crypto.hash.sha2.Sha512 = null,
    blake3_h: ?std.crypto.hash.Blake3 = null,

    pub fn init(algorithm: HashAlgorithm) HashState {
        return switch (algorithm) {
            .sha256 => .{ .algorithm = algorithm, .sha256 = std.crypto.hash.sha2.Sha256.init(.{}) },
            .sha512 => .{ .algorithm = algorithm, .sha512 = std.crypto.hash.sha2.Sha512.init(.{}) },
            .blake3 => .{ .algorithm = algorithm, .blake3_h = std.crypto.hash.Blake3.init(.{}) },
        };
    }

    pub fn update(self: *HashState, data: []const u8) void {
        switch (self.algorithm) {
            .sha256 => self.sha256.?.update(data),
            .sha512 => self.sha512.?.update(data),
            .blake3 => self.blake3_h.?.update(data),
        }
    }

    pub fn final(self: *HashState, allocator: std.mem.Allocator) ![]const u8 {
        switch (self.algorithm) {
            .sha256 => {
                var out: [32]u8 = undefined;
                self.sha256.?.final(&out);
                const hex = std.fmt.bytesToHex(out, .lower);
                return allocator.dupe(u8, &hex);
            },
            .sha512 => {
                var out: [64]u8 = undefined;
                self.sha512.?.final(&out);
                const hex = std.fmt.bytesToHex(out, .lower);
                return allocator.dupe(u8, &hex);
            },
            .blake3 => {
                var out: [32]u8 = undefined;
                self.blake3_h.?.final(&out);
                const hex = std.fmt.bytesToHex(out, .lower);
                return allocator.dupe(u8, &hex);
            },
        }
    }
};

const testing = std.testing;

test "hashString: sha256" {
    const result = try hashString(testing.allocator, "hello", .sha256);
    defer testing.allocator.free(result);
    try testing.expectEqual(@as(usize, 64), result.len);
}

test "hashString: blake3" {
    const result = try hashString(testing.allocator, "hello", .blake3);
    defer testing.allocator.free(result);
    try testing.expectEqual(@as(usize, 64), result.len);
}

test "blake3Hash: returns 32 bytes" {
    const h = blake3Hash("hello");
    try testing.expectEqual(@as(usize, 32), h.len);
}

test "HashState: incremental hashing matches single-pass" {
    var state = HashState.init(.sha256);
    state.update("hello");
    state.update(" ");
    state.update("world");
    const result = try state.final(testing.allocator);
    defer testing.allocator.free(result);

    var single = HashState.init(.sha256);
    single.update("hello world");
    const expected = try single.final(testing.allocator);
    defer testing.allocator.free(expected);

    try testing.expectEqualStrings(expected, result);
}

test "HashAlgorithm: digestLength" {
    try testing.expectEqual(@as(usize, 32), HashAlgorithm.sha256.digestLength());
    try testing.expectEqual(@as(usize, 64), HashAlgorithm.sha512.digestLength());
    try testing.expectEqual(@as(usize, 32), HashAlgorithm.blake3.digestLength());
}
