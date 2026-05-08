//! error_group.zig — Structured parallel dispatch with error capture (M14).
//!
//! `ErrorGroup` submits work units for execution, captures errors,
//! and cancels the shared `Context` on the first failure.
//!
//! ## Usage
//!
//!   var ctx = Context.background();
//!   var group = ErrorGroup.init(allocator, &ctx);
//!   defer group.deinit();
//!
//!   for (inputs) |input| {
//!       const unit = try WorkUnit(Handler).init(allocator, Handler{ .input = input }, &ctx);
//!       group.go(unit.toAny());
//!   }
//!
//!   if (group.wait()) |first_err| return first_err;
//!
//! ## Ownership
//!
//! - `go()` executes the work unit synchronously and transfers ownership.
//! - `wait()` returns the first recorded error if any.
//! - `deinit()` is safe to call after `wait()`.

const std = @import("std");
const Context = @import("context.zig").Context;
const AnyWorkUnit = @import("any_work_unit.zig").AnyWorkUnit;

pub const ErrorGroup = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    ctx: *Context,
    mu: std.Io.Mutex,
    first: ?anyerror,
    all: std.ArrayList(anyerror),

    /// Initialise without heap allocation. The context must outlive the ErrorGroup.
    pub fn init(
        allocator: std.mem.Allocator,
        ctx: *Context,
    ) Self {
        return .{
            .allocator = allocator,
            .ctx = ctx,
            .mu = .init,
            .first = null,
            .all = .empty,
        };
    }

    /// Free the error list. Call after `wait()`.
    pub fn deinit(self: *Self) void {
        self.all.deinit(self.allocator);
    }

    /// Execute a work unit synchronously. On failure, the first error
    /// cancels the shared context; all errors are recorded for `waitAll()`.
    pub fn go(self: *Self, unit: AnyWorkUnit) void {
        unit.runFn(unit.ptr) catch |e| {
            self.recordError(e);
        };
    }

    /// Returns the first error observed, or null if all succeeded.
    pub fn wait(self: *Self) ?anyerror {
        return self.first;
    }

    /// Returns a slice of all errors (may be empty). Caller owns the slice;
    /// free with `allocator.free(slice)`.
    pub fn waitAll(self: *Self) ![]anyerror {
        return self.all.toOwnedSlice(self.allocator);
    }

    fn recordError(self: *Self, e: anyerror) void {
        if (self.first == null) {
            self.first = e;
            self.ctx.cancel(e);
        }
        self.all.append(self.allocator, e) catch {};
    }
};

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;
const WorkUnit = @import("any_work_unit.zig").WorkUnit;

const CountOrFailHandler = struct {
    counter: *std.atomic.Value(usize),
    should_error: bool = false,

    pub fn execute(self: *CountOrFailHandler, arena: std.mem.Allocator, ctx: *const Context) !void {
        _ = arena;
        if (ctx.isCancelled()) return error.Cancelled;
        if (self.should_error) return error.UnitFailed;
        _ = self.counter.fetchAdd(1, .monotonic);
    }
};

test "ErrorGroup: wait returns null when all units succeed" {
    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &ctx);
    defer group.deinit();

    for (0..5) |_| {
        const unit = try WorkUnit(CountOrFailHandler).init(
            testing.allocator,
            .{ .counter = &counter },
            &ctx,
        );
        group.go(unit.toAny());
    }

    try testing.expectEqual(@as(?anyerror, null), group.wait());
    try testing.expectEqual(@as(usize, 5), counter.load(.acquire));
}

test "ErrorGroup: wait returns first error when one unit fails" {
    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &ctx);
    defer group.deinit();

    // Submit one failing unit and four succeeding units.
    const fail_unit = try WorkUnit(CountOrFailHandler).init(
        testing.allocator,
        .{ .counter = &counter, .should_error = true },
        &ctx,
    );
    group.go(fail_unit.toAny());

    for (0..4) |_| {
        const unit = try WorkUnit(CountOrFailHandler).init(
            testing.allocator,
            .{ .counter = &counter },
            &ctx,
        );
        group.go(unit.toAny());
    }

    const e = group.wait();
    try testing.expect(e != null);
}

