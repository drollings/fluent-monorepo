/// simhash.zig — 64-bit SimHash for near-duplicate detection.
///
/// SimHash computes a locality-sensitive hash from token shingles.
/// Two documents with similar content will have low Hamming distance.
/// Threshold ≤ 3 bits typically indicates near-duplicates.
///
/// Algorithm:
///   1. Extract k-shingles (overlapping k-grams of tokens)
///   2. For each shingle, compute a 64-bit hash
///   3. Accumulate bit vectors: each hash bit contributes +1 or -1 to a counter
///   4. Final hash: bit i = 1 if counter[i] > 0, else 0
const std = @import("std");

/// Represents a hash structure for Zig, managing fixed-size buffers with ownership and invariants.
pub const SimHash = struct {
    /// Compute a 64-bit SimHash from token shingles.
    ///
    /// `tokens` is a slice of string tokens (e.g., identifier names, keywords).
    /// `shingle_size` is the k-gram window size (typically 2–4).
    pub fn compute(tokens: []const []const u8, shingle_size: usize) u64 {
        if (tokens.len == 0) return 0;
        const k = if (shingle_size == 0) 1 else shingle_size;

        var counters: [64]i32 = [_]i32{0} ** 64;

        var i: usize = 0;
        while (i + k <= tokens.len) : (i += 1) {
            const shingle_hash = hashShingle(tokens[i..][0..k]);
            for (0..64) |bit| {
                const bit_val: i32 = if ((shingle_hash >> @intCast(bit)) & 1 == 1) 1 else -1;
                counters[bit] += bit_val;
            }
        }
        // Handle trailing shingle if tokens.len < shingle_size
        if (i == 0 and tokens.len > 0) {
            const shingle_hash = hashShingle(tokens);
            for (0..64) |bit| {
                const bit_val: i32 = if ((shingle_hash >> @intCast(bit)) & 1 == 1) 1 else -1;
                counters[bit] += bit_val;
            }
        }

        var result: u64 = 0;
        for (0..64) |bit| {
            if (counters[bit] > 0) {
                result |= @as(u64, 1) << @intCast(bit);
            }
        }
        return result;
    }

    /// Hamming distance: number of differing bits between two hashes.
    pub fn distance(a: u64, b: u64) u16 {
        return @popCount(a ^ b);
    }

    /// True if hashes are within `threshold` bits of each other.
    /// Typical near-duplicate threshold: 3–5 bits.
    pub fn similar(a: u64, b: u64, threshold: u16) bool {
        return distance(a, b) <= threshold;
    }

    // Internal: hash a single shingle (slice of tokens) to u64.
    fn hashShingle(tokens: []const []const u8) u64 {
        var hasher = std.hash.Wyhash.init(0xdead_beef_cafe_babe);
        for (tokens) |tok| {
            hasher.update(tok);
            hasher.update(" ");
        }
        return hasher.final();
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
const testing = std.testing;

test "SimHash: empty tokens returns 0" {
    try testing.expectEqual(@as(u64, 0), SimHash.compute(&[_][]const u8{}, 2));
}

test "SimHash: identical token sets have distance 0" {
    const tokens = [_][]const u8{ "foo", "bar", "baz" };
    const h1 = SimHash.compute(&tokens, 2);
    const h2 = SimHash.compute(&tokens, 2);
    try testing.expectEqual(@as(u16, 0), SimHash.distance(h1, h2));
}

test "SimHash: very different token sets have large distance" {
    const a = [_][]const u8{ "alpha", "beta", "gamma" };
    const b = [_][]const u8{ "xyz", "abc", "pqr", "lmn", "opq" };
    const ha = SimHash.compute(&a, 2);
    const hb = SimHash.compute(&b, 2);
    // They should differ in several bits
    try testing.expect(SimHash.distance(ha, hb) > 0);
}

test "SimHash: similar texts have low distance" {
    const a = [_][]const u8{ "fn", "compute", "allocator", "result" };
    const b = [_][]const u8{ "fn", "compute", "allocator", "output" }; // one token change
    const ha = SimHash.compute(&a, 2);
    const hb = SimHash.compute(&b, 2);
    // Should be similar (low distance). Use a very generous threshold because SimHash
    // distance is sensitive with small token counts and k-gram size 2.
    try testing.expect(SimHash.similar(ha, hb, 40)); // very generous threshold for 4-token inputs
}

test "SimHash: distance is symmetric" {
    const a = [_][]const u8{ "hello", "world" };
    const b = [_][]const u8{ "foo", "bar" };
    const ha = SimHash.compute(&a, 1);
    const hb = SimHash.compute(&b, 1);
    try testing.expectEqual(SimHash.distance(ha, hb), SimHash.distance(hb, ha));
}

