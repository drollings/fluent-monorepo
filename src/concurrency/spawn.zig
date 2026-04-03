//! spawn.zig — Fire-and-forget dispatch over std.Thread.Pool (M12).
//!
//! A thin wrapper that submits an `AnyWorkUnit` to a `std.Thread.Pool` via
//! `spawnWg`.  All scheduling complexity lives in `std.Thread.Pool`; this file
//! is intentionally minimal (~40 lines including comments).
//!
//! For error-capturing dispatch, use `ErrorGroup.go()` (error_group.zig).
//!
//! ## Usage
//!
//!   var wg: std.Thread.WaitGroup = .{};
//!   spawn(&pool, &wg, unit.toAny());
//!   pool.waitAndWork(&wg);
//!
//! `pool.waitAndWork` is preferred over `wg.wait()` when the calling thread
//! has no other work — it helps drain the queue, reducing latency for small
//! batches while still blocking properly (no spin).

const std = @import("std");
const AnyWorkUnit = @import("any_work_unit.zig").AnyWorkUnit;

/// Fire-and-forget spawn.  Errors from the work unit are logged but not
/// propagated.  `wg.finish()` is called by `spawnWg` after `runUnit` returns.
///
/// The work unit is executed by a thread in `pool`.  The caller must keep
/// any state referenced by the unit alive until `wg.wait()` (or
/// `pool.waitAndWork`) returns.
pub fn spawn(
    pool: *std.Thread.Pool,
    wg: *std.Thread.WaitGroup,
    unit: AnyWorkUnit,
) void {
    pool.spawnWg(wg, runUnit, .{unit});
}

fn runUnit(unit: AnyWorkUnit) void {
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

test "spawn: runs handler and signals wg" {
    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = testing.allocator, .n_jobs = 2 });
    defer pool.deinit();

    var executed = std.atomic.Value(bool).init(false);
    var ctx = Context.background();

    var wg: std.Thread.WaitGroup = .{};
    const unit = try WorkUnit(TestHandler).init(testing.allocator, .{ .executed = &executed }, &ctx);
    spawn(&pool, &wg, unit.toAny());
    pool.waitAndWork(&wg);

    try testing.expect(executed.load(.acquire));
}

test "spawn: 20 units on 4-thread pool — all complete" {
    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = testing.allocator, .n_jobs = 4 });
    defer pool.deinit();

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

    var wg: std.Thread.WaitGroup = .{};
    for (0..20) |_| {
        const unit = try WorkUnit(CountHandler).init(
            testing.allocator,
            .{ .counter = &count },
            &ctx,
        );
        spawn(&pool, &wg, unit.toAny());
    }
    pool.waitAndWork(&wg);

    try testing.expectEqual(@as(usize, 20), count.load(.acquire));
}

test "spawn: cancelled unit completes without hanging" {
    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = testing.allocator, .n_jobs = 2 });
    defer pool.deinit();

    var executed = std.atomic.Value(bool).init(false);
    var ctx = Context.background();
    ctx.cancel(error.Cancelled);

    var wg: std.Thread.WaitGroup = .{};
    const unit = try WorkUnit(TestHandler).init(testing.allocator, .{ .executed = &executed }, &ctx);
    spawn(&pool, &wg, unit.toAny());
    pool.waitAndWork(&wg);

    // Handler should NOT have been called (cancelled before execute).
    try testing.expect(!executed.load(.acquire));
}

test "spawn: GPA no leaks after pool.deinit" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("memory leak");

    var pool: std.Thread.Pool = undefined;
    try pool.init(.{ .allocator = gpa.allocator(), .n_jobs = 2 });
    defer pool.deinit();

    var executed = std.atomic.Value(bool).init(false);
    var ctx = Context.background();

    var wg: std.Thread.WaitGroup = .{};
    const unit = try WorkUnit(TestHandler).init(gpa.allocator(), .{ .executed = &executed }, &ctx);
    spawn(&pool, &wg, unit.toAny());
    pool.waitAndWork(&wg);
}
