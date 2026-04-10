//! Tests for simhash.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const simhash_mod = @import("simhash.zig");

test "embeddingHash: short embedding (shorter than DIMS) doesn't panic" {
    const short: [10]f32 = [_]f32{0.5} ** 10;
    _ = simhash_mod.embeddingHash(&short);
}
