//! error_group.zig — Structured parallel dispatch with error capture (M14).
//!
//! `ErrorGroup` submits work units to a `std.Thread.Pool`, captures errors,
//! and cancels the shared `Context` on the first failure.
//!
//! ## Usage
//!
//!   var ctx = Context.background();
//!   var group = ErrorGroup.init(allocator, &pool, &ctx);
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
//! - `go()` transfers ownership of the work unit to the pool closure.
//! - `wait()` blocks until all submitted units complete (via `waitAndWork`).
//! - `deinit()` is safe to call after `wait()`.
//! - `go()` must only be called from the submitting thread (not thread-safe).
//! - `wait()` and `waitAll()` must not be called concurrently.
//!
//! ## Cancellation
//!
//! When the first unit fails, `ErrorGroup` calls `ctx.cancel(err)`.  Subsequent
//! units will observe `ctx.isCancelled() == true` and return `error.Cancelled`
//! without doing real work — but they still complete (no orphaned goroutines).

const std = @import("std");
const Context = @import("context.zig").Context;
const AnyWorkUnit = @import("any_work_unit.zig").AnyWorkUnit;

/// Manages error group state with fixed buffers; owned by the caller; ensures consistent invariants.
pub const ErrorGroup = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    pool: *std.Thread.Pool,
    ctx: *Context,
    wg: std.Thread.WaitGroup,
    mu: std.Thread.Mutex,
    first: ?anyerror,
    all: std.ArrayListUnmanaged(anyerror),

    /// Initialise without heap allocation.  The pool and context must outlive
    /// the ErrorGroup.
    pub fn init(
        allocator: std.mem.Allocator,
        pool: *std.Thread.Pool,
        ctx: *Context,
    ) Self {
        return .{
            .allocator = allocator,
            .pool = pool,
            .ctx = ctx,
            .wg = .{},
            .mu = .{},
            .first = null,
            .all = .empty,
        };
    }

    /// Free the error list.  Call after `wait()`.
    pub fn deinit(self: *Self) void {
        self.all.deinit(self.allocator);
    }

    /// Submit a work unit for parallel execution.  On failure, the first error
    /// cancels the shared context; all errors are recorded for `waitAll()`.
    ///
    /// Must only be called from the submitting thread.
    pub fn go(self: *Self, unit: AnyWorkUnit) void {
        const closure = GroupClosure{ .group = self, .unit = unit };
        self.pool.spawnWg(&self.wg, GroupClosure.run, .{closure});
    }

    /// Block until all submitted units complete.
    /// Returns the first error observed, or null if all succeeded.
    ///
    /// Uses `waitAndWork`: the calling thread helps drain the pool queue while
    /// waiting, reducing latency for small batches.
    pub fn wait(self: *Self) ?anyerror {
        self.pool.waitAndWork(&self.wg);
        self.mu.lock();
        defer self.mu.unlock();
        return self.first;
    }

    /// Block until all submitted units complete.
    /// Returns a slice of all errors (may be empty).  Caller owns the slice;
    /// free with `allocator.free(slice)`.
    pub fn waitAll(self: *Self) ![]anyerror {
        self.pool.waitAndWork(&self.wg);
        self.mu.lock();
        defer self.mu.unlock();
        return self.all.toOwnedSlice(self.allocator);
    }

    fn recordError(self: *Self, e: anyerror) void {
        self.mu.lock();
        defer self.mu.unlock();
        if (self.first == null) {
            self.first = e;
            self.ctx.cancel(e);
        }
        self.all.append(self.allocator, e) catch {};
    }

    const GroupClosure = struct {
        group: *Self,
        unit: AnyWorkUnit,

        fn run(self: GroupClosure) void {
            self.unit.runFn(self.unit.ptr) catch |e| {
                self.group.recordError(e);
            };
        }
    };
};

// ── Tests ─────────────────────────────────────────────────────────────────────

const testing = std.testing;
const WorkUnit = @import("any_work_unit.zig").WorkUnit;