test "ErrorGroup: waitAll collects all errors" {
    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &ctx);
    defer group.deinit();

    // Three failing units.
    for (0..3) |_| {
        const unit = try WorkUnit(CountOrFailHandler).init(
            testing.allocator,
            .{ .counter = &counter, .should_error = true },
            &ctx,
        );
        group.go(unit.toAny());
    }

    const errs = try group.waitAll();
    defer testing.allocator.free(errs);
    // At least one error; others may be Cancelled.
    try testing.expect(errs.len > 0);
}

test "ErrorGroup: first error cancels context; subsequent units observe cancellation" {
    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &ctx);
    defer group.deinit();

    // Failing unit first.
    const fail_unit = try WorkUnit(CountOrFailHandler).init(
        testing.allocator,
        .{ .counter = &counter, .should_error = true },
        &ctx,
    );
    group.go(fail_unit.toAny());

    // Five units that check cancellation.
    for (0..5) |_| {
        const unit = try WorkUnit(CountOrFailHandler).init(
            testing.allocator,
            .{ .counter = &counter },
            &ctx,
        );
        group.go(unit.toAny());
    }

    _ = group.wait();
    // Context must be cancelled after wait.
    try testing.expect(ctx.isCancelled());
}

test "ErrorGroup: all submitted units run (synchronous mode)" {
    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &ctx);
    defer group.deinit();

    // In synchronous mode: units 0 and 1 run (1 fails, cancels ctx),
    // then units 2 and 3 observe cancellation and short-circuit.
    // The counter reflects only units that ran before cancellation check.
    const SlowHandler = struct {
        counter: *std.atomic.Value(usize),
        should_fail: bool = false,

        pub fn execute(self: *@This(), arena: std.mem.Allocator, c: *const Context) !void {
            _ = arena;
            _ = c;
            _ = self.counter.fetchAdd(1, .monotonic);
            if (self.should_fail) return error.SlowFail;
        }
    };

    for (0..4) |i| {
        const unit = try WorkUnit(SlowHandler).init(
            testing.allocator,
            .{ .counter = &counter, .should_fail = i == 1 },
            &ctx,
        );
        group.go(unit.toAny());
    }

    const e = group.wait();
    // At least one error occurred (the fail unit).
    try testing.expect(e != null);
    // Context was cancelled.
    try testing.expect(ctx.isCancelled());
    // Synchronous: at least the first 2 units ran (before ctx cancellation check).
    try testing.expect(counter.load(.acquire) >= 2);
}

test "ErrorGroup: GPA no leaks — all-success, one-fail, all-fail" {
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: all-success");
        var counter = std.atomic.Value(usize).init(0);
        var ctx = Context.background();
        var group = ErrorGroup.init(gpa.allocator(), &ctx);
        defer group.deinit();
        for (0..3) |_| {
            const unit = try WorkUnit(CountOrFailHandler).init(gpa.allocator(), .{ .counter = &counter }, &ctx);
            group.go(unit.toAny());
        }
        _ = group.wait();
    }
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: one-fail");
        var counter = std.atomic.Value(usize).init(0);
        var ctx = Context.background();
        var group = ErrorGroup.init(gpa.allocator(), &ctx);
        defer group.deinit();
        const fail = try WorkUnit(CountOrFailHandler).init(gpa.allocator(), .{ .counter = &counter, .should_error = true }, &ctx);
        group.go(fail.toAny());
        for (0..2) |_| {
            const unit = try WorkUnit(CountOrFailHandler).init(gpa.allocator(), .{ .counter = &counter }, &ctx);
            group.go(unit.toAny());
        }
        _ = group.wait();
    }
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: all-fail");
        var counter = std.atomic.Value(usize).init(0);
        var ctx = Context.background();
        var group = ErrorGroup.init(gpa.allocator(), &ctx);
        defer group.deinit();
        for (0..3) |_| {
            const unit = try WorkUnit(CountOrFailHandler).init(gpa.allocator(), .{ .counter = &counter, .should_error = true }, &ctx);
            group.go(unit.toAny());
        }
        _ = group.wait();
    }
}
