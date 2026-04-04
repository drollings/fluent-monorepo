/// hash.zig — Generic cryptographic hashing utilities
///
/// Provides allocator-friendly SHA-256 helpers and a content+model hash
/// suitable for embedding cache keys.  All functions are free of
/// guidance-domain types so they can be reused by any Zig tool.
const std = @import("std");

/// Converts input data into a SHA256 hexadecimal string using an allocator.
pub fn sha256Hex(allocator: std.mem.Allocator, data: []const u8) ![]const u8 {
    var hash_out: [32]u8 = undefined;
    std.crypto.hash.sha2.Sha256.hash(data, &hash_out, .{});
    const hex = std.fmt.bytesToHex(hash_out, .lower);
    const result = try allocator.alloc(u8, hex.len);
    @memcpy(result, &hex);
    return result;
}

/// Computes a 16-bit hash combining content and model data.
pub fn contentHashWithModel(content: []const u8, model: []const u8) [16]u8 {
    var hasher = std.crypto.hash.sha2.Sha256.init(.{});
    hasher.update(model);
    hasher.update("\x00");
    hasher.update(content);
    var digest: [32]u8 = undefined;
    hasher.final(&digest);

    var result: [16]u8 = undefined;
    const hex_chars = "0123456789abcdef";
    for (0..8) |i| {
        result[i * 2] = hex_chars[digest[i] >> 4];
        result[i * 2 + 1] = hex_chars[digest[i] & 0x0f];
    }
    return result;
}

// =============================================================================
// Multi-algorithm hashing — from coral/src/common/hash.zig
// =============================================================================

/// Defines a hash algorithm with fixed-size outputs; managed via ownership and not thread-safe.
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

/// Computes a hash for a file using the provided allocator and algorithm, returning the resulting hash value.
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

/// Manages batch hash results with fixed-size buffers; owned by the caller; ensures consistent state across operations.
pub const BatchHashResult = struct {
    path: []const u8,
    hash: ?[]const u8,
    error_msg: ?[]const u8,
};

/// Processes multiple paths with a hashing algorithm, returning results via callback.
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

/// Converts a byte slice into a Zig hash using the provided algorithm.
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

/// Computes a 32-byte Blake3 hash from the provided data slice.
pub fn blake3Hash(data: []const u8) [32]u8 {
    var out: [32]u8 = undefined;
    std.crypto.hash.Blake3.hash(data, &out, .{});
    return out;
}

/// Converts input data into a hexadecimal string using the Blake3 algorithm.
pub fn blake3Hex(allocator: std.mem.Allocator, data: []const u8) ![]const u8 {
    const h = blake3Hash(data);
    const hex = std.fmt.bytesToHex(h, .lower);
    return allocator.dupe(u8, &hex);
}

/// Manages hash state with fixed-size buffers; owned by the caller; ensures consistent key-value mapping.
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

    /// Finalise the hash and return an allocator-owned hex string; caller must free.
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

// =============================================================================
// Tests
// =============================================================================

test "sha256Hex produces correct length" {
    const result = try sha256Hex(std.testing.allocator, "test");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 64), result.len);
}

test "sha256Hex is deterministic" {
    const h1 = try sha256Hex(std.testing.allocator, "hello");
    defer std.testing.allocator.free(h1);
    const h2 = try sha256Hex(std.testing.allocator, "hello");
    defer std.testing.allocator.free(h2);
    try std.testing.expectEqualSlices(u8, h1, h2);
}

test "sha256Hex differs for different inputs" {
    const h1 = try sha256Hex(std.testing.allocator, "foo");
    defer std.testing.allocator.free(h1);
    const h2 = try sha256Hex(std.testing.allocator, "bar");
    defer std.testing.allocator.free(h2);
    try std.testing.expect(!std.mem.eql(u8, h1, h2));
}

test "contentHashWithModel is deterministic and model-sensitive" {
    const h1 = contentHashWithModel("text", "model-a");
    const h2 = contentHashWithModel("text", "model-a");
    const h3 = contentHashWithModel("text", "model-b");
    try std.testing.expectEqualSlices(u8, &h1, &h2);
    try std.testing.expect(!std.mem.eql(u8, &h1, &h3));
}

test "hashString: sha256" {
    const result = try hashString(std.testing.allocator, "hello", .sha256);
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 64), result.len);
}

test "hashString: blake3" {
    const result = try hashString(std.testing.allocator, "hello", .blake3);
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 64), result.len);
}

test "blake3Hash returns 32 bytes" {
    const h = blake3Hash("hello");
    try std.testing.expectEqual(@as(usize, 32), h.len);
}

test "blake3Hex produces 64-char hex" {
    const h = try blake3Hex(std.testing.allocator, "hello");
    defer std.testing.allocator.free(h);
    try std.testing.expectEqual(@as(usize, 64), h.len);
}

test "HashState incremental matches single-pass sha256" {
    var state = HashState.init(.sha256);
    state.update("hello");
    state.update(" ");
    state.update("world");
    const inc = try state.final(std.testing.allocator);
    defer std.testing.allocator.free(inc);

    var single = HashState.init(.sha256);
    single.update("hello world");
    const one = try single.final(std.testing.allocator);
    defer std.testing.allocator.free(one);

    try std.testing.expectEqualStrings(one, inc);
}

test "HashAlgorithm digestLength" {
    try std.testing.expectEqual(@as(usize, 32), HashAlgorithm.sha256.digestLength());
    try std.testing.expectEqual(@as(usize, 64), HashAlgorithm.sha512.digestLength());
    try std.testing.expectEqual(@as(usize, 32), HashAlgorithm.blake3.digestLength());
}