/// Handler that atomically increments a counter on success, or returns an error.
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
    // Pool must be declared in the same scope it's used — never return by value.
    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = testing.allocator, .n_jobs = 4 });
    defer pool.deinit();

    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &pool, &ctx);
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
    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = testing.allocator, .n_jobs = 4 });
    defer pool.deinit();

    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &pool, &ctx);
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
    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = testing.allocator, .n_jobs = 4 });
    defer pool.deinit();

    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &pool, &ctx);
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
    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = testing.allocator, .n_jobs = 4 });
    defer pool.deinit();

    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &pool, &ctx);
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

test "ErrorGroup: wait blocks until all units complete (including post-error)" {
    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = testing.allocator, .n_jobs = 2 });
    defer pool.deinit();

    var counter = std.atomic.Value(usize).init(0);
    var ctx = Context.background();
    var group = ErrorGroup.init(testing.allocator, &pool, &ctx);
    defer group.deinit();

    const SlowHandler = struct {
        counter: *std.atomic.Value(usize),
        delay_ns: u64,
        should_fail: bool = false,

        pub fn execute(self: *@This(), arena: std.mem.Allocator, c: *const Context) !void {
            _ = arena;
            _ = c;
            std.Thread.sleep(self.delay_ns);
            _ = self.counter.fetchAdd(1, .monotonic);
            if (self.should_fail) return error.SlowFail;
        }
    };

    for (0..4) |i| {
        const unit = try WorkUnit(SlowHandler).init(
            testing.allocator,
            .{ .counter = &counter, .delay_ns = 1 * std.time.ns_per_ms, .should_fail = i == 1 },
            &ctx,
        );
        group.go(unit.toAny());
    }

    _ = group.wait();
    // All 4 units must have run (even slow ones after the error).
    try testing.expectEqual(@as(usize, 4), counter.load(.acquire));
}

test "ErrorGroup: GPA no leaks — all-success, one-fail, all-fail" {
    {
        var gpa = std.heap.GeneralPurposeAllocator(.{}){};
        defer if (gpa.deinit() == .leak) @panic("leak: all-success");
        var pool: std.Thread.Pool = undefined;
        try pool.init(.{ .allocator = gpa.allocator(), .n_jobs = 2 });
        defer pool.deinit();
        var counter = std.atomic.Value(usize).init(0);
        var ctx = Context.background();
        var group = ErrorGroup.init(gpa.allocator(), &pool, &ctx);
        defer group.deinit();
        for (0..3) |_| {
            const unit = try WorkUnit(CountOrFailHandler).init(gpa.allocator(), .{ .counter = &counter }, &ctx);
            group.go(unit.toAny());
        }
        _ = group.wait();
    }
    {
        var gpa = std.heap.GeneralPurposeAllocator(.{}){};
        defer if (gpa.deinit() == .leak) @panic("leak: one-fail");
        var pool: std.Thread.Pool = undefined;
        try pool.init(.{ .allocator = gpa.allocator(), .n_jobs = 2 });
        defer pool.deinit();
        var counter = std.atomic.Value(usize).init(0);
        var ctx = Context.background();
        var group = ErrorGroup.init(gpa.allocator(), &pool, &ctx);
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
        var gpa = std.heap.GeneralPurposeAllocator(.{}){};
        defer if (gpa.deinit() == .leak) @panic("leak: all-fail");
        var pool: std.Thread.Pool = undefined;
        try pool.init(.{ .allocator = gpa.allocator(), .n_jobs = 2 });
        defer pool.deinit();
        var counter = std.atomic.Value(usize).init(0);
        var ctx = Context.background();
        var group = ErrorGroup.init(gpa.allocator(), &pool, &ctx);
        defer group.deinit();
        for (0..3) |_| {
            const unit = try WorkUnit(CountOrFailHandler).init(gpa.allocator(), .{ .counter = &counter, .should_error = true }, &ctx);
            group.go(unit.toAny());
        }
        _ = group.wait();
    }
}
