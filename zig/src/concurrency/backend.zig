//! backend.zig — ExecutionBackend VTable + SyncBackend + ZioBackend.
//!
//! ## VTable justification
//! Two concrete implementations exist today (SyncBackend, ZioBackend), so a
//! VTable is warranted per the Fluent WVR decision tree.
//!
//! ## Usage — SyncBackend (tests / single-threaded callers)
//!
//!   var sync = SyncBackend{};
//!   const backend = sync.backend();
//!   try backend.submit(unit.toAny());
//!   try backend.flush(); // no-op for SyncBackend; units run inline
//!
//! ## Usage — ZioBackend (production, inside a zio runtime)
//!
//!   const backend_obj = try ZioBackend.builder()
//!       .withPermits(8)
//!       .withFailFast(true)
//!       .build(allocator);
//!   defer backend_obj.deinit(); // must call flush() first
//!
//!   const backend = backend_obj.backend();
//!   try backend.submit(unit.toAny()); // group.spawn() internally
//!   try backend.flush();              // group.wait() + hasFailed() check
//!   backend.deinit();
//!
//! ## Ownership rules
//!
//! submit() transfers ownership of the AnyWorkUnit.  If submit() returns an
//! error the unit has already been cleaned up via deinitFn.
//! flush() must be called before deinit() on ZioBackend.

const std = @import("std");
const zio = @import("zio");
const work_unit = @import("work_unit.zig");

pub const AnyWorkUnit = work_unit.AnyWorkUnit;

// ─── VTable ───────────────────────────────────────────────────────────────────

/// Type-erased execution backend.
pub const ExecutionBackend = struct {
    ptr: *anyopaque,
    vtable: *const VTable,

    pub const VTable = struct {
        /// Submit a unit for execution.  Transfers ownership; cleans up on error.
        submitFn: *const fn (ptr: *anyopaque, unit: AnyWorkUnit) anyerror!void,
        /// Wait for all in-flight units to complete.
        flushFn: *const fn (ptr: *anyopaque) anyerror!void,
        /// Release backend resources.  flush() must precede deinit().
        deinitFn: *const fn (ptr: *anyopaque) void,
    };

    pub fn submit(self: ExecutionBackend, unit: AnyWorkUnit) anyerror!void {
        return self.vtable.submitFn(self.ptr, unit);
    }

    pub fn flush(self: ExecutionBackend) anyerror!void {
        return self.vtable.flushFn(self.ptr);
    }

    pub fn deinit(self: ExecutionBackend) void {
        self.vtable.deinitFn(self.ptr);
    }
};

// ─── SyncBackend ──────────────────────────────────────────────────────────────

/// Synchronous, stack-allocatable backend.  Runs each unit inline in submit().
/// Never errors on flush().  Safe to use outside a zio runtime.
pub const SyncBackend = struct {
    last_err: ?anyerror = null,

    const vtable = ExecutionBackend.VTable{
        .submitFn = submitFn,
        .flushFn = flushFn,
        .deinitFn = deinitFn,
    };

    pub fn backend(self: *SyncBackend) ExecutionBackend {
        return .{ .ptr = self, .vtable = &vtable };
    }

    fn submitFn(ptr: *anyopaque, unit: AnyWorkUnit) anyerror!void {
        const self: *SyncBackend = @ptrCast(@alignCast(ptr));
        // Run all units regardless of prior errors so that every result is
        // delivered to its Channel — prevents DAG-walker from deadlocking.
        unit.runFn(unit.ptr) catch |e| {
            self.last_err = e;
        };
    }

    fn flushFn(ptr: *anyopaque) anyerror!void {
        const self: *SyncBackend = @ptrCast(@alignCast(ptr));
        if (self.last_err) |e| {
            self.last_err = null;
            return e;
        }
    }

    fn deinitFn(_: *anyopaque) void {}
};

// ─── ZioBackend ───────────────────────────────────────────────────────────────

