//! Tests for line_verify.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const types = @import("../types.zig");
const line_verify_mod = @import("line_verify.zig");

test "verifyMemberLine - correct line" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\
        \\pub fn foo(x: u32) u32 {
        \\    return x;
        \\}
    ;
    const member = types.Member{
        .type = .fn_decl,
        .name = "foo",
        .line = 3,
    };
    const result = try line_verify_mod.verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(result.verified);
}
test "verifyMemberLine - stale line number" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\
        \\// some comment added above
        \\pub fn foo(x: u32) u32 {
        \\    return x;
        \\}
    ;
    // Member thinks it's at line 3, but it's actually at line 4.
    const member = types.Member{
        .type = .fn_decl,
        .name = "foo",
        .line = 3,
    };
    const result = try line_verify_mod.verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(!result.verified);
    try std.testing.expectEqual(@as(?u32, 4), result.corrected_line);
}
test "verifyMemberLine - member not found" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
    ;
    const member = types.Member{
        .type = .fn_decl,
        .name = "missingFn",
        .line = 1,
    };
    const result = try line_verify_mod.verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(!result.verified);
    try std.testing.expect(result.corrected_line == null);
}
test "verifyMemberLine - struct detection" {
    const allocator = std.testing.allocator;
    const source =
        \\const std = @import("std");
        \\
        \\pub const MyStruct = struct {
        \\    x: u32,
        \\};
    ;
    const member = types.Member{
        .type = .@"struct",
        .name = "MyStruct",
        .line = 3,
    };
    const result = try line_verify_mod.verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(result.verified);
}
test "verifyMemberLine - no line recorded" {
    const allocator = std.testing.allocator;
    const source = "pub fn foo() void {}";
    const member = types.Member{
        .type = .fn_decl,
        .name = "foo",
        .line = null,
    };
    const result = try line_verify_mod.verifyMemberLine(allocator, source, member);
    defer result.deinit(allocator);
    try std.testing.expect(!result.verified);
}
