//! Tests for work_unit.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const work_unit_mod = @import("work_unit.zig");

const testing = std.testing;

const BoolHandler = struct {
    executed: *bool,
    should_error: bool = false,

    pub fn execute(self: *BoolHandler, arena: std.mem.Allocator) !void {
        _ = arena;
        self.executed.* = true;
        if (self.should_error) return error.BoolHandlerFailed;
    }
};

test "WorkUnit: runFn executes handler" {
    var ok = false;
    const unit = try work_unit_mod.WorkUnit(BoolHandler).init(testing.allocator, .{ .executed = &ok });
    const any = unit.toAny();
    try any.runFn(any.ptr);
    try testing.expect(ok);
}

test "WorkUnit: runFn propagates handler error" {
    var ok = false;
    const unit = try work_unit_mod.WorkUnit(BoolHandler).init(testing.allocator, .{ .executed = &ok, .should_error = true });
    const any = unit.toAny();
    try testing.expectError(error.BoolHandlerFailed, any.runFn(any.ptr));
    try testing.expect(ok); // handler still ran
}

test "WorkUnit: deinitFn cleans up without executing" {
    var ok = false;
    const unit = try work_unit_mod.WorkUnit(BoolHandler).init(testing.allocator, .{ .executed = &ok });
    const any = unit.toAny();
    any.deinitFn(any.ptr);
    try testing.expect(!ok);
}

test "WorkUnit: GPA no leaks — success / error / deinit paths" {
    // success
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: success");
        var ok = false;
        const u = try work_unit_mod.WorkUnit(BoolHandler).init(gpa.allocator(), .{ .executed = &ok });
        const any = u.toAny();
        _ = any.runFn(any.ptr) catch {};
    }
    // error
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: error");
        var ok = false;
        const u = try work_unit_mod.WorkUnit(BoolHandler).init(gpa.allocator(), .{ .executed = &ok, .should_error = true });
        const any = u.toAny();
        _ = any.runFn(any.ptr) catch {};
    }
    // deinit
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: deinit");
        var ok = false;
        const u = try work_unit_mod.WorkUnit(BoolHandler).init(gpa.allocator(), .{ .executed = &ok });
        const any = u.toAny();
        any.deinitFn(any.ptr);
    }
}

test "WorkUnit: checkCancel is a no-op outside zio runtime — handler still runs" {
    // Outside a zio runtime checkCancel() returns immediately without error.
    // This test runs in the standard test harness (no zio Runtime), so the
    // pre-execution check must not prevent the handler from executing.
    var ok = false;
    const unit = try work_unit_mod.WorkUnit(BoolHandler).init(testing.allocator, .{ .executed = &ok });
    const any = unit.toAny();
    try any.runFn(any.ptr);
    try testing.expect(ok);
}

test "WorkUnit: arena is freed — alloc inside execute does not leak" {
    const ArenaHandler = struct {
        counter: *usize,

        pub fn execute(self: *@This(), arena: std.mem.Allocator) !void {
            _ = try arena.alloc(u8, 256); // arena-owned; freed with the unit
            self.counter.* += 1;
        }
    };

    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak: arena alloc");
    var count: usize = 0;
    const unit = try work_unit_mod.WorkUnit(ArenaHandler).init(gpa.allocator(), .{ .counter = &count });
    const any = unit.toAny();
    try any.runFn(any.ptr);
    try testing.expectEqual(@as(usize, 1), count);
}
