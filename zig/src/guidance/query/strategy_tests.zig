//! Tests for strategy.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const strategy_mod = @import("strategy.zig");

test "looksLikeIdentifier: single camelCase identifier" {
    try std.testing.expect(strategy_mod.looksLikeIdentifier("cmdExplain"));
    try std.testing.expect(strategy_mod.looksLikeIdentifier("GuidanceDb"));
    try std.testing.expect(strategy_mod.looksLikeIdentifier("executeStaged"));
    try std.testing.expect(strategy_mod.looksLikeIdentifier("_private"));
}
test "looksLikeIdentifier: rejects multi-word queries" {
    try std.testing.expect(!strategy_mod.looksLikeIdentifier("sync guidance"));
    try std.testing.expect(!strategy_mod.looksLikeIdentifier("AST parser"));
    try std.testing.expect(!strategy_mod.looksLikeIdentifier("How does this work?"));
}
test "looksLikeIdentifier: rejects empty and too-long" {
    try std.testing.expect(!strategy_mod.looksLikeIdentifier(""));
    try std.testing.expect(!strategy_mod.looksLikeIdentifier("a"));
    // 65 chars — too long
    try std.testing.expect(!strategy_mod.looksLikeIdentifier("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
}
