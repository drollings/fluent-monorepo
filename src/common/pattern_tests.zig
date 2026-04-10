//! Tests for pattern.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const pattern_mod = @import("pattern.zig");

test "detectRingBuffer" {
    try std.testing.expect(pattern_mod.detectRingBuffer("pub const RingBuffer = struct { ... }"));
    try std.testing.expect(pattern_mod.detectRingBuffer("// circular queue implementation"));
    try std.testing.expect(!pattern_mod.detectRingBuffer("pub fn add(a: i32, b: i32) i32 { return a + b; }"));
}
test "detectFactory" {
    try std.testing.expect(pattern_mod.detectFactory(undefined, undefined, "pub fn create(alloc: Allocator) !*Foo { ... }"));
    try std.testing.expect(pattern_mod.detectFactory(undefined, undefined, "pub const FooFactory = struct {}"));
    try std.testing.expect(!pattern_mod.detectFactory(undefined, undefined, "pub fn parse(input: []u8) !Foo {}"));
}
test "detectObserver" {
    try std.testing.expect(pattern_mod.detectObserver(
        undefined,
        undefined,
        "fn subscribe(self: *Self, cb: Callback) void {} fn notify(self: *Self) void {}",
    ));
    try std.testing.expect(!pattern_mod.detectObserver(undefined, undefined, "fn compute(x: f64) f64 { return x; }"));
}
