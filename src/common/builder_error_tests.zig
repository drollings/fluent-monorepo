//! Tests for builder_error.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const builder_error_mod = @import("builder_error.zig");

test "BuilderError: GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("memory leak");
    var arena = std.heap.ArenaAllocator.init(gpa.allocator());
    defer arena.deinit();

    _ = try builder_error_mod.BuilderError.init(arena.allocator(), .validation, "port", "99999", "max=65535", error.Overflow);
}
