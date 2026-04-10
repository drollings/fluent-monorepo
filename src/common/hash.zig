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

    return switch (algorithm) {
        .sha256 => hashFileGeneric(allocator, &buf, file, std.crypto.hash.sha2.Sha256.init(.{})),
        .sha512 => hashFileGeneric(allocator, &buf, file, std.crypto.hash.sha2.Sha512.init(.{})),
        .blake3 => hashFileGeneric(allocator, &buf, file, std.crypto.hash.Blake3.init(.{})),
    };
}

fn hashFileGeneric(
    allocator: std.mem.Allocator,
    buf: *[65536]u8,
    file: std.fs.File,
    hasher: anytype,
) ![]const u8 {
    const H = @TypeOf(hasher);
    var h = hasher;
    while (true) {
        const n = file.read(buf) catch break;
        if (n == 0) break;
        h.update(buf[0..n]);
    }
    var out: [H.digest_length]u8 = undefined;
    h.final(&out);
    const hex = std.fmt.bytesToHex(out, .lower);
    return allocator.dupe(u8, &hex);
}

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

/// FNV-1a 64-bit hash — non-cryptographic, stable, fast, suitable for cache keys.
/// Uses the standard FNV-1a offset basis and prime.
pub fn fnv1a64(input: []const u8) u64 {
    const FNV_OFFSET: u64 = 14695981039346656037;
    const FNV_PRIME: u64 = 1099511628211;
    var h: u64 = FNV_OFFSET;
    for (input) |byte| {
        h ^= byte;
        h *%= FNV_PRIME;
    }
    return h;
}

/// Converts a byte slice into a Zig hash using the provided algorithm.
pub fn hashString(
    allocator: std.mem.Allocator,
    data: []const u8,
    algorithm: HashAlgorithm,
) ![]const u8 {
    return switch (algorithm) {
        .sha256 => computeHashHex(allocator, data, std.crypto.hash.sha2.Sha256),
        .sha512 => computeHashHex(allocator, data, std.crypto.hash.sha2.Sha512),
        .blake3 => computeHashHex(allocator, data, std.crypto.hash.Blake3),
    };
}

fn computeHashHex(allocator: std.mem.Allocator, data: []const u8, HashType: type) ![]const u8 {
    var out: [HashType.digest_length]u8 = undefined;
    HashType.hash(data, &out, .{});
    const hex = std.fmt.bytesToHex(out, .lower);
    return allocator.dupe(u8, &hex);
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
        return switch (self.algorithm) {
            .sha256 => finalizeHashHex(allocator, &self.sha256.?),
            .sha512 => finalizeHashHex(allocator, &self.sha512.?),
            .blake3 => finalizeHashHex(allocator, &self.blake3_h.?),
        };
    }
};

fn finalizeHashHex(allocator: std.mem.Allocator, hasher: anytype) ![]const u8 {
    const H = @TypeOf(hasher.*);
    var out: [H.digest_length]u8 = undefined;
    hasher.final(&out);
    const hex = std.fmt.bytesToHex(out, .lower);
    return allocator.dupe(u8, &hex);
}

// =============================================================================
// Tests
// =============================================================================
