//! spawn.zig — Fire-and-forget dispatch (M12).
//!
//! Executes an `AnyWorkUnit` synchronously.  Thread pools are not available in
//! Zig 0.16.0; all work runs on the calling thread.
//!
//! For error-capturing dispatch, use `ErrorGroup.go()` (error_group.zig).

const std = @import("std");
const AnyWorkUnit = @import("any_work_unit.zig").AnyWorkUnit;

/// Execute a work unit synchronously (ignoring cancellation errors).
pub fn spawn(unit: AnyWorkUnit) void {
    unit.runFn(unit.ptr) catch |e| {
        if (e != error.Cancelled) {
            std.log.warn("work unit failed: {s}", .{@errorName(e)});
        }
    };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;
const Context = @import("context.zig").Context;
const WorkUnit = @import("any_work_unit.zig").WorkUnit;

const TestHandler = struct {
    executed: *std.atomic.Value(bool),

    pub fn execute(self: *TestHandler, arena: std.mem.Allocator, ctx: *const Context) !void {
        _ = arena;
        _ = ctx;
        self.executed.store(true, .release);
    }
};

test "spawn: runs handler" {
    var executed = std.atomic.Value(bool).init(false);
    var ctx = Context.background();

    const unit = try WorkUnit(TestHandler).init(testing.allocator, .{ .executed = &executed }, &ctx);
    spawn(unit.toAny());

    try testing.expect(executed.load(.acquire));
}

test "spawn: 20 units — all complete" {
    var count = std.atomic.Value(usize).init(0);
    var ctx = Context.background();

    const CountHandler = struct {
        counter: *std.atomic.Value(usize),

        pub fn execute(self: *@This(), arena: std.mem.Allocator, c: *const Context) !void {
            _ = arena;
            _ = c;
            _ = self.counter.fetchAdd(1, .monotonic);
        }
    };

    for (0..20) |_| {
        const unit = try WorkUnit(CountHandler).init(
            testing.allocator,
            .{ .counter = &count },
            &ctx,
        );
        spawn(unit.toAny());
    }

    try testing.expectEqual(@as(usize, 20), count.load(.acquire));
}

test "spawn: cancelled unit does not execute handler" {
    var executed = std.atomic.Value(bool).init(false);
    var ctx = Context.background();
    ctx.cancel(error.Cancelled);

    const unit = try WorkUnit(TestHandler).init(testing.allocator, .{ .executed = &executed }, &ctx);
    spawn(unit.toAny());

    // Handler should NOT have been called (cancelled before execute).
    try testing.expect(!executed.load(.acquire));
}

test "spawn: GPA no leaks" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("memory leak");

    var executed = std.atomic.Value(bool).init(false);
    var ctx = Context.background();

    const unit = try WorkUnit(TestHandler).init(gpa.allocator(), .{ .executed = &executed }, &ctx);
    spawn(unit.toAny());
}