/// Async backend backed by a zio.Group + zio.Semaphore.
/// Heap-allocated so Group and Semaphore fields have stable addresses.
/// Must be used from within a zio runtime task.
///
/// Fluent Builder:
///   ZioBackend.builder()
///       .withPermits(N)       // default: 8 — max concurrent units
///       .withFailFast(true)   // default: false
///       .build(allocator)     // returns *ZioBackend (caller owns)
pub const ZioBackend = struct {
    allocator: std.mem.Allocator,
    group: zio.Group,
    semaphore: zio.Semaphore,

    const vtable = ExecutionBackend.VTable{
        .submitFn = submitFn,
        .flushFn = flushFn,
        .deinitFn = deinitFn,
    };

    // ── Builder ──────────────────────────────────────────────────────────────

    pub const Builder = struct {
        permits: u32 = 8,
        fail_fast: bool = false,

        pub fn withPermits(self: Builder, n: u32) Builder {
            var b = self;
            b.permits = n;
            return b;
        }

        pub fn withFailFast(self: Builder, v: bool) Builder {
            var b = self;
            b.fail_fast = v;
            return b;
        }

        pub fn build(self: Builder, allocator: std.mem.Allocator) !*ZioBackend {
            const z = try allocator.create(ZioBackend);
            z.* = .{
                .allocator = allocator,
                .group = .init,
                .semaphore = .{ .permits = self.permits },
            };
            if (self.fail_fast) z.group.setFailFast();
            return z;
        }
    };

    pub fn builder() Builder {
        return .{};
    }

    // ── VTable impl ──────────────────────────────────────────────────────────

    pub fn backend(self: *ZioBackend) ExecutionBackend {
        return .{ .ptr = self, .vtable = &vtable };
    }

    fn submitFn(ptr: *anyopaque, unit: AnyWorkUnit) anyerror!void {
        const self: *ZioBackend = @ptrCast(@alignCast(ptr));
        self.group.spawn(runUnit, .{ &self.semaphore, unit }) catch |e| {
            unit.deinitFn(unit.ptr); // discard unit if spawn fails
            return e;
        };
    }

    fn flushFn(ptr: *anyopaque) anyerror!void {
        const self: *ZioBackend = @ptrCast(@alignCast(ptr));
        // wait() only returns error.Canceled when the *calling* coroutine is
        // cancelled; task errors are captured by group internally.
        try self.group.wait();
        if (self.group.hasFailed()) return error.TaskFailed;
    }

    fn deinitFn(ptr: *anyopaque) void {
        const self: *ZioBackend = @ptrCast(@alignCast(ptr));
        self.allocator.destroy(self);
    }

    // ── Internal task ────────────────────────────────────────────────────────

    /// Runs inside the zio runtime.  Acquires a semaphore permit, runs the
    /// unit, releases the permit.  Cleans up the unit on cancellation.
    fn runUnit(sem: *zio.Semaphore, unit: AnyWorkUnit) !void {
        sem.wait() catch |e| {
            unit.deinitFn(unit.ptr);
            return e;
        };
        defer sem.post();
        try unit.runFn(unit.ptr);
    }
};

// ─── Tests ────────────────────────────────────────────────────────────────────

const testing = std.testing;
const WorkUnit = work_unit.WorkUnit;

const CountHandler = struct {
    counter: *usize,
    fail: bool = false,

    pub fn execute(self: *CountHandler, arena: std.mem.Allocator) !void {
        _ = arena;
        self.counter.* += 1;
        if (self.fail) return error.CountFailed;
    }
};

test "SyncBackend: submits and runs units inline" {
    var count: usize = 0;
    var sync = SyncBackend{};
    const b = sync.backend();

    const wu1 = try WorkUnit(CountHandler).init(testing.allocator, .{ .counter = &count });
    const wu2 = try WorkUnit(CountHandler).init(testing.allocator, .{ .counter = &count });
    try b.submit(wu1.toAny());
    try b.submit(wu2.toAny());
    try b.flush();

    try testing.expectEqual(@as(usize, 2), count);
}

test "SyncBackend: flush returns last error, then clears it" {
    var count: usize = 0;
    var sync = SyncBackend{};
    const b = sync.backend();

    // failing unit — submit absorbs the error, flush surfaces it
    const u = try WorkUnit(CountHandler).init(testing.allocator, .{ .counter = &count, .fail = true });
    try b.submit(u.toAny()); // does NOT return error
    try testing.expectError(error.CountFailed, b.flush());
    // second flush is clean
    try b.flush();
}

test "SyncBackend: continues after first failure (no deadlock)" {
    var count: usize = 0;
    var sync = SyncBackend{};
    const b = sync.backend();

    const uf = try WorkUnit(CountHandler).init(testing.allocator, .{ .counter = &count, .fail = true });
    const us = try WorkUnit(CountHandler).init(testing.allocator, .{ .counter = &count });
    try b.submit(uf.toAny());
    try b.submit(us.toAny());
    // both ran regardless of first failure
    try testing.expectEqual(@as(usize, 2), count);
    _ = b.flush() catch {};
}

test "SyncBackend: GPA no leaks" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    var count: usize = 0;
    var sync = SyncBackend{};
    const b = sync.backend();
    const u = try WorkUnit(CountHandler).init(gpa.allocator(), .{ .counter = &count });
    try b.submit(u.toAny());
    try b.flush();
}
