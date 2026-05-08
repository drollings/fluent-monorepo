//! concurrency — lightweight zio-backed work queue with Fluent WVR interface.
//!
//! ## Public API
//!
//!   // Type-erased work unit
//!   AnyWorkUnit          — 3-pointer handle (ptr, runFn, deinitFn)
//!   WorkUnit(Handler)    — typed wrapper; init() → toAny()
//!
//!   // Execution backends
//!   ExecutionBackend     — VTable interface (submit / flush / deinit)
//!   SyncBackend          — inline, stack-allocatable; safe outside zio runtime
//!   ZioBackend           — async, zio.Group + zio.Semaphore; Fluent Builder
//!
//!   // Fire-and-forget dispatch
//!   spawn(unit)          — run unit synchronously on the calling thread;
//!                          swallows errors (logs non-cancellation failures)
//!
//!   // zio primitives re-exported for caller convenience
//!   Channel              — zio.Channel(T)
//!   Semaphore            — zio.Semaphore
//!   Group                — zio.Group
//!   checkCancel          — zio.checkCancel
//!
//! ## Quick start
//!
//!   const concurrency = @import("concurrency");
//!
//!   // SyncBackend (tests)
//!   var sync = concurrency.SyncBackend{};
//!   const b  = sync.backend();
//!
//!   // ZioBackend (production — must be inside a zio runtime task)
//!   const zb = try concurrency.ZioBackend.builder()
//!       .withPermits(8)
//!       .build(allocator);
//!   defer zb.deinit();
//!   const b = zb.backend();
//!
//!   // Define a handler
//!   const MyHandler = struct {
//!       result_ch: *concurrency.Channel(MyResult),
//!       pub fn execute(self: *MyHandler, arena: std.mem.Allocator) !void {
//!           const r = try compute(arena);
//!           try self.result_ch.send(r);
//!       }
//!   };
//!
//!   // Submit work
//!   const unit = try concurrency.WorkUnit(MyHandler).init(allocator, handler);
//!   try b.submit(unit.toAny());
//!   try b.flush();
//!
//!   // Or fire-and-forget
//!   const unit2 = try concurrency.WorkUnit(MyHandler).init(allocator, handler);
//!   concurrency.spawn(unit2.toAny());

const std = @import("std");
const zio = @import("zio");
const work_unit = @import("work_unit.zig");

// Own types
pub const AnyWorkUnit = work_unit.AnyWorkUnit;
pub const WorkUnit = work_unit.WorkUnit;
pub const ExecutionBackend = @import("backend.zig").ExecutionBackend;
pub const SyncBackend = @import("backend.zig").SyncBackend;
pub const ZioBackend = @import("backend.zig").ZioBackend;

// zio primitives — re-exported so callers need not import zio directly
pub const Channel = zio.Channel;
pub const Semaphore = zio.Semaphore;
pub const Group = zio.Group;
pub const checkCancel = zio.checkCancel;

/// Fire-and-forget synchronous dispatch.
///
/// Executes the unit on the calling thread and discards the result.
/// `error.Canceled` (from zio.checkCancel()) is silently swallowed — it means
/// the enclosing Group was cancelled before this unit ran, which is expected.
/// All other errors are logged at warn level.
///
/// Equivalent to SyncBackend.submit() but without capturing the error for
/// a later flush().  Use this when the caller does not need to observe failure.
pub fn spawn(unit: AnyWorkUnit) void {
    unit.runFn(unit.ptr) catch |e| {
        if (e != error.Canceled) {
            std.log.warn("work unit failed: {s}", .{@errorName(e)});
        }
    };
}

// ─── Tests ────────────────────────────────────────────────────────────────────

const testing = std.testing;

const SpawnCountHandler = struct {
    counter: *usize,
    should_fail: bool = false,

    pub fn execute(self: *SpawnCountHandler, arena: std.mem.Allocator) !void {
        _ = arena;
        self.counter.* += 1;
        if (self.should_fail) return error.SpawnTestFail;
    }
};

test "spawn: runs handler on calling thread" {
    var count: usize = 0;
    const unit = try WorkUnit(SpawnCountHandler).init(
        testing.allocator,
        .{ .counter = &count },
    );
    spawn(unit.toAny());
    try testing.expectEqual(@as(usize, 1), count);
}

test "spawn: swallows handler error without panicking" {
    var count: usize = 0;
    const unit = try WorkUnit(SpawnCountHandler).init(
        testing.allocator,
        .{ .counter = &count, .should_fail = true },
    );
    spawn(unit.toAny()); // must not panic; error is logged then discarded
    try testing.expectEqual(@as(usize, 1), count); // handler still ran
}

test "spawn: GPA no leaks — success and error paths" {
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: success");
        var count: usize = 0;
        const unit = try WorkUnit(SpawnCountHandler).init(gpa.allocator(), .{ .counter = &count });
        spawn(unit.toAny());
    }
    {
        var gpa: std.heap.DebugAllocator(.{}) = .init;
        defer if (gpa.deinit() == .leak) @panic("leak: error");
        var count: usize = 0;
        const unit = try WorkUnit(SpawnCountHandler).init(gpa.allocator(), .{ .counter = &count, .should_fail = true });
        spawn(unit.toAny());
    }
}
