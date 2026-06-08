//! work_unit.zig — Type-erased work unit with per-unit arena (WVR pattern).
//!
//! This is the Zig equivalent of a Go closure passed to a goroutine: the
//! Handler struct is the captured state, and toAny() type-erases it for
//! submission to any ExecutionBackend.
//!
//! ## Usage
//!
//!   const TargetHandler = struct {
//!       target:    *const Target,
//!       result_ch: *zio.Channel(DagResult),
//!
//!       pub fn execute(self: *TargetHandler, arena: std.mem.Allocator) !void {
//!           const result = try runTarget(self.target, arena);
//!           try self.result_ch.send(result);
//!       }
//!   };
//!
//!   const unit = try WorkUnit(TargetHandler).init(allocator, .{
//!       .target    = my_target,
//!       .result_ch = &result_ch,
//!   });
//!   try backend.submit(unit.toAny());
//!
//! ## Ownership
//!
//! After toAny() the caller must not access the pointer.
//! runFn and deinitFn both free the struct and deinit the per-unit arena
//! on every exit path (success, error, cancellation).
//!
//! ## Cancellation
//!
//! runFn calls zio.checkCancel() before execute() — a unit whose enclosing
//! Group has been cancelled is silently skipped without entering the handler.
//! checkCancel() is a no-op outside a zio runtime, so SyncBackend is safe.
//!
//! Handlers do NOT need to call zio.checkCancel() themselves unless they want
//! mid-handler yield points (e.g. after a slow allocation step).

const std = @import("std");
const zio = @import("zio");

/// Type-erased 3-pointer work handle.  Size invariant: exactly 3 pointers.
pub const AnyWorkUnit = struct {
    ptr: *anyopaque,
    /// Execute handler, deinit arena, destroy self.  Called at most once.
    runFn: *const fn (ptr: *anyopaque) anyerror!void,
    /// Deinit arena and destroy self without executing.  Called when the
    /// unit is discarded before execution — e.g. after ZioBackend
    /// cancellation or a failed group.spawn().
    deinitFn: *const fn (ptr: *anyopaque) void,
};

comptime {
    std.debug.assert(@sizeOf(AnyWorkUnit) == 3 * @sizeOf(*anyopaque));
}

/// Typed wrapper over a `Handler`.
///
/// Handler contract:
///   pub fn execute(self: *Handler, arena: std.mem.Allocator) anyerror!void
///
/// The arena is valid for the duration of execute() only.
/// If execute() allocates data that must outlive itself, write it to a
/// Channel or a caller-owned pointer in the handler struct before returning.
pub fn WorkUnit(comptime Handler: type) type {
    return struct {
        const Self = @This();

        parent: std.mem.Allocator,
        arena: std.heap.ArenaAllocator,
        handler: Handler,

        /// Heap-allocate and initialise.  Ownership transfers to the
        /// AnyWorkUnit returned by toAny(); do not retain the pointer.
        pub fn init(allocator: std.mem.Allocator, handler: Handler) !*Self {
            const self = try allocator.create(Self);
            self.* = .{
                .parent = allocator,
                .arena = std.heap.ArenaAllocator.init(allocator),
                .handler = handler,
            };
            return self;
        }

        /// Produce a type-erased handle.  After this call the caller
        /// must not access `self`.
        pub fn toAny(self: *Self) AnyWorkUnit {
            return .{ .ptr = self, .runFn = runFn, .deinitFn = deinitFn };
        }

        fn runFn(ptr: *anyopaque) anyerror!void {
            const self: *Self = @ptrCast(@alignCast(ptr));
            const parent = self.parent;
            defer {
                self.arena.deinit();
                parent.destroy(self);
            }
            // Dead-on-arrival check: skip the handler if the enclosing Group
            // was cancelled before this unit was dequeued.  Outside a zio
            // runtime (e.g. SyncBackend) checkCancel() is a no-op.
            try zio.checkCancel();
            try self.handler.execute(self.arena.allocator());
        }

        fn deinitFn(ptr: *anyopaque) void {
            const self: *Self = @ptrCast(@alignCast(ptr));
            const parent = self.parent;
            self.arena.deinit();
            parent.destroy(self);
        }
    };
}
