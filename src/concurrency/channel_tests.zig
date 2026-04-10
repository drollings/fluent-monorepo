//! Tests for channel.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const channel_mod = @import("channel.zig");

test "Channel: GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("memory leak");

    const ch = try channel_mod.Channel(i32).init(gpa.allocator(), 4);
    try ch.send(1);
    _ = ch.recv();
    ch.deinit();
}
